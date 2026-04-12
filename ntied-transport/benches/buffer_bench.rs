use criterion::{Criterion, black_box, criterion_group, criterion_main};

use ntied_transport::connection_v2::stream::buffer;
use ntied_transport::connection_v2::stream::experimental_buffer;

const CAP: usize = 256 * 1024; // 256 KB
const CHUNK: usize = 1300; // ~MTU

fn send_write_emit_ack(c: &mut Criterion) {
    let data = vec![0xABu8; CHUNK];
    let mut out = vec![0u8; CHUNK];

    let mut group = c.benchmark_group("send_write_emit_ack");

    group.bench_function("experimental_buffer", |b| {
        b.iter(|| {
            let mut buf = experimental_buffer::SendBuf::new(CAP);
            for _ in 0..CAP / CHUNK {
                buf.write(black_box(&data), false);
            }
            for _ in 0..CAP / CHUNK {
                let (off, n, _) = buf.emit(&mut out);
                buf.ack(off, n);
            }
        });
    });

    group.bench_function("buffer", |b| {
        b.iter(|| {
            let mut buf = buffer::SendBuf::new(CAP);
            for _ in 0..CAP / CHUNK {
                buf.write(black_box(&data), false);
            }
            for _ in 0..CAP / CHUNK {
                let (off, n, _) = buf.emit(&mut out);
                buf.ack(off, n);
            }
        });
    });

    group.finish();
}

fn send_retransmit(c: &mut Criterion) {
    let data = vec![0xABu8; CHUNK];
    let mut out = vec![0u8; CHUNK];

    let mut group = c.benchmark_group("send_retransmit");

    group.bench_function("experimental_buffer", |b| {
        b.iter(|| {
            let mut buf = experimental_buffer::SendBuf::new(CAP);
            // Fill and emit.
            for _ in 0..CAP / CHUNK {
                buf.write(black_box(&data), false);
            }
            for _ in 0..CAP / CHUNK {
                buf.emit(&mut out);
            }
            // Lose every other chunk and retransmit.
            let chunks = CAP / CHUNK;
            for i in (0..chunks).step_by(2) {
                buf.loss((i * CHUNK) as u64, CHUNK);
            }
            while buf.has_retransmits() {
                buf.emit(&mut out);
            }
        });
    });

    group.bench_function("buffer", |b| {
        b.iter(|| {
            let mut buf = buffer::SendBuf::new(CAP);
            for _ in 0..CAP / CHUNK {
                buf.write(black_box(&data), false);
            }
            for _ in 0..CAP / CHUNK {
                buf.emit(&mut out);
            }
            let chunks = CAP / CHUNK;
            for i in (0..chunks).step_by(2) {
                buf.loss((i * CHUNK) as u64, CHUNK);
            }
            while buf.has_retransmits() {
                buf.emit(&mut out);
            }
        });
    });

    group.finish();
}

fn recv_in_order(c: &mut Criterion) {
    let data = vec![0xABu8; CHUNK];
    let mut out = vec![0u8; CHUNK];

    let mut group = c.benchmark_group("recv_in_order");

    group.bench_function("experimental_buffer", |b| {
        b.iter(|| {
            let mut buf = experimental_buffer::RecvBuf::new(CAP);
            for i in 0..CAP / CHUNK {
                let off = (i * CHUNK) as u64;
                buf.write(off, black_box(&data), false).unwrap();
                buf.read(&mut out);
            }
        });
    });

    group.bench_function("buffer", |b| {
        b.iter(|| {
            let mut buf = buffer::RecvBuf::new(CAP);
            for i in 0..CAP / CHUNK {
                let off = (i * CHUNK) as u64;
                buf.write(off, black_box(&data), false).unwrap();
                buf.read(&mut out);
            }
        });
    });

    group.finish();
}

fn recv_out_of_order(c: &mut Criterion) {
    let data = vec![0xABu8; CHUNK];
    let mut out = vec![0u8; CAP];

    let mut group = c.benchmark_group("recv_out_of_order");
    let chunks = CAP / CHUNK;

    group.bench_function("experimental_buffer", |b| {
        b.iter(|| {
            let mut buf = experimental_buffer::RecvBuf::new(CAP);
            // Write odd chunks first, then even.
            for i in (1..chunks).step_by(2) {
                let off = (i * CHUNK) as u64;
                buf.write(off, black_box(&data), false).unwrap();
            }
            for i in (0..chunks).step_by(2) {
                let off = (i * CHUNK) as u64;
                buf.write(off, black_box(&data), false).unwrap();
            }
            buf.read(&mut out);
        });
    });

    group.bench_function("buffer", |b| {
        b.iter(|| {
            let mut buf = buffer::RecvBuf::new(CAP);
            for i in (1..chunks).step_by(2) {
                let off = (i * CHUNK) as u64;
                buf.write(off, black_box(&data), false).unwrap();
            }
            for i in (0..chunks).step_by(2) {
                let off = (i * CHUNK) as u64;
                buf.write(off, black_box(&data), false).unwrap();
            }
            buf.read(&mut out);
        });
    });

    group.finish();
}

fn send_streaming(c: &mut Criterion) {
    let data = vec![0xABu8; CHUNK];
    let mut out = vec![0u8; CHUNK];

    let total = 10 * CAP; // 2.5 MB total throughput
    let mut group = c.benchmark_group("send_streaming");

    group.bench_function("experimental_buffer", |b| {
        b.iter(|| {
            let mut buf = experimental_buffer::SendBuf::new(CAP);
            let mut written = 0u64;
            while written < total as u64 {
                let n = buf.write(black_box(&data), false) as u64;
                written += n;
                if buf.cap() == 0 || n == 0 {
                    while buf.unsent() > 0 {
                        let (off, n, _) = buf.emit(&mut out);
                        buf.ack(off, n);
                    }
                    buf.update_max_data(buf.ack_off() + CAP as u64);
                }
            }
        });
    });

    group.bench_function("buffer", |b| {
        b.iter(|| {
            let mut buf = buffer::SendBuf::new(CAP);
            let mut written = 0u64;
            while written < total as u64 {
                let n = buf.write(black_box(&data), false) as u64;
                written += n;
                if buf.cap() == 0 || n == 0 {
                    while buf.unsent() > 0 {
                        let (off, n, _) = buf.emit(&mut out);
                        buf.ack(off, n);
                    }
                    buf.update_max_data(buf.ack_off() + CAP as u64);
                }
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    send_write_emit_ack,
    send_retransmit,
    recv_in_order,
    recv_out_of_order,
    send_streaming,
);
criterion_main!(benches);
