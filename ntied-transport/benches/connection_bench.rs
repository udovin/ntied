//! Raw `Connection`-level throughput bench: no UDP, no Tokio, no Node.
//!
//! Drives two `Connection`s synchronously through an in-memory packet buffer
//! to measure the transport-layer ceiling.  This isolates the cost of:
//!   - encrypt/decrypt
//!   - frame encode/decode
//!   - stream/channel manager bookkeeping
//!   - ACK/loss tracking
//!
//! Anything below this is the cost added by UDP I/O, async scheduling, and
//! the Node-level orchestration in the `node_bench`.

use std::time::Instant;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use ntied_transport::PrivateKey;
use ntied_transport::connection::{Config, Connection, ConnectionId, RecvInfo};
use ntied_transport::wire::packet::parse_init;

/// Maximum packet size we exchange.  Mirrors a UDP MTU.
const PKT_BUF: usize = 1500;

fn established_pair() -> (Connection, Connection) {
    let t = Instant::now();
    let mut buf = [0u8; PKT_BUF];

    let mut client =
        Connection::open_with_config(ConnectionId(1), PrivateKey::generate(), Config::default());
    let (n, _) = client.send(&mut buf, t).unwrap();
    let init = parse_init(&buf[..n]).unwrap();
    let mut server = Connection::accept_with_config(
        ConnectionId(2),
        ConnectionId(init.initiator_connection_id),
        init.kem_public_key,
        PrivateKey::generate(),
        Config::default(),
    );

    // InitAck
    let (n, _) = server.send(&mut buf, t).unwrap();
    client.recv(&mut buf[..n], RecvInfo { now: t }).unwrap();

    // Drain remaining handshake packets until quiescent.
    loop {
        let mut progress = false;
        while let Ok((n, _)) = client.send(&mut buf, t) {
            server.recv(&mut buf[..n], RecvInfo { now: t }).unwrap();
            progress = true;
        }
        while let Ok((n, _)) = server.send(&mut buf, t) {
            client.recv(&mut buf[..n], RecvInfo { now: t }).unwrap();
            progress = true;
        }
        if !progress {
            break;
        }
    }
    assert!(client.is_established());
    assert!(server.is_established());
    (client, server)
}

/// Drain whatever `sender` wants to send into `receiver`.
fn pump(sender: &mut Connection, receiver: &mut Connection, buf: &mut [u8], now: Instant) {
    while let Ok((n, _)) = sender.send(buf, now) {
        receiver.recv(&mut buf[..n], RecvInfo { now }).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Channel throughput (raw)
// ---------------------------------------------------------------------------

fn bench_channel_throughput_raw(c: &mut Criterion) {
    let mut group = c.benchmark_group("raw_channel_throughput");

    for &(msg_size, total_bytes) in &[
        (64usize, 1024 * 1024),     // 16384 msgs
        (512, 1024 * 1024),         // 2048 msgs
        (4096, 4 * 1024 * 1024),    // 1024 msgs
        (65536, 16 * 1024 * 1024),  // 256 msgs
    ] {
        let msg_count = total_bytes / msg_size;

        group.throughput(Throughput::Bytes(total_bytes as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("msg_{msg_size}")),
            &msg_size,
            |b, &ms| {
                b.iter_with_setup(
                    || {
                        let (client, server) = established_pair();
                        let buf = vec![0u8; PKT_BUF];
                        (client, server, buf, vec![0xCDu8; ms])
                    },
                    |(mut client, mut server, mut pkt, payload)| {
                        let t = Instant::now();
                        let mut delivered = 0usize;
                        let mut queued = 0usize;
                        // Pre-queue + drain in lockstep so the channel buffer
                        // doesn't overflow.
                        while delivered < msg_count {
                            // Try to queue another message.
                            if queued < msg_count {
                                match client.channel_send(0, payload.clone(), true) {
                                    Ok(_) => queued += 1,
                                    Err(_) => {}
                                }
                            }
                            // Drive packets client -> server.
                            pump(&mut client, &mut server, &mut pkt, t);
                            // Drain delivered messages on server.
                            while let Ok(msg) = server.channel_recv(0) {
                                black_box(msg);
                                delivered += 1;
                            }
                            // Server -> client (ACKs, MaxData, etc.).
                            pump(&mut server, &mut client, &mut pkt, t);
                        }
                    },
                );
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Stream throughput (raw) — for comparison with channels.
// ---------------------------------------------------------------------------

fn bench_stream_throughput_raw(c: &mut Criterion) {
    let mut group = c.benchmark_group("raw_stream_throughput");

    for &(chunk_size, total_bytes) in &[
        (64usize, 1024 * 1024),
        (512, 1024 * 1024),
        (4096, 4 * 1024 * 1024),
        (65536, 16 * 1024 * 1024),
    ] {
        group.throughput(Throughput::Bytes(total_bytes as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("chunk_{chunk_size}")),
            &chunk_size,
            |b, &cs| {
                b.iter_with_setup(
                    || {
                        let (client, server) = established_pair();
                        let buf = vec![0u8; PKT_BUF];
                        (client, server, buf, vec![0xCDu8; cs])
                    },
                    |(mut client, mut server, mut pkt, payload)| {
                        let t = Instant::now();
                        let mut sent = 0usize;
                        let mut received = 0usize;
                        let mut recv_buf = vec![0u8; 65536];
                        while received < total_bytes {
                            // Push more bytes if room.
                            while sent < total_bytes {
                                let remaining = total_bytes - sent;
                                let chunk = &payload[..payload.len().min(remaining)];
                                let w = client.stream_write(0, chunk, false).unwrap();
                                if w == 0 {
                                    break;
                                }
                                sent += w;
                            }
                            pump(&mut client, &mut server, &mut pkt, t);
                            // Drain stream.
                            loop {
                                match server.stream_read(0, &mut recv_buf) {
                                    Ok((n, _)) if n > 0 => {
                                        received += n;
                                    }
                                    _ => break,
                                }
                            }
                            pump(&mut server, &mut client, &mut pkt, t);
                        }
                    },
                );
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Handshake cost (raw)
// ---------------------------------------------------------------------------

fn bench_handshake_raw(c: &mut Criterion) {
    c.bench_function("raw_handshake", |b| {
        b.iter(|| {
            let (client, server) = established_pair();
            black_box((client, server));
        });
    });
}

criterion_group!(
    benches,
    bench_handshake_raw,
    bench_channel_throughput_raw,
    bench_stream_throughput_raw,
);
criterion_main!(benches);
