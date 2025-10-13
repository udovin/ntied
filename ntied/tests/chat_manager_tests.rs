use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ntied::chat::{ChatListener, ChatManager};
use ntied::contact::{ContactManager, ContactStatus};
use ntied::models::{Message, MessageKind};
use ntied::packet::ContactProfile;
use ntied::storage::Storage;

use ntied_crypto::PrivateKey;
use ntied_server::Server;
use ntied_transport::{Address, ToAddress};

use tokio::sync::Mutex as TokioMutex;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_sqlite::Value;

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("ntied=trace,ntied_server=debug,ntied_transport=debug")
        .try_init();
}

async fn start_server() -> (SocketAddr, JoinHandle<()>) {
    let server = Server::new("127.0.0.1:0").await.unwrap();
    let server_addr = server.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = server.run().await;
    });
    (server_addr, handle)
}

async fn open_temp_storage() -> (tempfile::TempDir, Arc<TokioMutex<Storage>>) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let storage = Storage::create(dir.path(), "test-pass")
        .await
        .expect("failed to create storage");
    (dir, Arc::new(TokioMutex::new(storage)))
}

async fn scalar_i64(storage: &Arc<TokioMutex<Storage>>, query: &str, params: Vec<Value>) -> i64 {
    let mut guard = storage.lock().await;
    let conn = guard.connection().await;
    let row = conn
        .query_row(query, params)
        .await
        .expect("query_row failed");
    let row = row.expect("no row returned");
    match row.into_values().into_iter().next().expect("no column") {
        Value::Integer(i) => i,
        other => panic!("expected integer scalar, got {:?}", other),
    }
}

async fn table_exists(storage: &Arc<TokioMutex<Storage>>, name: &str) -> bool {
    let count = scalar_i64(
        storage,
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        vec![Value::Text(name.to_string())],
    )
    .await;
    count == 1
}

#[tokio::test]
async fn test_chat_manager_creates_tables_and_starts_empty() {
    init_tracing();
    let (server_addr, server_handle) = start_server().await;

    let (_dir, storage) = open_temp_storage().await;

    let key_a = PrivateKey::generate().unwrap();
    let mgr_a = Arc::new(
        ContactManager::new(
            server_addr,
            key_a,
            ContactProfile {
                name: "Alice".into(),
            },
        )
        .await,
    );

    // ChatManager::new should create the required tables
    let _chats = ChatManager::new(storage.clone(), mgr_a.clone())
        .await
        .expect("ChatManager::new failed");

    assert!(
        table_exists(&storage, "config").await,
        "config table missing"
    );
    assert!(
        table_exists(&storage, "contact").await,
        "contact table missing"
    );
    assert!(
        table_exists(&storage, "message").await,
        "message table missing"
    );

    // No chats should be present
    let random_addr: Address = PrivateKey::generate()
        .unwrap()
        .public_key()
        .to_address()
        .unwrap();

    let cm = ChatManager::new(storage.clone(), mgr_a.clone())
        .await
        .expect("ChatManager::new failed on reload");
    let none = cm.get_contact_chat(random_addr).await;
    assert!(
        none.is_none(),
        "expected no chat handle for unknown address"
    );

    server_handle.abort();
}

#[tokio::test]
async fn test_add_contact_chat_persists_and_reload() {
    init_tracing();
    let (server_addr, server_handle) = start_server().await;

    let (_dir, storage) = open_temp_storage().await;

    // Two identities
    let key_a = PrivateKey::generate().unwrap();
    let key_b = PrivateKey::generate().unwrap();
    let addr_b = key_b.public_key().to_address().unwrap();
    let pub_b = key_b.public_key().clone();

    // Manager A
    let mgr_a = Arc::new(
        ContactManager::new(
            server_addr,
            key_a,
            ContactProfile {
                name: "Alice".into(),
            },
        )
        .await,
    );

    // Give transports time to register
    sleep(Duration::from_millis(200)).await;

    // Chat manager A
    let chats_a = ChatManager::new(storage.clone(), mgr_a.clone())
        .await
        .expect("ChatManager::new failed");

    // Add contact chat and verify it is present in runtime cache
    let handle = chats_a
        .add_contact_chat(addr_b, pub_b.clone(), "Bob".into(), Some("Bobby".into()))
        .await
        .expect("add_contact_chat failed");
    assert_eq!(handle.address(), addr_b);

    let got = chats_a.get_contact_chat(addr_b).await;
    assert!(got.is_some(), "chat handle should be present after add");

    // Verify persisted in DB by counting rows with that address
    let count = scalar_i64(
        &storage,
        "SELECT COUNT(*) FROM \"contact\" WHERE \"address\" = ?1",
        vec![Value::Text(addr_b.to_string())],
    )
    .await;
    assert_eq!(count, 1, "contact row should be persisted");

    // Drop and reload ChatManager to verify it loads from storage
    drop(chats_a);
    let chats_reload = ChatManager::new(storage.clone(), mgr_a.clone())
        .await
        .expect("ChatManager reload failed");
    let got_after_reload = chats_reload.get_contact_chat(addr_b).await;
    assert!(
        got_after_reload.is_some(),
        "chat handle should be loaded from storage"
    );

    server_handle.abort();
}

#[tokio::test]
async fn test_remove_contact_chat_removes_from_db_and_cache() {
    init_tracing();
    let (server_addr, server_handle) = start_server().await;

    let (_dir, storage) = open_temp_storage().await;

    // Two identities
    let key_a = PrivateKey::generate().unwrap();
    let key_b = PrivateKey::generate().unwrap();
    let addr_b = key_b.public_key().to_address().unwrap();
    let pub_b = key_b.public_key().clone();

    // Manager A
    let mgr_a = Arc::new(
        ContactManager::new(
            server_addr,
            key_a,
            ContactProfile {
                name: "Alice".into(),
            },
        )
        .await,
    );

    let chats_a = ChatManager::new(storage.clone(), mgr_a.clone())
        .await
        .expect("ChatManager::new failed");

    // Add then remove
    chats_a
        .add_contact_chat(addr_b, pub_b, "Bob".into(), None)
        .await
        .expect("add_contact_chat failed");

    // Ensure it's in the cache before removal
    assert!(
        chats_a.get_contact_chat(addr_b).await.is_some(),
        "chat handle should exist before removal"
    );

    chats_a
        .remove_contact_chat(addr_b)
        .await
        .expect("remove_contact_chat failed");

    // Verify DB row is gone
    let count = scalar_i64(
        &storage,
        "SELECT COUNT(*) FROM \"contact\" WHERE \"address\" = ?1",
        vec![Value::Text(addr_b.to_string())],
    )
    .await;
    assert_eq!(count, 0, "contact row should be deleted");

    // And cache should no longer contain the handle
    let after_remove = chats_a.get_contact_chat(addr_b).await;
    assert!(
        after_remove.is_none(),
        "chat handle should be removed from cache"
    );

    server_handle.abort();
}

#[tokio::test]
async fn test_one_way_message_delivery() {
    init_tracing();
    let (server_addr, server_handle) = start_server().await;

    // Open storages
    let (_dir_a, storage_a) = open_temp_storage().await;
    let (_dir_b, storage_b) = open_temp_storage().await;

    // Two identities
    let key_a = PrivateKey::generate().unwrap();
    let key_b = PrivateKey::generate().unwrap();
    let addr_a = key_a.public_key().to_address().unwrap();
    let addr_b = key_b.public_key().to_address().unwrap();
    let pub_a = key_a.public_key().clone();
    let pub_b = key_b.public_key().clone();

    // Managers
    let mgr_a = Arc::new(
        ContactManager::new(
            server_addr,
            key_a,
            ContactProfile {
                name: "Alice".into(),
            },
        )
        .await,
    );
    let mgr_b = Arc::new(
        ContactManager::new(server_addr, key_b, ContactProfile { name: "Bob".into() }).await,
    );

    // Give transports time to register
    sleep(Duration::from_millis(300)).await;

    // 1) Perform explicit handshake via ContactManager (no ChatManager involved yet)
    let a_outgoing = mgr_a.connect_contact(addr_b).await;
    let incoming_addr = timeout(Duration::from_secs(10), mgr_b.on_incoming_address())
        .await
        .expect("timeout waiting for incoming at B")
        .expect("incoming channel closed");
    assert_eq!(incoming_addr, addr_a);
    let b_incoming = mgr_b.connect_contact(incoming_addr).await;
    b_incoming.accept().await.expect("B accept failed");

    // Wait until both Accepted at contact level
    let mut tries = 100;
    while tries > 0 {
        if a_outgoing.status() == ContactStatus::Accepted
            && b_incoming.status() == ContactStatus::Accepted
        {
            break;
        }
        sleep(Duration::from_millis(50)).await;
        tries -= 1;
    }
    assert!(tries > 0, "contacts did not reach Accepted");

    // 2) Create ChatManagers and chat handles after handshake
    let chats_a = ChatManager::new(storage_a.clone(), mgr_a.clone())
        .await
        .expect("ChatManager A init failed");
    let chats_b = ChatManager::new(storage_b.clone(), mgr_b.clone())
        .await
        .expect("ChatManager B init failed");

    let a_handle = chats_a
        .add_contact_chat(addr_b, pub_b, "Bob".into(), None)
        .await
        .expect("A add_contact_chat failed");
    let b_handle = chats_b
        .add_contact_chat(addr_a, pub_a, "Alice".into(), None)
        .await
        .expect("B add_contact_chat failed");

    // Send message from A to B
    a_handle
        .send_message(MessageKind::Text("hello-from-A".into()))
        .await
        .expect("send_message failed");

    // Expect message on B
    let msg = timeout(Duration::from_secs(5), b_handle.recv_message())
        .await
        .expect("timeout waiting for B to receive message")
        .expect("B recv_message failed");

    assert!(msg.incoming, "B should see incoming message");
    match msg.kind {
        MessageKind::Text(s) => assert_eq!(s, "hello-from-A"),
    }

    server_handle.abort();
}

#[derive(Clone)]
struct TestListener {
    tx: tokio::sync::mpsc::UnboundedSender<(bool, String)>,
}

#[async_trait::async_trait]
impl ChatListener for TestListener {
    async fn on_incoming_message(&self, _address: Address, message: Message) {
        let text = match message.kind {
            MessageKind::Text(s) => s,
        };
        let _ = self.tx.send((true, text));
    }

    async fn on_outgoing_message(&self, _address: Address, message: Message) {
        let text = match message.kind {
            MessageKind::Text(s) => s,
        };
        let _ = self.tx.send((false, text));
    }
}

#[tokio::test]
async fn test_chat_listener_emits_events() {
    init_tracing();
    let (server_addr, server_handle) = start_server().await;

    let (_dir_a, storage_a) = open_temp_storage().await;
    let (_dir_b, storage_b) = open_temp_storage().await;

    // Two identities
    let key_a = PrivateKey::generate().unwrap();
    let key_b = PrivateKey::generate().unwrap();
    let addr_a = key_a.public_key().to_address().unwrap();
    let addr_b = key_b.public_key().to_address().unwrap();
    let pub_a = key_a.public_key().clone();
    let pub_b = key_b.public_key().clone();

    // Managers
    let mgr_a = Arc::new(
        ContactManager::new(
            server_addr,
            key_a,
            ContactProfile {
                name: "Alice".into(),
            },
        )
        .await,
    );
    let mgr_b = Arc::new(
        ContactManager::new(server_addr, key_b, ContactProfile { name: "Bob".into() }).await,
    );

    // Give transports time to register
    sleep(Duration::from_millis(300)).await;

    // 1) Perform explicit handshake via ContactManager
    let a_outgoing = mgr_a.connect_contact(addr_b).await;
    let incoming_addr = timeout(Duration::from_secs(10), mgr_b.on_incoming_address())
        .await
        .expect("timeout waiting for incoming at B")
        .expect("incoming channel closed");
    assert_eq!(incoming_addr, addr_a);
    let b_incoming = mgr_b.connect_contact(incoming_addr).await;
    b_incoming.accept().await.expect("B accept failed");

    // Wait until both Accepted at contact level
    let mut tries = 100;
    while tries > 0 {
        if a_outgoing.status() == ContactStatus::Accepted
            && b_incoming.status() == ContactStatus::Accepted
        {
            break;
        }
        sleep(Duration::from_millis(50)).await;
        tries -= 1;
    }
    assert!(tries > 0, "contacts did not reach Accepted");

    // 2) Create ChatManagers with listeners
    let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel::<(bool, String)>();
    let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel::<(bool, String)>();
    let listener_a = Arc::new(TestListener { tx: tx_a });
    let listener_b = Arc::new(TestListener { tx: tx_b });

    let chats_a = ChatManager::with_listener(storage_a.clone(), mgr_a.clone(), listener_a)
        .await
        .expect("ChatManager A init failed");
    let chats_b = ChatManager::with_listener(storage_b.clone(), mgr_b.clone(), listener_b)
        .await
        .expect("ChatManager B init failed");

    let a_handle = chats_a
        .add_contact_chat(addr_b, pub_b, "Bob".into(), None)
        .await
        .expect("A add_contact_chat failed");
    let _b_handle = chats_b
        .add_contact_chat(addr_a, pub_a, "Alice".into(), None)
        .await
        .expect("B add_contact_chat failed");

    // Send message from A to B
    a_handle
        .send_message(MessageKind::Text("hello-listener".into()))
        .await
        .expect("send failed");

    // Expect listener event on B (incoming)
    let (incoming_b, text_b) = timeout(Duration::from_secs(5), rx_b.recv())
        .await
        .expect("timeout waiting for B listener")
        .expect("B listener closed");
    assert!(incoming_b, "B should see incoming event");
    assert_eq!(text_b, "hello-listener");

    // Expect listener event on A (outgoing confirmed)
    let (incoming_a, text_a) = timeout(Duration::from_secs(5), rx_a.recv())
        .await
        .expect("timeout waiting for A listener")
        .expect("A listener closed");
    assert!(!incoming_a, "A should see outgoing event");
    assert_eq!(text_a, "hello-listener");

    server_handle.abort();
}

#[tokio::test]
async fn test_bidirectional_message_delivery() {
    init_tracing();
    let (server_addr, server_handle) = start_server().await;

    // Open storages
    let (_dir_a, storage_a) = open_temp_storage().await;
    let (_dir_b, storage_b) = open_temp_storage().await;

    // Two identities
    let key_a = PrivateKey::generate().unwrap();
    let key_b = PrivateKey::generate().unwrap();
    let addr_a = key_a.public_key().to_address().unwrap();
    let addr_b = key_b.public_key().to_address().unwrap();
    let pub_a = key_a.public_key().clone();
    let pub_b = key_b.public_key().clone();

    // Managers
    let mgr_a = Arc::new(
        ContactManager::new(
            server_addr,
            key_a,
            ContactProfile {
                name: "Alice".into(),
            },
        )
        .await,
    );
    let mgr_b = Arc::new(
        ContactManager::new(server_addr, key_b, ContactProfile { name: "Bob".into() }).await,
    );

    // Give transports time to register
    sleep(Duration::from_millis(300)).await;

    // 1) Perform explicit handshake via ContactManager
    let a_outgoing = mgr_a.connect_contact(addr_b).await;
    let incoming_addr = timeout(Duration::from_secs(10), mgr_b.on_incoming_address())
        .await
        .expect("timeout waiting for incoming at B")
        .expect("incoming channel closed");
    assert_eq!(incoming_addr, addr_a);
    let b_incoming = mgr_b.connect_contact(incoming_addr).await;
    b_incoming.accept().await.expect("B accept failed");

    // Wait until both Accepted at contact level
    let mut tries = 100;
    while tries > 0 {
        if a_outgoing.status() == ContactStatus::Accepted
            && b_incoming.status() == ContactStatus::Accepted
        {
            break;
        }
        sleep(Duration::from_millis(50)).await;
        tries -= 1;
    }
    assert!(tries > 0, "contacts did not reach Accepted");

    // 2) Create ChatManagers and chat handles after handshake
    let chats_a = ChatManager::new(storage_a.clone(), mgr_a.clone())
        .await
        .expect("ChatManager A init failed");
    let chats_b = ChatManager::new(storage_b.clone(), mgr_b.clone())
        .await
        .expect("ChatManager B init failed");

    let a_handle = chats_a
        .add_contact_chat(addr_b, pub_b, "Bob".into(), None)
        .await
        .expect("A add_contact_chat failed");
    let b_handle = chats_b
        .add_contact_chat(addr_a, pub_a, "Alice".into(), None)
        .await
        .expect("B add_contact_chat failed");

    // Send A->B
    a_handle
        .send_message(MessageKind::Text("ping".into()))
        .await
        .expect("A->B send failed");
    let msg_b = timeout(Duration::from_secs(5), b_handle.recv_message())
        .await
        .expect("timeout waiting B recv")
        .expect("B recv failed");
    assert!(msg_b.incoming, "B should see incoming");
    match msg_b.kind {
        MessageKind::Text(s) => assert_eq!(s, "ping"),
    }

    // Send B->A
    b_handle
        .send_message(MessageKind::Text("pong".into()))
        .await
        .expect("B->A send failed");
    let msg_a = timeout(Duration::from_secs(5), a_handle.recv_message())
        .await
        .expect("timeout waiting A recv")
        .expect("A recv failed");
    assert!(msg_a.incoming, "A should see incoming");
    match msg_a.kind {
        MessageKind::Text(s) => assert_eq!(s, "pong"),
    }

    server_handle.abort();
}
