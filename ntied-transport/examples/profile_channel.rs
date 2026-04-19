// Standalone profiling harness for channel throughput.
// Sends data in small batches to stay within buffer limits, like the bench.
// Run under `samply record -- target/profiling/examples/profile_channel ...`
// to capture a CPU profile viewable in Firefox Profiler.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use ntied_transport::PrivateKey;
use ntied_transport::node_v2::{Connection, Node};

fn localhost() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

async fn connect_pair() -> (Connection, Connection, Arc<Node>, Arc<Node>) {
    let na = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let nb = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let addr_b = nb.local_addr().unwrap();
    let nb2 = nb.clone();
    let accept = tokio::spawn(async move { nb2.accept().await.unwrap() });
    let ca = na.connect(addr_b).await.unwrap();
    let cb = accept.await.unwrap();
    (ca, cb, na, nb)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let batch_size: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let msg_size: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4096);
    let iterations: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);

    eprintln!(
        "profile_channel: {} iterations × {} batch × {} bytes = {} MiB total",
        iterations,
        batch_size,
        msg_size,
        (iterations * batch_size * msg_size) / (1 << 20)
    );

    let (ca, cb, _na, _nb) = connect_pair().await;
    let ch_a = Arc::new(ca.open_channel().unwrap());
    ch_a.send(b"warmup".to_vec()).await.unwrap();
    let ch_b = Arc::new(cb.accept_channel().await.unwrap());
    ch_b.recv().await.unwrap();

    let start = Instant::now();
    let payload = vec![0xCDu8; msg_size];

    for _ in 0..iterations {
        let ch_a2 = ch_a.clone();
        let ch_b2 = ch_b.clone();
        let payload2 = payload.clone();
        let mc = batch_size;

        let recv = tokio::spawn(async move {
            for _ in 0..mc {
                ch_b2.recv().await.unwrap();
            }
        });

        for _ in 0..batch_size {
            ch_a2.send(payload2.clone()).await.unwrap();
        }
        recv.await.unwrap();
    }

    let elapsed = start.elapsed();
    let total_bytes = iterations * batch_size * msg_size;
    let mib_per_sec = (total_bytes as f64 / (1 << 20) as f64) / elapsed.as_secs_f64();

    eprintln!(
        "{} bytes in {:?} = {:.1} MiB/s",
        total_bytes, elapsed, mib_per_sec
    );
}
