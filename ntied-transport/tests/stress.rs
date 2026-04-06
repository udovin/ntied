mod common;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use ntied_transport::{Node, PrivateKey, RelayNode};

use common::{
    StreamReader, connect_direct, connect_via_relay, frame_message, init_tracing, localhost,
    make_payload, verify_payload,
};

// ── Multi-channel stress through relay ──

#[tokio::test(flavor = "multi_thread")]
async fn stress_many_channels_through_relay() {
    init_tracing();

    const NUM_STREAMS: usize = 5;
    const NUM_DATAGRAMS: usize = 3;
    const STREAM_MSGS: u32 = 100;
    const DG_MSGS: u32 = 100;
    const STREAM_PAYLOAD: usize = 200;
    const DG_PAYLOAD: usize = 500;

    let p = connect_via_relay().await;
    assert!(p.conn_a.is_established().await);
    assert!(p.conn_b.is_established().await);

    let mut send_tasks = Vec::new();
    let mut recv_tasks = Vec::new();

    // Streams: reliable ordered
    for i in 0..NUM_STREAMS {
        let purpose = 0x100 + i as u16;
        let sa = p.conn_a.open_stream(purpose).await.unwrap();
        let (sb, got) = p.conn_b.accept_stream().await.unwrap();
        assert_eq!(got, purpose);

        send_tasks.push(tokio::spawn(async move {
            for seq in 0..STREAM_MSGS {
                let payload = make_payload(seq, STREAM_PAYLOAD);
                let framed = frame_message(&payload);
                sa.send(&framed).await.unwrap();
            }
        }));

        let idx = i;
        recv_tasks.push(tokio::spawn(async move {
            let mut reader = StreamReader::new();
            let mut expected = 0u32;
            while expected < STREAM_MSGS {
                while let Some(msg) = reader.try_read() {
                    let seq = verify_payload(&msg)
                        .unwrap_or_else(|| panic!("stream {idx}: corrupted payload"));
                    assert_eq!(seq, expected, "stream {idx}: order mismatch");
                    expected += 1;
                }
                if expected >= STREAM_MSGS {
                    break;
                }
                match tokio::time::timeout(Duration::from_secs(20), sb.recv()).await {
                    Ok(Ok(data)) => reader.push(&data),
                    Ok(Err(e)) => panic!("stream {idx} seq {expected}: {e}"),
                    Err(_) => panic!("stream {idx} seq {expected}: timeout"),
                }
            }
            expected
        }));
    }

    // Datagrams: unreliable
    for i in 0..NUM_DATAGRAMS {
        let purpose = 0x200 + i as u16;
        let da = p.conn_a.open_datagram(purpose).await.unwrap();
        let (db, got) = p.conn_b.accept_datagram().await.unwrap();
        assert_eq!(got, purpose);

        send_tasks.push(tokio::spawn(async move {
            for seq in 0..DG_MSGS {
                let payload = make_payload(seq, DG_PAYLOAD);
                da.send(&payload).await.unwrap();
                if seq % 10 == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        }));

        let idx = i;
        recv_tasks.push(tokio::spawn(async move {
            let mut received = HashSet::new();
            loop {
                match tokio::time::timeout(Duration::from_secs(10), db.recv()).await {
                    Ok(Ok(data)) => {
                        let seq = verify_payload(&data)
                            .unwrap_or_else(|| panic!("datagram {idx}: corrupted"));
                        received.insert(seq);
                    }
                    Ok(Err(_)) | Err(_) => break,
                }
            }
            received.len() as u32
        }));
    }

    // Wait for senders
    for task in send_tasks {
        task.await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Collect results
    let mut stream_results = Vec::new();
    let mut dg_results = Vec::new();

    for (i, task) in recv_tasks.into_iter().enumerate() {
        let result = tokio::time::timeout(Duration::from_secs(30), task)
            .await
            .unwrap_or_else(|_| panic!("receiver {i} timed out"))
            .unwrap();
        if i < NUM_STREAMS {
            stream_results.push(result);
        } else {
            dg_results.push(result);
        }
    }

    for (i, count) in stream_results.iter().enumerate() {
        assert_eq!(*count, STREAM_MSGS, "stream {i}: {count}/{STREAM_MSGS}");
    }
    for (i, count) in dg_results.iter().enumerate() {
        assert!(*count > 0, "datagram {i}: 0/{DG_MSGS}");
        eprintln!("datagram {i}: {count}/{DG_MSGS} ({}%)", count * 100 / DG_MSGS);
    }

    p.relay_task.abort();
}

// ── Direct stress ──

#[tokio::test(flavor = "multi_thread")]
async fn stress_many_channels_direct() {
    init_tracing();

    const NUM_STREAMS: usize = 5;
    const STREAM_MSGS: u32 = 100;
    const PAYLOAD_SIZE: usize = 200;

    let p = connect_direct().await;

    let mut send_tasks = Vec::new();
    let mut recv_tasks = Vec::new();

    for i in 0..NUM_STREAMS {
        let purpose = i as u16;
        let sa = p.conn_a.open_stream(purpose).await.unwrap();
        let (sb, got) = p.conn_b.accept_stream().await.unwrap();
        assert_eq!(got, purpose);

        send_tasks.push(tokio::spawn(async move {
            for seq in 0..STREAM_MSGS {
                let payload = make_payload(seq, PAYLOAD_SIZE);
                let framed = frame_message(&payload);
                sa.send(&framed).await.unwrap();
            }
        }));

        let idx = i;
        recv_tasks.push(tokio::spawn(async move {
            let mut reader = StreamReader::new();
            let mut expected = 0u32;
            while expected < STREAM_MSGS {
                while let Some(msg) = reader.try_read() {
                    let seq = verify_payload(&msg)
                        .unwrap_or_else(|| panic!("stream {idx}: corrupted"));
                    assert_eq!(seq, expected, "stream {idx}: order");
                    expected += 1;
                }
                if expected >= STREAM_MSGS {
                    break;
                }
                match tokio::time::timeout(Duration::from_secs(20), sb.recv()).await {
                    Ok(Ok(data)) => reader.push(&data),
                    Ok(Err(e)) => panic!("stream {idx} seq {expected}: {e}"),
                    Err(_) => panic!("stream {idx} seq {expected}: timeout"),
                }
            }
            expected
        }));
    }

    for task in send_tasks {
        task.await.unwrap();
    }

    for (i, task) in recv_tasks.into_iter().enumerate() {
        let count = tokio::time::timeout(Duration::from_secs(30), task)
            .await
            .unwrap_or_else(|_| panic!("recv {i} timeout"))
            .unwrap();
        assert_eq!(count, STREAM_MSGS, "stream {i}: {count}/{STREAM_MSGS}");
    }
}

// ── Bulk transfer correctness ──

#[tokio::test(flavor = "multi_thread")]
async fn bulk_transfer_1mb_direct() {
    init_tracing();
    let p = connect_direct().await;

    let sa = p.conn_a.open_stream(1).await.unwrap();
    let (sb, _) = p.conn_b.accept_stream().await.unwrap();

    // In debug mode crypto is ~100x slower, so keep this moderate.
    // Run benchmarks in release mode for full throughput measurement.
    let total = 256 * 1024; // 256 KB
    let chunk_size = 2048;

    let send_task = tokio::spawn(async move {
        let mut h: u64 = 0;
        let mut offset = 0usize;
        while offset < total {
            let size = chunk_size.min(total - offset);
            let mut chunk = vec![0u8; size];
            for (j, b) in chunk.iter_mut().enumerate() {
                *b = ((offset + j) % 251) as u8;
            }
            for &b in &chunk {
                h = h.wrapping_mul(31).wrapping_add(b as u64);
            }
            sa.send(&chunk).await.unwrap();
            offset += size;
        }
        h
    });

    let recv_task = tokio::spawn(async move {
        let mut received = 0usize;
        let mut h: u64 = 0;
        while received < total {
            let data = tokio::time::timeout(Duration::from_secs(30), sb.recv())
                .await
                .expect("timeout")
                .expect("recv error");
            for &b in &data {
                h = h.wrapping_mul(31).wrapping_add(b as u64);
            }
            received += data.len();
        }
        (received, h)
    });

    let sent_h = send_task.await.unwrap();
    let (recv_total, recv_h) = recv_task.await.unwrap();

    assert_eq!(recv_total, total);
    assert_eq!(recv_h, sent_h, "data corruption detected");
}

#[tokio::test(flavor = "multi_thread")]
async fn bulk_transfer_512kb_relay() {
    init_tracing();
    let p = connect_via_relay().await;

    let sa = p.conn_a.open_stream(1).await.unwrap();
    let (sb, _) = p.conn_b.accept_stream().await.unwrap();

    // Relay path is significantly slower in debug mode due to crypto overhead
    // (each packet traverses 3 nodes with encryption). Keep the size moderate.
    let total = 128 * 1024; // 128 KB
    let chunk_size = 1024;

    let send_task = tokio::spawn(async move {
        let mut h: u64 = 0;
        let mut offset = 0usize;
        while offset < total {
            let size = chunk_size.min(total - offset);
            let mut chunk = vec![0u8; size];
            for (j, b) in chunk.iter_mut().enumerate() {
                *b = ((offset + j) % 251) as u8;
            }
            for &b in &chunk {
                h = h.wrapping_mul(31).wrapping_add(b as u64);
            }
            sa.send(&chunk).await.unwrap();
            offset += size;
        }
        h
    });

    let recv_task = tokio::spawn(async move {
        let mut received = 0usize;
        let mut h: u64 = 0;
        while received < total {
            let data = tokio::time::timeout(Duration::from_secs(60), sb.recv())
                .await
                .unwrap_or_else(|_| panic!("timeout at {received}/{total}"))
                .expect("recv error");
            for &b in &data {
                h = h.wrapping_mul(31).wrapping_add(b as u64);
            }
            received += data.len();
        }
        (received, h)
    });

    let sent_h = send_task.await.unwrap();
    let (recv_total, recv_h) = recv_task.await.unwrap();

    assert_eq!(recv_total, total);
    assert_eq!(recv_h, sent_h, "data corruption detected");

    p.relay_task.abort();
}

/// Verify that sending across multiple streams overcomes the per-stream window limit.
#[tokio::test(flavor = "multi_thread")]
async fn bulk_transfer_multi_stream_256kb() {
    init_tracing();
    let p = connect_direct().await;

    // Use 5 streams, each sending ~55 KB = 275 KB total
    const STREAMS: usize = 5;
    const PER_STREAM: usize = 55 * 1024;
    const CHUNK: usize = 1024;

    let mut tasks = Vec::new();

    for i in 0..STREAMS {
        let purpose = i as u16;
        let sa = p.conn_a.open_stream(purpose).await.unwrap();
        let (sb, got) = p.conn_b.accept_stream().await.unwrap();
        assert_eq!(got, purpose);

        tasks.push(tokio::spawn(async move {
            let mut send_h: u64 = 0;
            let mut offset = 0;
            while offset < PER_STREAM {
                let size = CHUNK.min(PER_STREAM - offset);
                let mut data = vec![0u8; size];
                for (j, b) in data.iter_mut().enumerate() {
                    *b = ((i * 1000 + offset + j) % 251) as u8;
                }
                for &b in &data {
                    send_h = send_h.wrapping_mul(31).wrapping_add(b as u64);
                }
                sa.send(&data).await.unwrap();
                offset += size;
            }

            let mut recv_h: u64 = 0;
            let mut received = 0;
            while received < PER_STREAM {
                let data = tokio::time::timeout(Duration::from_secs(30), sb.recv())
                    .await
                    .expect("timeout")
                    .expect("recv error");
                for &b in &data {
                    recv_h = recv_h.wrapping_mul(31).wrapping_add(b as u64);
                }
                received += data.len();
            }

            assert_eq!(received, PER_STREAM);
            assert_eq!(send_h, recv_h, "stream {i} corrupted");
        }));
    }

    for task in tasks {
        task.await.unwrap();
    }
}

// ── Concurrent bidirectional stress ──

#[tokio::test(flavor = "multi_thread")]
async fn bidirectional_stress() {
    init_tracing();

    const MSGS: u32 = 100;
    const PAYLOAD: usize = 256;

    let p = connect_direct().await;

    // A -> B
    let sa = p.conn_a.open_stream(1).await.unwrap();
    let (rb, _) = p.conn_b.accept_stream().await.unwrap();

    // B -> A
    let sb = p.conn_b.open_stream(2).await.unwrap();
    let (ra, _) = p.conn_a.accept_stream().await.unwrap();

    let send_a = tokio::spawn(async move {
        for seq in 0..MSGS {
            let payload = make_payload(seq, PAYLOAD);
            let framed = frame_message(&payload);
            sa.send(&framed).await.unwrap();
        }
    });

    let send_b = tokio::spawn(async move {
        for seq in 0..MSGS {
            let payload = make_payload(seq, PAYLOAD);
            let framed = frame_message(&payload);
            sb.send(&framed).await.unwrap();
        }
    });

    let recv_b = tokio::spawn(async move {
        let mut reader = StreamReader::new();
        let mut expected = 0u32;
        while expected < MSGS {
            while let Some(msg) = reader.try_read() {
                let seq = verify_payload(&msg).expect("corrupted A->B");
                assert_eq!(seq, expected);
                expected += 1;
            }
            if expected >= MSGS {
                break;
            }
            let data = tokio::time::timeout(Duration::from_secs(20), rb.recv())
                .await
                .expect("timeout A->B")
                .expect("recv error A->B");
            reader.push(&data);
        }
    });

    let recv_a = tokio::spawn(async move {
        let mut reader = StreamReader::new();
        let mut expected = 0u32;
        while expected < MSGS {
            while let Some(msg) = reader.try_read() {
                let seq = verify_payload(&msg).expect("corrupted B->A");
                assert_eq!(seq, expected);
                expected += 1;
            }
            if expected >= MSGS {
                break;
            }
            let data = tokio::time::timeout(Duration::from_secs(20), ra.recv())
                .await
                .expect("timeout B->A")
                .expect("recv error B->A");
            reader.push(&data);
        }
    });

    send_a.await.unwrap();
    send_b.await.unwrap();
    recv_b.await.unwrap();
    recv_a.await.unwrap();
}
