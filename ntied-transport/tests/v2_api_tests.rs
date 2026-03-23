use std::net::SocketAddr;
use std::sync::Arc;

use ntied_transport::v2::api::Transport;
use ntied_transport::v2::crypto::PrivateKey;
use ntied_transport::v2::discovery::{Discovery, HashMapDiscovery};

const STACK_SIZE: usize = 16 * 1024 * 1024;

fn localhost() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

fn run_async<F, Fut>(f: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(STACK_SIZE)
                .enable_all()
                .build()
                .unwrap()
                .block_on(f());
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn bind_auto_registers() {
    run_async(|| async {
        let discovery = Arc::new(HashMapDiscovery::new());
        let identity = PrivateKey::generate();
        let peer_id = identity.public_key().peer_id();

        let transport = Transport::bind(localhost(), identity, &discovery)
            .await
            .unwrap();

        let local_addr = transport.local_addr().unwrap();
        assert_eq!(discovery.resolve(&peer_id).await, Some(local_addr));
    });
}

#[test]
fn connect_unknown_peer_fails() {
    run_async(|| async {
        let discovery = Arc::new(HashMapDiscovery::new());
        let identity = PrivateKey::generate();

        let transport = Transport::bind(localhost(), identity, &discovery)
            .await
            .unwrap();

        let unknown = PrivateKey::generate().public_key().peer_id();
        match transport.connect(&unknown).await {
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            Ok(_) => panic!("expected NotFound error for unknown peer"),
        }
    });
}

#[test]
fn two_transports_handshake() {
    run_async(|| async {
        let discovery = Arc::new(HashMapDiscovery::new());

        let id_a = PrivateKey::generate();
        let id_b = PrivateKey::generate();
        let peer_id_b = id_b.public_key().peer_id();

        let t_a = Transport::bind(localhost(), id_a, &discovery)
            .await
            .unwrap();
        let t_b = Transport::bind(localhost(), id_b, &discovery)
            .await
            .unwrap();

        let connect = tokio::spawn(async move { t_a.connect(&peer_id_b).await });
        let accept = tokio::spawn(async move { t_b.accept().await });

        let conn_a = connect.await.unwrap().unwrap();
        let conn_b = accept.await.unwrap().unwrap();

        assert!(conn_a.is_established().await);
        assert!(conn_b.is_established().await);
    });
}

#[test]
fn stream_over_discovery() {
    run_async(|| async {
        let discovery = Arc::new(HashMapDiscovery::new());

        let id_a = PrivateKey::generate();
        let id_b = PrivateKey::generate();
        let peer_id_b = id_b.public_key().peer_id();

        let t_a = Transport::bind(localhost(), id_a, &discovery)
            .await
            .unwrap();
        let t_b = Transport::bind(localhost(), id_b, &discovery)
            .await
            .unwrap();

        let connect = tokio::spawn(async move { t_a.connect(&peer_id_b).await.unwrap() });
        let accept = tokio::spawn(async move { t_b.accept().await.unwrap() });

        let conn_a = connect.await.unwrap();
        let conn_b = accept.await.unwrap();

        let stream_a = conn_a.open_stream(42).await.unwrap();
        stream_a.send(b"hello via discovery").await.unwrap();

        let (stream_b, purpose) = conn_b.accept_stream().await.unwrap();
        assert_eq!(purpose, 42);

        let data = stream_b.recv().await.unwrap();
        assert_eq!(data, b"hello via discovery");
    });
}

#[test]
fn bidirectional_streams_over_discovery() {
    run_async(|| async {
        let discovery = Arc::new(HashMapDiscovery::new());

        let id_a = PrivateKey::generate();
        let id_b = PrivateKey::generate();
        let peer_id_b = id_b.public_key().peer_id();

        let t_a = Transport::bind(localhost(), id_a, &discovery)
            .await
            .unwrap();
        let t_b = Transport::bind(localhost(), id_b, &discovery)
            .await
            .unwrap();

        let connect = tokio::spawn(async move { t_a.connect(&peer_id_b).await.unwrap() });
        let accept = tokio::spawn(async move { t_b.accept().await.unwrap() });

        let conn_a = connect.await.unwrap();
        let conn_b = accept.await.unwrap();

        let sa = conn_a.open_stream(1).await.unwrap();
        sa.send(b"ping").await.unwrap();

        let (sb, purpose) = conn_b.accept_stream().await.unwrap();
        assert_eq!(purpose, 1);
        let data = sb.recv().await.unwrap();
        assert_eq!(data, b"ping");

        let sb2 = conn_b.open_stream(2).await.unwrap();
        sb2.send(b"pong").await.unwrap();

        let (sa2, purpose) = conn_a.accept_stream().await.unwrap();
        assert_eq!(purpose, 2);

        let data = sa2.recv().await.unwrap();
        assert_eq!(data, b"pong");
    });
}

#[test]
fn multi_message_exchange() {
    run_async(|| async {
        let discovery = Arc::new(HashMapDiscovery::new());

        let id_a = PrivateKey::generate();
        let id_b = PrivateKey::generate();
        let peer_id_b = id_b.public_key().peer_id();

        let t_a = Transport::bind(localhost(), id_a, &discovery)
            .await
            .unwrap();
        let t_b = Transport::bind(localhost(), id_b, &discovery)
            .await
            .unwrap();

        let connect = tokio::spawn(async move { t_a.connect(&peer_id_b).await.unwrap() });
        let accept = tokio::spawn(async move { t_b.accept().await.unwrap() });

        let conn_a = Arc::new(connect.await.unwrap());
        let conn_b = Arc::new(accept.await.unwrap());

        let sa = conn_a.open_stream(10).await.unwrap();
        let (sb, purpose) = conn_b.accept_stream().await.unwrap();
        assert_eq!(purpose, 10);

        let mut expected = Vec::new();
        for i in 0..10u32 {
            let msg = format!("message-{i}");
            sa.send(msg.as_bytes()).await.unwrap();
            expected.extend_from_slice(msg.as_bytes());
        }

        let mut received = Vec::new();
        while received.len() < expected.len() {
            let chunk = sb.recv().await.unwrap();
            received.extend_from_slice(&chunk);
        }
        assert_eq!(received, expected);

        let sb_out = conn_b.open_stream(20).await.unwrap();
        let (sa_in, purpose) = conn_a.accept_stream().await.unwrap();
        assert_eq!(purpose, 20);

        let large = vec![0xCDu8; 4000];
        sb_out.send(&large).await.unwrap();

        let mut received = Vec::new();
        while received.len() < large.len() {
            let chunk = sa_in.recv().await.unwrap();
            received.extend_from_slice(&chunk);
        }
        assert_eq!(received.len(), 4000);
        assert!(received.iter().all(|&b| b == 0xCD));
    });
}
