use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ntied::contact::{ContactManager, ContactStatus};
use ntied::packet::ContactProfile;
use ntied_crypto::PrivateKey;
use ntied_server::Server;
use ntied_transport::ToAddress;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

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

async fn wait_until<F>(mut f: F, tries: usize, delay: Duration) -> bool
where
    F: FnMut() -> bool,
{
    for _ in 0..tries {
        if f() {
            return true;
        }
        sleep(delay).await;
    }
    false
}

#[tokio::test]
async fn test_accept_contact() {
    init_tracing();
    let (server_addr, server_handle) = start_server().await;
    // Create two managers (Alice and Bob)
    let alice_key = PrivateKey::generate().unwrap();
    let alice_addr = alice_key.public_key().to_address().unwrap();
    let bob_key = PrivateKey::generate().unwrap();
    let bob_addr = bob_key.public_key().to_address().unwrap();
    let alice = ContactManager::new(
        server_addr,
        alice_key,
        ContactProfile {
            name: "Alice".to_string(),
        },
    )
    .await;
    let bob = ContactManager::new(
        server_addr,
        bob_key,
        ContactProfile {
            name: "Bob".to_string(),
        },
    )
    .await;
    // Allow transports to bind/register
    sleep(Duration::from_millis(400)).await;
    // Alice initiates outgoing contact
    let alice_to_bob = alice.connect_contact(bob_addr).await;
    // Bob waits for incoming connection/handle
    let bob_incoming_address = timeout(Duration::from_secs(5), bob.on_incoming_address())
        .await
        .expect("Timed out waiting for Bob to receive incoming contact")
        .expect("Bob failed to receive incoming contact");
    assert_eq!(bob_incoming_address, alice_addr);
    let bob_incoming = bob.connect_contact(bob_incoming_address).await;
    assert_eq!(bob_incoming.status(), ContactStatus::PendingIncoming);
    // Wait until Bob sees Alice's profile (after receiving Contact::Request)
    let profile_seen = wait_until(
        || bob_incoming.profile().is_some(),
        20,
        Duration::from_millis(100),
    )
    .await;
    assert!(
        profile_seen,
        "Bob did not receive Alice's profile (request)"
    );
    // Bob accepts the contact
    bob_incoming
        .accept()
        .await
        .expect("Bob failed to accept contact");
    // Alice should transition to Accepted
    let alice_accepted = wait_until(
        || alice_to_bob.status() == ContactStatus::Accepted,
        50,
        Duration::from_millis(100),
    )
    .await;
    assert!(alice_accepted, "Alice did not reach Accepted status");
    // Bob should transition to Accepted after accept()
    let bob_accepted = wait_until(
        || bob_incoming.status() == ContactStatus::Accepted,
        50,
        Duration::from_millis(100),
    )
    .await;
    assert!(bob_accepted, "Bob did not reach Accepted status");
    // Optionally verify connection flag stabilizes
    let connected_ok = wait_until(
        || alice_to_bob.is_connected() || bob_incoming.is_connected(),
        50,
        Duration::from_millis(100),
    )
    .await;
    assert!(
        connected_ok,
        "Connection was not established for either side"
    );
    server_handle.abort();
}

#[tokio::test]
async fn test_reject_contact() {
    init_tracing();
    let (server_addr, server_handle) = start_server().await;
    // Create two managers
    let a_key = PrivateKey::generate().unwrap();
    let a_addr = a_key.public_key().to_address().unwrap();
    let b_key = PrivateKey::generate().unwrap();
    let b_addr = b_key.public_key().to_address().unwrap();
    let a_mgr = ContactManager::new(
        server_addr,
        a_key,
        ContactProfile {
            name: "A".to_string(),
        },
    )
    .await;
    let b_mgr = ContactManager::new(
        server_addr,
        b_key,
        ContactProfile {
            name: "B".to_string(),
        },
    )
    .await;
    sleep(Duration::from_millis(400)).await;
    // A initiates outgoing
    let a_to_b = a_mgr.connect_contact(b_addr).await;
    // B receives incoming and rejects
    let b_incoming_address = timeout(Duration::from_secs(5), b_mgr.on_incoming_address())
        .await
        .expect("Timed out waiting for incoming")
        .expect("Incoming channel closed unexpectedly");
    assert_eq!(b_incoming_address, a_addr);
    let b_incoming = b_mgr.connect_contact(b_incoming_address).await;
    // Ensure B has seen the request and profile (optional)
    let profile_seen = wait_until(
        || b_incoming.profile().is_some(),
        20,
        Duration::from_millis(100),
    )
    .await;
    assert!(profile_seen, "B did not observe incoming profile");
    b_incoming
        .reject()
        .await
        .expect("B failed to reject incoming contact");
    // A should switch to RejectedOutgoing
    let a_rejected = wait_until(
        || a_to_b.status() == ContactStatus::RejectedOutgoing,
        50,
        Duration::from_millis(100),
    )
    .await;
    assert!(a_rejected, "A did not reach RejectedOutgoing status");
    // B should switch to RejectedIncoming
    let b_rejected = wait_until(
        || b_incoming.status() == ContactStatus::RejectedIncoming,
        50,
        Duration::from_millis(100),
    )
    .await;
    assert!(b_rejected, "B did not reach RejectedIncoming status");
    server_handle.abort();
}

#[tokio::test]
async fn test_simultaneous_connect_and_accept_paths() {
    init_tracing();
    let (server_addr, server_handle) = start_server().await;
    // Create two managers
    let left_key = PrivateKey::generate().unwrap();
    let left_addr = left_key.public_key().to_address().unwrap();
    let right_key = PrivateKey::generate().unwrap();
    let right_addr = right_key.public_key().to_address().unwrap();
    let left = ContactManager::new(
        server_addr,
        left_key,
        ContactProfile {
            name: "Left".to_string(),
        },
    )
    .await;
    let right = Arc::new(
        ContactManager::new(
            server_addr,
            right_key,
            ContactProfile {
                name: "Right".to_string(),
            },
        )
        .await,
    );
    sleep(Duration::from_millis(300)).await;
    // Left initiates outgoing; Right concurrently waits for incoming handle.
    // This exercises connect() path on Left handle and accept() path on Right.
    let left_handle = left.connect_contact(right_addr).await;
    let right_incoming_task = tokio::spawn({
        let right = right.clone();
        async move {
            timeout(Duration::from_secs(5), right.on_incoming_address())
                .await
                .expect("Timeout waiting for right incoming")
                .expect("Right incoming channel closed")
        }
    });
    let right_incoming_address = right_incoming_task.await.unwrap();
    assert_eq!(right_incoming_address, left_addr);
    let right_incoming = right.connect_contact(right_incoming_address).await;
    // Accept on right, completing the handshake
    right_incoming
        .accept()
        .await
        .expect("Right failed to accept");
    // Both should reach Accepted
    let left_ok = wait_until(
        || left_handle.status() == ContactStatus::Accepted,
        50,
        Duration::from_millis(100),
    )
    .await;
    assert!(left_ok, "Left did not reach Accepted");
    let right_ok = wait_until(
        || right_incoming.status() == ContactStatus::Accepted,
        50,
        Duration::from_millis(100),
    )
    .await;
    assert!(right_ok, "Right did not reach Accepted");
    server_handle.abort();
}

#[tokio::test]
async fn test_preknown_contact_auto_handshake() {
    init_tracing();
    let (server_addr, server_handle) = start_server().await;
    // Create identities
    let a_key = PrivateKey::generate().unwrap();
    let a_addr = a_key.public_key().to_address().unwrap();
    let a_pub = a_key.public_key().clone();
    let b_key = PrivateKey::generate().unwrap();
    let b_addr = b_key.public_key().to_address().unwrap();
    // Start managers
    let a_mgr = ContactManager::new(
        server_addr,
        a_key,
        ContactProfile {
            name: "Alice".to_string(),
        },
    )
    .await;
    let b_mgr = ContactManager::new(
        server_addr,
        b_key,
        ContactProfile {
            name: "Bob".to_string(),
        },
    )
    .await;
    sleep(Duration::from_millis(300)).await;
    // Scenario:
    // B already knows A (pre-added/accepted), while A initiates contact as outgoing.
    // B's handle is created in Accepted state and should auto-establish a connection,
    // reply to A's Request with Accept, and both converge to Accepted.
    let b_known_profile = ContactProfile {
        name: "AliceKnown".to_string(),
    };
    let b_known_handle = b_mgr
        .add_contact(a_addr, a_pub, b_known_profile.clone())
        .await;
    assert_eq!(
        b_known_handle.status(),
        ContactStatus::Accepted,
        "Pre-known contact on B must be Accepted immediately"
    );
    let a_outgoing = a_mgr.connect_contact(b_addr).await;
    // A should connect and send Request; B (already Accepted) should respond with Accept.
    let a_ok = wait_until(
        || a_outgoing.status() == ContactStatus::Accepted,
        50,
        Duration::from_secs(10),
    )
    .await;
    assert!(a_ok, "A did not reach Accepted status");
    // B remains Accepted; optionally verify connection establishment
    let connected = wait_until(
        || a_outgoing.is_connected() || b_known_handle.is_connected(),
        50,
        Duration::from_secs(10),
    )
    .await;
    assert!(connected, "Expected at least one side to report connected");
    // Optionally ensure B's handle knows A's public key after connection
    let pk_known = wait_until(
        || b_known_handle.public_key().is_some(),
        50,
        Duration::from_secs(10),
    )
    .await;
    assert!(pk_known, "B's known handle did not record A's public key");
    server_handle.abort();
}

#[tokio::test]
async fn test_both_outgoing_remain_pending() {
    init_tracing();
    let (server_addr, server_handle) = start_server().await;
    // Create two managers
    let a_key = PrivateKey::generate().unwrap();
    let a_addr = a_key.public_key().to_address().unwrap();
    let b_key = PrivateKey::generate().unwrap();
    let b_addr = b_key.public_key().to_address().unwrap();
    let a_mgr = ContactManager::new(
        server_addr,
        a_key,
        ContactProfile {
            name: "A".to_string(),
        },
    )
    .await;
    let b_mgr = ContactManager::new(
        server_addr,
        b_key,
        ContactProfile {
            name: "B".to_string(),
        },
    )
    .await;
    // Give transports time to register
    sleep(Duration::from_millis(400)).await;
    // Both sides initiate outgoing concurrently (no one uses accept_contact).
    let a_outgoing = a_mgr.connect_contact(b_addr).await;
    let b_outgoing = b_mgr.connect_contact(a_addr).await;
    // Wait some time for connection attempts and request exchange
    sleep(Duration::from_secs(2)).await;
    // Expect both sides to remain PendingOutgoing since neither side accepts.
    assert_eq!(a_outgoing.status(), ContactStatus::PendingOutgoing);
    assert_eq!(b_outgoing.status(), ContactStatus::PendingOutgoing);
    // Additionally ensure neither side moved to Accepted implicitly.
    assert!(!matches!(a_outgoing.status(), ContactStatus::Accepted));
    assert!(!matches!(b_outgoing.status(), ContactStatus::Accepted));
    server_handle.abort();
}
