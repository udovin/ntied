use std::collections::{HashMap, hash_map};
use std::sync::Arc;

use anyhow::{Context as _, anyhow};
use ntied_crypto::PublicKey;
use ntied_transport::Address;
use tokio::sync::Mutex as TokioMutex;
use tokio_sqlite::Value;

use crate::contact::ContactManager;
use crate::models::{ColumnIndex, Contact, DateTime};
use crate::packet::ContactProfile;
use crate::storage::Storage;

use super::{ChatHandle, ChatListener, StubListener};

pub struct ChatManager {
    storage: Arc<TokioMutex<Storage>>,
    contact_manager: Arc<ContactManager>,
    chats: Arc<TokioMutex<HashMap<Address, ChatHandle>>>,
    listener: Arc<dyn ChatListener>,
}

impl ChatManager {
    pub async fn new(
        storage: Arc<TokioMutex<Storage>>,
        contact_manager: Arc<ContactManager>,
    ) -> Result<Self, anyhow::Error> {
        Self::with_listener(storage, contact_manager, Arc::new(StubListener)).await
    }

    pub async fn with_listener<L>(
        storage: Arc<TokioMutex<Storage>>,
        contact_manager: Arc<ContactManager>,
        listener: Arc<L>,
    ) -> Result<Self, anyhow::Error>
    where
        L: ChatListener + 'static,
    {
        Self::create_tables(storage.as_ref()).await?;
        let contacts = Self::get_contacts(storage.as_ref()).await?;
        let mut chats = HashMap::new();
        for contact in contacts {
            let address = contact.address;
            let public_key = contact.public_key.clone();
            let profile = ContactProfile {
                name: contact.name.clone(),
            };
            let contact_handle = contact_manager
                .add_contact(address, public_key, profile)
                .await;
            let handle =
                ChatHandle::new(contact_handle, contact, storage.clone(), listener.clone());
            chats.insert(address, handle);
        }
        let chats = Arc::new(TokioMutex::new(chats));
        Ok(Self {
            storage,
            contact_manager,
            chats,
            listener,
        })
    }

    pub async fn list_contact_chats(&self) -> Vec<ChatHandle> {
        let mut result = Vec::new();
        let chats = self.chats.lock().await;
        for chat in chats.values() {
            result.push(chat.clone());
        }
        result
    }

    pub async fn add_contact_chat(
        &self,
        address: Address,
        public_key: PublicKey,
        name: String,
        local_name: Option<String>,
    ) -> Result<ChatHandle, anyhow::Error> {
        let contact_handle = self.contact_manager.connect_contact(address).await;
        let mut chats = self.chats.lock().await;
        match chats.entry(address) {
            hash_map::Entry::Occupied(entry) => {
                return Ok(entry.get().clone());
            }
            hash_map::Entry::Vacant(entry) => {
                let contact = Contact {
                    id: 0,
                    address,
                    public_key: public_key.clone(),
                    name,
                    local_name,
                    create_time: DateTime::now(),
                };
                let contact = self.create_contact(contact).await?;
                let handle = ChatHandle::new(
                    contact_handle,
                    contact,
                    self.storage.clone(),
                    self.listener.clone(),
                );
                entry.insert(handle.clone());
                return Ok(handle);
            }
        }
    }

    pub async fn get_contact_chat(&self, address: Address) -> Option<ChatHandle> {
        let chats = self.chats.lock().await;
        chats.get(&address).cloned()
    }

    pub async fn remove_contact_chat(&self, address: Address) -> Result<(), anyhow::Error> {
        let mut chats = self.chats.lock().await;
        if let hash_map::Entry::Occupied(entry) = chats.entry(address) {
            self.delete_contact(entry.get().contact().id).await?;
            entry.remove();
        }
        self.contact_manager.remove_contact(address).await;
        Ok(())
    }

    async fn get_contacts(storage: &TokioMutex<Storage>) -> Result<Vec<Contact>, anyhow::Error> {
        let columns = Contact::columns();
        let query = format!(
            "SELECT {} FROM \"contact\" ORDER BY \"id\"",
            Self::format_columns(columns)
        );
        let mut storage = storage.lock().await;
        let connection = storage.connection().await;
        let mut rows = connection.query(query, vec![]).await?;
        assert_eq!(columns.columns(), rows.columns());
        let mut result = Vec::new();
        while let Some(row) = rows.next().await {
            let row = row?;
            let values = row.into_values();
            let contact = Contact::from_values(values, columns)?;
            result.push(contact);
        }
        Ok(result)
    }

    async fn create_contact(&self, mut contact: Contact) -> Result<Contact, anyhow::Error> {
        let columns = Self::columns_without_id(Contact::columns(), "id");
        let values = contact.values(&columns);
        let query = format!(
            "INSERT INTO \"contact\" ({}) VALUES ({})",
            Self::format_columns(&columns),
            Self::format_values(&values),
        );
        let mut storage = self.storage.lock().await;
        let connection = storage.connection().await;
        let status = connection.execute(query, values).await?;
        contact.id = status
            .last_insert_id()
            .ok_or(anyhow!("Cannot retrieve contact id"))?;
        Ok(contact)
    }

    async fn delete_contact(&self, id: i64) -> Result<(), anyhow::Error> {
        let query = "DELETE FROM \"contact\" WHERE \"id\" = ?1";
        let mut storage = self.storage.lock().await;
        let connection = storage.connection().await;
        let status = connection.execute(query, vec![Value::Integer(id)]).await?;
        if status.rows_affected() != 1 {
            return Err(anyhow!("Cannot delete contact"));
        }
        Ok(())
    }

    async fn create_tables(storage: &TokioMutex<Storage>) -> Result<(), anyhow::Error> {
        let mut storage = storage.lock().await;
        let conn = storage.connection().await;

        conn.execute("PRAGMA foreign_keys = ON", Vec::<Value>::new())
            .await
            .context("Failed to enable foreign keys")?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS \"config\" (
                \"id\" INTEGER PRIMARY KEY AUTOINCREMENT,
                \"key\" TEXT NOT NULL UNIQUE,
                \"value\" TEXT NOT NULL
            )",
            Vec::new(),
        )
        .await
        .context("Failed to create config table")?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS \"contact\" (
                    \"id\" INTEGER PRIMARY KEY AUTOINCREMENT,
                    \"address\" TEXT NOT NULL UNIQUE,
                    \"public_key\" BLOB NOT NULL,
                    \"name\" TEXT NOT NULL,
                    \"local_name\" TEXT,
                    \"create_time\" BIGINT NOT NULL
                )",
            Vec::<Value>::new(),
        )
        .await
        .context("Failed to create contact table")?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS \"message\" (
                    \"id\" INTEGER PRIMARY KEY AUTOINCREMENT,
                    \"contact_id\" INTEGER NOT NULL,
                    \"message_id\" TEXT NOT NULL UNIQUE,
                    \"log_id\" INTEGER,
                    \"incoming\" INTEGER NOT NULL,
                    \"kind\" TEXT NOT NULL,
                    \"content\" TEXT NOT NULL,
                    \"create_time\" BIGINT NOT NULL,
                    \"receive_time\" BIGINT,
                    \"read_time\" BIGINT,
                    FOREIGN KEY (\"contact_id\") REFERENCES \"contact\" (\"id\") ON DELETE CASCADE
                )",
            Vec::<Value>::new(),
        )
        .await
        .context("Failed to create message table")?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS message__contact_id_log_id_idx
                 ON \"message\" (\"contact_id\", \"log_id\")",
            Vec::<Value>::new(),
        )
        .await
        .context("Failed to create message__contact_id_log_id_idx index")?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS message__contact_id_create_time_idx
                 ON \"message\" (\"contact_id\", \"create_time\")",
            Vec::<Value>::new(),
        )
        .await
        .context("Failed to create message__contact_id_create_time_idx index")?;

        Ok(())
    }

    fn columns_without_id(columns: &ColumnIndex, id_name: &str) -> ColumnIndex {
        let mut result = ColumnIndex::builder();
        for name in columns.columns() {
            if name != id_name {
                result.add(name);
            }
        }
        result.build()
    }

    fn format_columns(columns: &ColumnIndex) -> String {
        let mut result = String::new();
        for column in columns.columns() {
            if !result.is_empty() {
                result.push_str(", ");
            }
            result.push('"');
            result.push_str(&column);
            result.push('"');
        }
        result
    }

    fn format_values(values: &[Value]) -> String {
        let mut result = String::new();
        for i in 0..values.len() {
            if !result.is_empty() {
                result.push_str(", ");
            }
            result.push('?');
            result.push_str(&(i + 1).to_string());
        }
        result
    }
}
