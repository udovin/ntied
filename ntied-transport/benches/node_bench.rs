use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use ntied_transport::PrivateKey;
use ntied_transport::node_v2::{Connection, Node};

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

// ---------------------------------------------------------------------------
// Stream throughput
// ---------------------------------------------------------------------------

async fn stream_throughput(total: usize, chunk_size: usize) -> usize {
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
        let mut received = 0usize;
        let mut buf = [0u8; 65536];
        while received < t {
            let (n, _) = tokio::time::timeout(Duration::from_secs(30), sb.recv(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("timeout at {received}/{t}"))
                .unwrap();
            received += n;
        }
        received
    });

    send.await.unwrap();
    recv.await.unwrap()
}

fn bench_stream_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("stream_throughput");

    for &chunk_size in &[1024, 4096] {
        let total = 256 * 1024; // 256 KB
        group.throughput(Throughput::Bytes(total as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("chunk_{chunk_size}")),
            &chunk_size,
            |b, &cs| {
                b.iter(|| rt.block_on(stream_throughput(total, cs)));
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Stream latency (small messages)
// ---------------------------------------------------------------------------

fn bench_stream_latency(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    // Setup once: create connection pair + streams.
    let (sa, sb, _ca, _cb, _na, _nb) = rt.block_on(async {
        let (ca, cb, na, nb) = connect_pair().await;
        let sa = ca.open_stream().unwrap();
        sa.send(b"warmup").await.unwrap();
        let sb = cb.accept_stream().await.unwrap();
        let mut buf = [0u8; 64];
        sb.recv(&mut buf).await.unwrap();
        (sa, sb, ca, cb, na, nb)
    });

    let sa = Arc::new(sa);
    let sb = Arc::new(sb);

    c.bench_function("stream_latency_oneway", |b| {
        b.iter(|| {
            let sa = sa.clone();
            let sb = sb.clone();
            rt.block_on(async move {
                sa.send(b"ping").await.unwrap();
                let mut buf = [0u8; 64];
                sb.recv(&mut buf).await.unwrap();
            });
        });
    });
}

// ---------------------------------------------------------------------------
// Channel throughput
// ---------------------------------------------------------------------------

fn bench_channel_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("channel_throughput");

    for &(msg_size, msg_count) in &[(64, 200), (512, 50), (4096, 10)] {
        // Setup once per parameter.
        let (ch_a, ch_b, _ca, _cb, _na, _nb) = rt.block_on(async {
            let (ca, cb, na, nb) = connect_pair().await;
            let ch_a = ca.open_channel().unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            ch_a.send(b"warmup".to_vec(), deadline).await.unwrap();
            let ch_b = cb.accept_channel().await.unwrap();
            ch_b.recv().await.unwrap();
            (ch_a, ch_b, ca, cb, na, nb)
        });

        let ch_a = Arc::new(ch_a);
        let ch_b = Arc::new(ch_b);

        group.throughput(Throughput::Bytes((msg_count * msg_size) as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("msg_{msg_size}")),
            &msg_size,
            |b, &ms| {
                let ch_a = ch_a.clone();
                let ch_b = ch_b.clone();
                b.iter(|| {
                    let ch_a = ch_a.clone();
                    let ch_b = ch_b.clone();
                    rt.block_on(async move {
                        let data = vec![0xCDu8; ms];
                        let deadline = Instant::now() + Duration::from_secs(30);
                        let mc = msg_count;

                        let recv = tokio::spawn(async move {
                            for _ in 0..mc {
                                ch_b.recv().await.unwrap();
                            }
                        });

                        for _ in 0..msg_count {
                            ch_a.send(data.clone(), deadline).await.unwrap();
                        }
                        recv.await.unwrap();
                    });
                });
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Channel latency (small messages)
// ---------------------------------------------------------------------------

fn bench_channel_latency(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let (ch_a, ch_b, _ca, _cb, _na, _nb) = rt.block_on(async {
        let (ca, cb, na, nb) = connect_pair().await;
        let ch_a = ca.open_channel().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        ch_a.send(b"warmup".to_vec(), deadline).await.unwrap();
        let ch_b = cb.accept_channel().await.unwrap();
        ch_b.recv().await.unwrap();
        (ch_a, ch_b, ca, cb, na, nb)
    });

    let ch_a = Arc::new(ch_a);
    let ch_b = Arc::new(ch_b);

    c.bench_function("channel_latency_oneway", |b| {
        b.iter(|| {
            let ch_a = ch_a.clone();
            let ch_b = ch_b.clone();
            rt.block_on(async move {
                let deadline = Instant::now() + Duration::from_secs(30);
                ch_a.send(b"ping".to_vec(), deadline).await.unwrap();
                ch_b.recv().await.unwrap();
            });
        });
    });
}

// ---------------------------------------------------------------------------
// Handshake time
// ---------------------------------------------------------------------------

async fn handshake_time() -> Duration {
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

    let start = Instant::now();
    let _ca = na.connect(addr_b).await.unwrap();
    let elapsed = start.elapsed();
    let _cb = accept.await.unwrap();
    elapsed
}

fn bench_handshake(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    c.bench_function("handshake", |b| {
        b.iter(|| rt.block_on(handshake_time()));
    });
}

criterion_group!(
    benches,
    bench_handshake,
    bench_stream_throughput,
    bench_stream_latency,
    bench_channel_throughput,
    bench_channel_latency,
);
criterion_main!(benches);
