use ntied_crypto::PrivateKey;
use ntied_server::Server;
use ntied_transport::{ToAddress, Transport};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::sleep;

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(format!(
            "{}=trace,ntied_transport=trace,ntied_server=debug",
            module_path!()
        ))
        .try_init();
}

#[tokio::test]
async fn test_transport_server_integration() {
    init_tracing();
    let (server_addr, server_task) = create_server().await;
    // Create a client transport
    let private_key = PrivateKey::generate().unwrap();
    let address = private_key.public_key().to_address().unwrap();
    tracing::info!(?address, "Creating transport");
    let transport = Transport::bind("127.0.0.1:0", address, private_key, server_addr)
        .await
        .unwrap();
    tracing::info!("Transport created successfully");
    // Verify transport is working
    assert_ne!(transport.local_addr().port(), 0);
    assert_eq!(transport.address(), address);
    tracing::info!("Test completed successfully");
    // Cleanup
    server_task.abort();
}

#[tokio::test]
async fn test_two_transports_connect() {
    init_tracing();
    let (server_addr, server_task) = create_server().await;
    // Create first transport
    let private_key1 = PrivateKey::generate().unwrap();
    let address1 = private_key1.public_key().to_address().unwrap();
    tracing::info!(?address1, "Creating transport 1");
    let transport1 = Transport::bind("127.0.0.1:0", address1, private_key1, server_addr)
        .await
        .unwrap();
    // Create second transport
    let private_key2 = PrivateKey::generate().unwrap();
    let address2 = private_key2.public_key().to_address().unwrap();
    tracing::info!(?address2, "Creating transport 2");
    let transport2 = Transport::bind("127.0.0.1:0", address2, private_key2, server_addr)
        .await
        .unwrap();
    tracing::info!("Both transports created successfully");
    let connect_task = tokio::spawn(async move { transport1.connect(address2).await.unwrap() });
    let accept_task = tokio::spawn(async move { transport2.accept().await.unwrap() });
    // Transport 1 connects to Transport 2
    tracing::info!("Transport 1 connecting to Transport 2");
    let _connection1_to_2 = connect_task.await.unwrap();
    // Transport 2 accepts the incoming connection
    tracing::info!("Transport 2 accepting connection");
    let _connection2_from_1 = accept_task.await.unwrap();
    tracing::info!("Connection established successfully");
    // Cleanup
    server_task.abort();
}

#[tokio::test]
async fn test_connect_to_nonexistent_peer() {
    init_tracing();
    let (server_addr, server_task) = create_server().await;
    // Create a transport
    let private_key = PrivateKey::generate().unwrap();
    let address = private_key.public_key().to_address().unwrap();
    tracing::info!(?address, "Creating transport");
    let transport = Transport::bind("127.0.0.1:0", address, private_key, server_addr)
        .await
        .unwrap();
    // Generate a non-existent address
    let nonexistent_private_key = PrivateKey::generate().unwrap();
    let nonexistent_address = nonexistent_private_key.public_key().to_address().unwrap();
    tracing::info!(
        ?nonexistent_address,
        "Attempting to connect to non-existent peer"
    );
    // Try to connect to non-existent peer - should fail
    assert!(
        transport.connect(nonexistent_address).await.is_err(),
        "Connection to non-existent peer should fail"
    );
    tracing::info!(
        "Test completed successfully - connection to non-existent peer properly rejected"
    );
    // Cleanup
    server_task.abort();
}

#[tokio::test]
async fn test_long_connection() {
    init_tracing();
    let (server_addr, server_task) = create_server().await;
    // Peer 1.
    let private_key1 = PrivateKey::generate().unwrap();
    let address1 = private_key1.public_key().to_address().unwrap();
    let transport1 = Transport::bind("127.0.0.1:0", address1, private_key1, server_addr)
        .await
        .unwrap();
    // Peer 2.
    let private_key2 = PrivateKey::generate().unwrap();
    let address2 = private_key2.public_key().to_address().unwrap();
    let transport2 = Transport::bind("127.0.0.1:0", address2, private_key2, server_addr)
        .await
        .unwrap();
    let connect_task = tokio::spawn(async move { transport1.connect(address2).await.unwrap() });
    let accept_task = tokio::spawn(async move { transport2.accept().await.unwrap() });
    // Connection 1.
    let connection1 = Arc::new(connect_task.await.unwrap());
    let task1_send = tokio::spawn({
        let connection1 = connection1.clone();
        async move {
            for i in 0..6 {
                connection1.send(format!("1:{}", i)).await.unwrap();
                sleep(Duration::from_secs(5)).await;
            }
            connection1.send("close").await.unwrap();
        }
    });
    let task1_recv = tokio::spawn(async move {
        let mut messages = Vec::new();
        while let Ok(message) = connection1.recv().await {
            let message: String = message.try_into().unwrap();
            tracing::info!("Peer 1 recv message: {}", message);
            if message == "close" {
                break;
            }
            messages.push(message);
        }
        messages
    });
    // Connection 2.
    let connection2 = Arc::new(accept_task.await.unwrap());
    let task2_send = tokio::spawn({
        let connection2 = connection2.clone();
        async move {
            for i in 0..6 {
                connection2.send(format!("2:{}", i)).await.unwrap();
                sleep(Duration::from_secs(5)).await;
            }
            connection2.send("close").await.unwrap();
        }
    });
    let task2_recv = tokio::spawn(async move {
        let mut messages = Vec::new();
        while let Ok(message) = connection2.recv().await {
            let message: String = message.try_into().unwrap();
            tracing::info!("Peer 2 recv message: {}", message);
            if message == "close" {
                break;
            }
            messages.push(message);
        }
        messages
    });
    // Joining connections.
    task1_send.await.unwrap();
    task2_send.await.unwrap();
    let messages1 = task1_recv.await.unwrap();
    let messages2 = task2_recv.await.unwrap();
    assert_eq!(messages1.len(), 6);
    assert_eq!(messages2.len(), 6);
    // Cleanup
    server_task.abort();
}

async fn create_server() -> (
    SocketAddr,
    JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>,
) {
    let server = Server::new("127.0.0.1:0").await.unwrap();
    let server_addr = server.local_addr().unwrap();
    tracing::info!(?server_addr, "Server started");
    let server_task = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(100)).await;
    (server_addr, server_task)
}
