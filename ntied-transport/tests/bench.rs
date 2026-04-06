mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ntied_transport::{Node, PrivateKey, RelayNode};

use common::{connect_direct, connect_via_relay, init_tracing, localhost};

// ── Bandwidth benchmarks ──
//
// These tests measure throughput and print results. They use #[ignore] so they
// don't run in normal `cargo test` — run with `cargo test --test bench -- --ignored`.

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_stream_bandwidth_direct() {
    init_tracing();
    let p = connect_direct().await;

    let sa = p.conn_a.open_stream(1).await.unwrap();
    let (sb, _) = p.conn_b.accept_stream().await.unwrap();

    let total_bytes: usize = 10 * 1024 * 1024; // 10 MB
    let chunk_size = 8192;
    let chunk = vec![0xABu8; chunk_size];

    let send_chunk = chunk.clone();
    let send_task = tokio::spawn(async move {
        let start = Instant::now();
        let mut sent = 0;
        while sent < total_bytes {
            sa.send(&send_chunk).await.unwrap();
            sent += chunk_size;
        }
        start.elapsed()
    });

    let recv_task = tokio::spawn(async move {
        let start = Instant::now();
        let mut received = 0usize;
        while received < total_bytes {
            let data = tokio::time::timeout(Duration::from_secs(60), sb.recv())
                .await
                .expect("timeout")
                .expect("recv error");
            received += data.len();
        }
        (start.elapsed(), received)
    });

    let send_elapsed = send_task.await.unwrap();
    let (recv_elapsed, recv_total) = recv_task.await.unwrap();

    let send_mbps = (total_bytes as f64 / 1024.0 / 1024.0) / send_elapsed.as_secs_f64();
    let recv_mbps = (recv_total as f64 / 1024.0 / 1024.0) / recv_elapsed.as_secs_f64();

    eprintln!("=== Direct Stream Bandwidth ===");
    eprintln!("  Total: {} MB", total_bytes / 1024 / 1024);
    eprintln!("  Send:  {send_mbps:.2} MB/s ({send_elapsed:.2?})");
    eprintln!("  Recv:  {recv_mbps:.2} MB/s ({recv_elapsed:.2?})");
    eprintln!("  Received: {recv_total} bytes");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_stream_bandwidth_relay() {
    init_tracing();
    let p = connect_via_relay().await;

    let sa = p.conn_a.open_stream(1).await.unwrap();
    let (sb, _) = p.conn_b.accept_stream().await.unwrap();

    let total_bytes: usize = 5 * 1024 * 1024; // 5 MB (relay is slower)
    let chunk_size = 4096;
    let chunk = vec![0xCDu8; chunk_size];

    let send_chunk = chunk.clone();
    let send_task = tokio::spawn(async move {
        let start = Instant::now();
        let mut sent = 0;
        while sent < total_bytes {
            sa.send(&send_chunk).await.unwrap();
            sent += chunk_size;
        }
        start.elapsed()
    });

    let recv_task = tokio::spawn(async move {
        let start = Instant::now();
        let mut received = 0usize;
        while received < total_bytes {
            let data = tokio::time::timeout(Duration::from_secs(120), sb.recv())
                .await
                .expect("timeout")
                .expect("recv error");
            received += data.len();
        }
        (start.elapsed(), received)
    });

    let send_elapsed = send_task.await.unwrap();
    let (recv_elapsed, recv_total) = recv_task.await.unwrap();

    let send_mbps = (total_bytes as f64 / 1024.0 / 1024.0) / send_elapsed.as_secs_f64();
    let recv_mbps = (recv_total as f64 / 1024.0 / 1024.0) / recv_elapsed.as_secs_f64();

    eprintln!("=== Relay Stream Bandwidth ===");
    eprintln!("  Total: {} MB", total_bytes / 1024 / 1024);
    eprintln!("  Send:  {send_mbps:.2} MB/s ({send_elapsed:.2?})");
    eprintln!("  Recv:  {recv_mbps:.2} MB/s ({recv_elapsed:.2?})");

    p.relay_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_datagram_throughput_direct() {
    init_tracing();
    let p = connect_direct().await;

    let da = p.conn_a.open_datagram(1).await.unwrap();
    let (db, _) = p.conn_b.accept_datagram().await.unwrap();

    let msg_count = 10_000;
    let msg_size = 512;
    let msg = vec![0xEFu8; msg_size];

    let send_msg = msg.clone();
    let send_task = tokio::spawn(async move {
        let start = Instant::now();
        for _ in 0..msg_count {
            da.send(&send_msg).await.unwrap();
        }
        start.elapsed()
    });

    let recv_task = tokio::spawn(async move {
        let start = Instant::now();
        let mut received = 0usize;
        loop {
            match tokio::time::timeout(Duration::from_secs(10), db.recv()).await {
                Ok(Ok(data)) => {
                    received += 1;
                    assert_eq!(data.len(), msg_size);
                }
                _ => break,
            }
        }
        (start.elapsed(), received)
    });

    let send_elapsed = send_task.await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (recv_elapsed, recv_count) = recv_task.await.unwrap();

    let total_sent = msg_count * msg_size;
    let total_recv = recv_count * msg_size;
    let send_mbps = (total_sent as f64 / 1024.0 / 1024.0) / send_elapsed.as_secs_f64();
    let recv_mbps = (total_recv as f64 / 1024.0 / 1024.0) / recv_elapsed.as_secs_f64();
    let delivery = recv_count as f64 / msg_count as f64 * 100.0;

    eprintln!("=== Direct Datagram Throughput ===");
    eprintln!("  Messages: {msg_count} x {msg_size}B");
    eprintln!("  Send:     {send_mbps:.2} MB/s ({send_elapsed:.2?})");
    eprintln!("  Recv:     {recv_mbps:.2} MB/s ({recv_elapsed:.2?})");
    eprintln!("  Delivery: {recv_count}/{msg_count} ({delivery:.1}%)");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_connection_establishment_direct() {
    init_tracing();

    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    let node_b = Arc::new(Node::bind(localhost(), PrivateKey::generate()).await.unwrap());
    let addr_b = node_b.local_addr().unwrap();

    let iterations = 20;
    let mut times = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let nb = node_b.clone();
        let accept = tokio::spawn(async move { nb.accept().await.unwrap() });

        let start = Instant::now();
        let conn_a = node_a.connect(addr_b).await.unwrap();
        let elapsed = start.elapsed();

        let conn_b = accept.await.unwrap();
        assert!(conn_a.is_established().await);
        assert!(conn_b.is_established().await);

        times.push(elapsed);
        conn_a.close().await.unwrap();
        drop(conn_b);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let avg = times.iter().map(|t| t.as_micros()).sum::<u128>() / iterations as u128;
    let min = times.iter().map(|t| t.as_micros()).min().unwrap();
    let max = times.iter().map(|t| t.as_micros()).max().unwrap();

    eprintln!("=== Direct Connection Establishment ===");
    eprintln!("  Iterations: {iterations}");
    eprintln!("  Avg: {avg} us");
    eprintln!("  Min: {min} us");
    eprintln!("  Max: {max} us");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_connection_establishment_relay() {
    init_tracing();

    let relay = RelayNode::bind(localhost(), PrivateKey::generate())
        .await
        .unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let _relay_task = tokio::spawn(async move { relay.run().await });

    let node_a = Node::bind(localhost(), PrivateKey::generate()).await.unwrap();
    node_a.attach_relay(relay_addr).await.unwrap();

    let id_b = PrivateKey::generate();
    let peer_id_b = id_b.public_key().peer_id();
    let node_b = Arc::new(Node::bind(localhost(), id_b).await.unwrap());
    node_b.attach_relay(relay_addr).await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let iterations = 10;
    let mut times = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let nb = node_b.clone();
        let accept = tokio::spawn(async move { nb.accept().await.unwrap() });

        let start = Instant::now();
        let conn_a = node_a.connect_peer(&peer_id_b).await.unwrap();
        let elapsed = start.elapsed();

        let conn_b = accept.await.unwrap();
        assert!(conn_a.is_established().await);
        assert!(conn_b.is_established().await);

        times.push(elapsed);
        conn_a.close().await.unwrap();
        drop(conn_b);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let avg = times.iter().map(|t| t.as_micros()).sum::<u128>() / iterations as u128;
    let min = times.iter().map(|t| t.as_micros()).min().unwrap();
    let max = times.iter().map(|t| t.as_micros()).max().unwrap();

    eprintln!("=== Relay Connection Establishment ===");
    eprintln!("  Iterations: {iterations}");
    eprintln!("  Avg: {avg} us");
    eprintln!("  Min: {min} us");
    eprintln!("  Max: {max} us");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_stream_latency_direct() {
    init_tracing();
    let p = connect_direct().await;

    let sa = p.conn_a.open_stream(1).await.unwrap();
    let (sb, _) = p.conn_b.accept_stream().await.unwrap();

    // B echoes back
    let sb2 = p.conn_b.open_stream(2).await.unwrap();
    let (sa2, _) = p.conn_a.accept_stream().await.unwrap();

    let iterations = 100;
    let mut rtts = Vec::with_capacity(iterations);

    for i in 0..iterations {
        let msg = format!("ping-{i}");
        let start = Instant::now();
        sa.send(msg.as_bytes()).await.unwrap();

        let data = tokio::time::timeout(Duration::from_secs(5), sb.recv())
            .await
            .expect("timeout")
            .expect("recv error");

        sb2.send(&data).await.unwrap();
        let echo = tokio::time::timeout(Duration::from_secs(5), sa2.recv())
            .await
            .expect("timeout")
            .expect("recv error");
        let rtt = start.elapsed();

        assert_eq!(echo, msg.as_bytes());
        rtts.push(rtt);
    }

    let avg_us = rtts.iter().map(|t| t.as_micros()).sum::<u128>() / iterations as u128;
    let min_us = rtts.iter().map(|t| t.as_micros()).min().unwrap();
    let max_us = rtts.iter().map(|t| t.as_micros()).max().unwrap();

    // Median
    let mut sorted: Vec<u128> = rtts.iter().map(|t| t.as_micros()).collect();
    sorted.sort();
    let median_us = sorted[sorted.len() / 2];

    eprintln!("=== Direct Stream RTT Latency ===");
    eprintln!("  Iterations: {iterations}");
    eprintln!("  Avg:    {avg_us} us");
    eprintln!("  Median: {median_us} us");
    eprintln!("  Min:    {min_us} us");
    eprintln!("  Max:    {max_us} us");
}
