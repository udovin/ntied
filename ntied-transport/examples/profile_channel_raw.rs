//! Raw-Connection profiling harness.
//!
//! Mirrors `benches/connection_bench.rs::raw_channel_throughput` but as a
//! standalone binary suitable for samply / perf / instruments:
//!
//!     cargo build --release --example profile_channel_raw
//!     samply record -- target/release/examples/profile_channel_raw <msg_size> <total_mib>
//!
//! Defaults: msg_size=64, total_mib=64 (≈ 1M small messages).  Smaller
//! messages amplify per-fragment overhead, which is where the channel hot
//! path is most visible.

use std::env;
use std::time::Instant;

use ntied_transport::PrivateKey;
use ntied_transport::connection::{Config, Connection, ConnectionId, RecvInfo};
use ntied_transport::wire::packet::parse_init;

const PKT_BUF: usize = 1500;

fn established_pair() -> (Connection, Connection) {
    let t = Instant::now();
    let mut buf = [0u8; PKT_BUF];

    let mut client = Connection::open_with_config(
        ConnectionId(1),
        PrivateKey::generate(),
        Config::default(),
    );
    let (n, _) = client.send(&mut buf, t).unwrap();
    let init = parse_init(&buf[..n]).unwrap();
    let mut server = Connection::accept_with_config(
        ConnectionId(2),
        ConnectionId(init.initiator_connection_id),
        init.kem_public_key,
        PrivateKey::generate(),
        Config::default(),
    );

    let (n, _) = server.send(&mut buf, t).unwrap();
    client.recv(&mut buf[..n], RecvInfo { now: t }).unwrap();

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

fn pump(sender: &mut Connection, receiver: &mut Connection, buf: &mut [u8], now: Instant) {
    while let Ok((n, _)) = sender.send(buf, now) {
        receiver.recv(&mut buf[..n], RecvInfo { now }).unwrap();
    }
}

fn main() {
    let msg_size: usize = env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(64);
    let total_mib: usize = env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(64);

    let total_bytes = total_mib * 1024 * 1024;
    let msg_count = total_bytes / msg_size;

    eprintln!(
        "profile_channel_raw: msg_size={msg_size} msgs={msg_count} total={total_mib} MiB"
    );

    let (mut client, mut server) = established_pair();
    let mut pkt = vec![0u8; PKT_BUF];
    let payload = vec![0xCDu8; msg_size];

    let start = Instant::now();
    let t = start;
    let mut delivered = 0usize;
    let mut queued = 0usize;

    while delivered < msg_count {
        while queued < msg_count {
            match client.channel_send(0, payload.clone(), true) {
                Ok(_) => queued += 1,
                Err(_) => break,
            }
        }
        pump(&mut client, &mut server, &mut pkt, t);
        while let Ok(msg) = server.channel_recv(0) {
            std::hint::black_box(msg);
            delivered += 1;
        }
        pump(&mut server, &mut client, &mut pkt, t);
    }

    let elapsed = start.elapsed();
    let mib_per_sec = (total_bytes as f64 / (1 << 20) as f64) / elapsed.as_secs_f64();
    eprintln!("{total_bytes} bytes in {elapsed:?} = {mib_per_sec:.1} MiB/s");
}
