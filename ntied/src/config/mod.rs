use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::anyhow;
use ntied_crypto::PrivateKey;
use tokio::sync::Mutex as TokioMutex;
use tokio_sqlite::Value;

use crate::packet::ContactProfile;
use crate::storage::Storage;

/// Simple configuration manager backed by the `"config"` table.
/// Keys used:
/// - `"private_key_pem"`: String (PEM-encoded private key)
/// - `"profile"`: JSON-encoded `ContactProfile`
/// - `"server_addr"`: String (SocketAddr as "ip:port")
pub struct ConfigManager {
    storage: Arc<TokioMutex<Storage>>,
}

impl ConfigManager {
    /// Create a new ConfigManager. Does not perform I/O.
    pub fn new(storage: Arc<TokioMutex<Storage>>) -> Self {
        Self { storage }
    }

    /// Initialize account with a freshly generated private key and provided profile name.
    /// Persists `"private_key_pem"` and `"profile"` in the config table.
    /// Returns the profile and the private key.
    pub async fn init_account(
        &self,
        name: String,
    ) -> Result<(ContactProfile, PrivateKey), anyhow::Error> {
        self.ensure_tables().await?;
        // Detect if an account was already initialized
        if self.has_config_key("private_key_pem").await? || self.has_config_key("profile").await? {
            return Err(anyhow!("Account already initialized"));
        }
        // Generate private key and persist PEM
        let private_key =
            PrivateKey::generate().map_err(|e| anyhow!("Failed to generate private key: {}", e))?;
        let pem = private_key
            .to_pem()
            .map_err(|e| anyhow!("Failed to serialize private key to PEM: {}", e))?;
        self.upsert_config("private_key_pem", pem).await?;
        // Persist profile
        let profile = ContactProfile { name };
        let profile_json = serde_json::to_string(&profile)
            .map_err(|e| anyhow!("Failed to serialize profile: {}", e))?;
        self.upsert_config("profile", profile_json).await?;
        Ok((profile, private_key))
    }

    /// Load previously persisted profile.
    pub async fn get_profile(&self) -> Result<ContactProfile, anyhow::Error> {
        self.ensure_tables().await?;
        let raw = self
            .get_config("profile")
            .await?
            .ok_or(anyhow!("Profile not set"))?;
        let profile: ContactProfile =
            serde_json::from_str(&raw).map_err(|e| anyhow!("Failed to parse profile: {}", e))?;
        Ok(profile)
    }

    /// Load the persisted private key and return its corresponding public key.
    /// Note: The interface asks for PublicKey, not PrivateKey.
    pub async fn get_private_key(&self) -> Result<PrivateKey, anyhow::Error> {
        self.ensure_tables().await?;
        let pem = self
            .get_config("private_key_pem")
            .await?
            .ok_or(anyhow!("Private key not set"))?;
        let private_key = PrivateKey::from_pem(&pem)
            .map_err(|e| anyhow!("Failed to parse private key from PEM: {}", e))?;
        Ok(private_key)
    }

    /// Read the server address from config.
    pub async fn get_server_addr(&self) -> Result<SocketAddr, anyhow::Error> {
        self.ensure_tables().await?;
        let raw = self
            .get_config("server_addr")
            .await?
            .ok_or(anyhow!("Server address not set"))?;
        let addr = SocketAddr::from_str(&raw)
            .map_err(|e| anyhow!("Failed to parse server address '{}': {}", raw, e))?;
        Ok(addr)
    }

    /// Persist the server address in config.
    pub async fn set_server_addr(&self, server_addr: SocketAddr) -> Result<(), anyhow::Error> {
        self.ensure_tables().await?;
        self.upsert_config("server_addr", server_addr.to_string())
            .await
    }

    async fn ensure_tables(&self) -> Result<(), anyhow::Error> {
        let mut storage = self.storage.lock().await;
        let conn = storage.connection().await;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS \"config\" (
                \"id\" INTEGER PRIMARY KEY AUTOINCREMENT,
                \"key\" TEXT NOT NULL UNIQUE,
                \"value\" TEXT NOT NULL
            )",
            Vec::<Value>::new(),
        )
        .await
        .map_err(|e| anyhow!("Failed to create config table: {}", e))?;
        Ok(())
    }

    async fn has_config_key(&self, key: &str) -> Result<bool, anyhow::Error> {
        let mut storage = self.storage.lock().await;
        let conn = storage.connection().await;
        let row = conn
            .query_row(
                "SELECT 1 FROM \"config\" WHERE \"key\" = ?1 LIMIT 1",
                vec![Value::Text(key.to_string())],
            )
            .await
            .map_err(|e| anyhow!("Failed to query config: {}", e))?;
        Ok(row.is_some())
    }

    async fn get_config(&self, key: &str) -> Result<Option<String>, anyhow::Error> {
        let mut storage = self.storage.lock().await;
        let conn = storage.connection().await;
        let row = conn
            .query_row(
                "SELECT \"value\" FROM \"config\" WHERE \"key\" = ?1 LIMIT 1",
                vec![Value::Text(key.to_string())],
            )
            .await
            .map_err(|e| anyhow!("Failed to query config '{}': {}", key, e))?;
        match row {
            Some(row) => {
                let mut values = row.into_values();
                match values.pop() {
                    Some(Value::Text(s)) => Ok(Some(s)),
                    Some(other) => Err(anyhow!("Unexpected value type: {:?}", other)),
                    None => Ok(None),
                }
            }
            None => Ok(None),
        }
    }

    async fn upsert_config(&self, key: &str, value: String) -> Result<(), anyhow::Error> {
        let mut storage = self.storage.lock().await;
        let conn = storage.connection().await;
        // Try UPDATE first
        let update_status = conn
            .execute(
                "UPDATE \"config\" SET \"value\" = ?1 WHERE \"key\" = ?2",
                vec![Value::Text(value.clone()), Value::Text(key.to_string())],
            )
            .await
            .map_err(|e| anyhow!("Failed to update config '{}': {}", key, e))?;
        if update_status.rows_affected() == 0 {
            // No row updated; perform INSERT
            conn.execute(
                "INSERT INTO \"config\" (\"key\", \"value\") VALUES (?1, ?2)",
                vec![Value::Text(key.to_string()), Value::Text(value)],
            )
            .await
            .map_err(|e| anyhow!("Failed to insert config '{}': {}", key, e))?;
        }
        Ok(())
    }
}
