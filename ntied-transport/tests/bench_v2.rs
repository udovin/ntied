use std::net::SocketAddr;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use ntied_transport::PrivateKey;
use ntied_transport::node_v2::Node;

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

async fn measure_throughput(
    conn_a: &ntied_transport::node_v2::Connection,
    conn_b: &ntied_transport::node_v2::Connection,
    purpose: u16,
    total: usize,
    chunk_size: usize,
) -> (Duration, usize) {
    let sa = conn_a.open_stream(purpose);
    let sb = conn_b.accept_stream().await.unwrap();
    let chunk = vec![0xABu8; chunk_size];
    let t = total;

    let send = tokio::spawn(async move {
        let mut sent = 0;
        while sent < t {
            sa.send(&chunk).await.unwrap();
            sent += chunk.len();
        }
    });

    let recv = tokio::spawn(async move {
        let start = Instant::now();
        let mut received = 0usize;
        while received < t {
            let data = tokio::time::timeout(Duration::from_secs(60), sb.recv())
                .await
                .unwrap_or_else(|_| panic!("timeout at {received}/{t}"))
                .unwrap();
            received += data.len();
        }
        (start.elapsed(), received)
    });

    send.await.unwrap();
    recv.await.unwrap()
}

async fn measure_latency(
    conn_a: &ntied_transport::node_v2::Connection,
    conn_b: &ntied_transport::node_v2::Connection,
    purpose: u16,
) -> Vec<Duration> {
    let sa = conn_a.open_stream(purpose);
    let sb = conn_b.accept_stream().await.unwrap();
    let msg = vec![0xABu8; 1000];

    // Warmup
    for _ in 0..20 {
        sa.send(&msg).await.unwrap();
        let _ = sb.recv().await.unwrap();
    }

    let mut times = Vec::with_capacity(200);
    for _ in 0..200 {
        let start = Instant::now();
        sa.send(&msg).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), sb.recv())
            .await
            .unwrap()
            .unwrap();
        times.push(start.elapsed());
    }
    times.sort();
    times
}

fn report(label: &str, times: &[Duration]) {
    let avg = times.iter().map(|t| t.as_micros()).sum::<u128>() / times.len() as u128;
    let p50 = times[times.len() / 2].as_micros();
    let p95 = times[times.len() * 95 / 100].as_micros();
    eprintln!("{label}: avg={avg}µs p50={p50}µs p95={p95}µs");
}

#[tokio::test(flavor = "multi_thread")]
async fn perf_v2_direct() {
    init_tracing();

    let na = Node::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    let nb = Arc::new(
        Node::bind(localhost(), PrivateKey::generate())
            .await
            .unwrap(),
    );
    let n = nb.clone();
    let a = tokio::spawn(async move { n.accept().await.unwrap() });
    let ca = na.connect(nb.local_addr().unwrap()).await.unwrap();
    let cb = a.await.unwrap();

    eprintln!("\n=== node_v2 Direct Throughput ===");
    for (i, &(kb, cs)) in [
        (64, 1024),
        (256, 2048),
        (1024, 4096),
        (10240, 4096),
        // (128 * 1024, 1024),
    ]
    .iter()
    .enumerate()
    {
        let (elapsed, received) = measure_throughput(&ca, &cb, (i + 1) as u16, kb * 1024, cs).await;
        eprintln!(
            "  {kb:>6}KB: {:.1} MB/s ({elapsed:.2?})",
            (received as f64 / 1048576.0) / elapsed.as_secs_f64()
        );
    }

    let t = measure_latency(&ca, &cb, 100).await;
    report("  latency", &t);
}
