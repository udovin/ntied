mod common;
use std::sync::Arc;
use std::time::{Duration, Instant};
use common::{init_tracing, localhost};
use ntied_transport::{Node, PrivateKey, RelayNode};

async fn measure(
    conn_a: &ntied_transport::Connection, conn_b: &ntied_transport::Connection,
    purpose: u16, total: usize, chunk_size: usize,
) -> (Duration, usize) {
    let sa = conn_a.open_stream(purpose).await.unwrap();
    let (sb, _) = conn_b.accept_stream().await.unwrap();
    let c = vec![0xABu8; chunk_size];
    let t = total;
    let c2 = c.clone();
    let send = tokio::spawn(async move { let mut s=0; while s<t { sa.send(&c2).await.unwrap(); s+=c2.len(); } });
    let recv = tokio::spawn(async move {
        let start = Instant::now(); let mut r=0usize;
        while r<t { let d=tokio::time::timeout(Duration::from_secs(60),sb.recv()).await.unwrap_or_else(|_|panic!("timeout at {r}/{t}")).unwrap(); r+=d.len(); }
        (start.elapsed(), r)
    });
    send.await.unwrap(); recv.await.unwrap()
}

async fn measure_latency(conn_a: &ntied_transport::Connection, conn_b: &ntied_transport::Connection, purpose: u16) -> Vec<Duration> {
    let sa = conn_a.open_stream(purpose).await.unwrap();
    let (sb, _) = conn_b.accept_stream().await.unwrap();
    let msg = vec![0xABu8; 1000];
    for _ in 0..20 { sa.send(&msg).await.unwrap(); let _ = sb.recv().await.unwrap(); }
    let mut times = Vec::with_capacity(200);
    for _ in 0..200 {
        let start = Instant::now();
        sa.send(&msg).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), sb.recv()).await.unwrap().unwrap();
        times.push(start.elapsed());
    }
    times.sort(); times
}

fn report(label: &str, times: &[Duration]) {
    let avg = times.iter().map(|t| t.as_micros()).sum::<u128>() / times.len() as u128;
    let p50 = times[times.len()/2].as_micros();
    let p95 = times[times.len()*95/100].as_micros();
    eprintln!("{label}: avg={avg}µs p50={p50}µs p95={p95}µs");
}

#[tokio::test(flavor = "multi_thread")]
async fn perf_direct() {
    init_tracing();
    let na = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let nb = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let n = nb.clone();
    let a = tokio::spawn(async move { n.accept().await.unwrap() });
    let ca = na.connect(nb.local_addr().unwrap()).await.unwrap();
    let cb = a.await.unwrap();
    eprintln!("\n=== Direct ===");
    for (i, &(kb,cs)) in [(64,1024),(256,2048),(1024,4096)].iter().enumerate() {
        let (e,r) = measure(&ca,&cb,(i+1) as u16,kb*1024,cs).await;
        eprintln!("  {kb:>6}KB: {:.1} MB/s ({e:.2?})", (r as f64/1048576.0)/e.as_secs_f64());
    }
    let t = measure_latency(&ca,&cb,100).await;
    report("  latency", &t);
}

#[tokio::test(flavor = "multi_thread")]
async fn perf_relay() {
    init_tracing();
    let relay = RelayNode::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let ra = relay.local_addr().unwrap();
    let _rt = tokio::spawn(async move { relay.run().await });
    let na = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    na.attach_relay(ra).await.unwrap();
    let ib = PrivateKey::generate(); let pb = ib.public_key().peer_id();
    let nb = Arc::new(Node::bind(localhost(), ib).await.unwrap());
    nb.attach_relay(ra).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let n = nb.clone();
    let a = tokio::spawn(async move { n.accept().await.unwrap() });
    let ca = na.connect_peer(&pb).await.unwrap();
    let cb = a.await.unwrap();
    eprintln!("\n=== Relay ===");
    for (i, &(kb,cs)) in [(64,1024),(256,2048)].iter().enumerate() {
        let (e,r) = measure(&ca,&cb,(i+1) as u16,kb*1024,cs).await;
        eprintln!("  {kb:>6}KB: {:.1} MB/s ({e:.2?})", (r as f64/1048576.0)/e.as_secs_f64());
    }
    let t = measure_latency(&ca,&cb,100).await;
    report("  latency", &t);
}
