use std::net::SocketAddr;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use ntied_transport::PrivateKey;
use ntied_transport::node::{Connection, Node};

static TRACING_INIT: Once = Once::new();

fn init_tracing() {
    TRACING_INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_target(false)
            .with_test_writer()
            .init();
    });
}

fn localhost() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

async fn connect_pair() -> (Connection, Connection, Arc<Node>, Arc<Node>) {
    let na = Arc::new(
        Node::bind(localhost(), PrivateKey::generate())
            .await
            .unwrap(),
    );
    let nb = Arc::new(
        Node::bind(localhost(), PrivateKey::generate())
            .await
            .unwrap(),
    );
    let addr_b = nb.local_addr().unwrap();
    let nb2 = nb.clone();
    let accept = tokio::spawn(async move { nb2.accept().await.unwrap() });
    let ca = na.connect(addr_b).await.unwrap();
    let cb = accept.await.unwrap();
    (ca, cb, na, nb)
}

async fn measure_throughput(total: usize, chunk_size: usize) -> (Duration, usize) {
    let (ca, cb, _na, _nb) = connect_pair().await;

    let sa = ca.open_stream().unwrap();
    let chunk = vec![0xABu8; chunk_size];
    let t = total;

    let send = tokio::spawn(async move {
        let mut sent = 0;
        while sent < t {
            let remaining = t - sent;
            let to_send = &chunk[..remaining.min(chunk.len())];
            let w = sa.send(to_send).await.unwrap();
            sent += w;
        }
    });

    let sb = cb.accept_stream().await.unwrap();
    let recv = tokio::spawn(async move {
        let start = Instant::now();
        let mut received = 0usize;
        let mut buf = [0u8; 65536];
        while received < t {
            let (n, _) = tokio::time::timeout(Duration::from_secs(60), sb.recv(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("timeout at {received}/{t}"))
                .unwrap();
            received += n;
        }
        (start.elapsed(), received)
    });

    send.await.unwrap();
    recv.await.unwrap()
}

#[tokio::test]
async fn direct_throughput() {
    init_tracing();

    eprintln!("\n=== Direct Throughput ===");
    for &(kb, cs) in &[
        (32 * 1024, 100),
        (32 * 1024, 200),
        (32 * 1024, 400),
        (32 * 1024, 800),
        (32 * 1024, 1600),
        (32 * 1024, 4096),
    ] {
        let (elapsed, received) = measure_throughput(kb * 1024, cs).await;
        eprintln!(
            "  {kb:>6}KB: {:.1} MB/s ({elapsed:.2?})",
            (received as f64 / 1048576.0) / elapsed.as_secs_f64()
        );
    }
}
