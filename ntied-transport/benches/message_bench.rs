use criterion::{Criterion, black_box, criterion_group, criterion_main};

use ntied_transport::channel::message::{MessageAssembler, MessageFragmenter};
use ntied_transport::stream::buffer::{RecvBuf, SendBuf};

const MSG_SIZE: usize = 64 * 1024; // 64 KB message
const FRAG_SIZE: usize = 1300; // ~MTU

fn send_fragment_emit(c: &mut Criterion) {
    let data = vec![0xABu8; MSG_SIZE];
    let mut out = [0u8; FRAG_SIZE];

    let mut group = c.benchmark_group("send_fragment");

    group.bench_function("message_fragmenter", |b| {
        b.iter(|| {
            let mut f = MessageFragmenter::new(black_box(data.clone()));
            while f.emit(&mut out).is_some() {}
        });
    });

    group.bench_function("send_buf", |b| {
        b.iter(|| {
            let mut buf = SendBuf::new(MSG_SIZE);
            buf.write(black_box(&data), true);
            while buf.unsent() > 0 || buf.has_retransmits() {
                buf.emit(&mut out);
            }
        });
    });

    group.finish();
}

fn recv_assemble_in_order(c: &mut Criterion) {
    // Generate fragments covering entire message, last one may be shorter.
    let num_frags = (MSG_SIZE + FRAG_SIZE - 1) / FRAG_SIZE;
    let frags: Vec<(u64, Vec<u8>)> = (0..num_frags)
        .map(|i| {
            let off = i * FRAG_SIZE;
            let len = FRAG_SIZE.min(MSG_SIZE - off);
            (off as u64, vec![0xABu8; len])
        })
        .collect();

    let mut group = c.benchmark_group("recv_assemble_in_order");

    group.bench_function("message_assembler", |b| {
        b.iter(|| {
            let mut a = MessageAssembler::new();
            let last = frags.len() - 1;
            for (i, (off, data)) in frags.iter().enumerate() {
                a.write(*off, black_box(data), i == last).unwrap();
            }
            assert!(a.is_complete());
            black_box(a.take());
        });
    });

    group.bench_function("recv_buf", |b| {
        let mut out = vec![0u8; MSG_SIZE];
        b.iter(|| {
            let mut buf = RecvBuf::new(MSG_SIZE);
            for (off, data) in &frags {
                buf.write(*off, black_box(data), false).unwrap();
            }
            buf.read(&mut out);
            black_box(&out);
        });
    });

    group.finish();
}

fn recv_assemble_out_of_order(c: &mut Criterion) {
    let num_frags = (MSG_SIZE + FRAG_SIZE - 1) / FRAG_SIZE;
    let frags: Vec<(u64, Vec<u8>)> = (0..num_frags)
        .map(|i| {
            let off = i * FRAG_SIZE;
            let len = FRAG_SIZE.min(MSG_SIZE - off);
            (off as u64, vec![0xABu8; len])
        })
        .collect();

    let mut group = c.benchmark_group("recv_assemble_out_of_order");

    group.bench_function("message_assembler", |b| {
        b.iter(|| {
            let mut a = MessageAssembler::new();
            for i in (1..num_frags).step_by(2) {
                let fin = frags[i].0 as usize + frags[i].1.len() == MSG_SIZE;
                a.write(frags[i].0, black_box(&frags[i].1), fin).unwrap();
            }
            for i in (0..num_frags).step_by(2) {
                let fin = frags[i].0 as usize + frags[i].1.len() == MSG_SIZE;
                a.write(frags[i].0, black_box(&frags[i].1), fin).unwrap();
            }
            assert!(a.is_complete());
            black_box(a.take());
        });
    });

    group.bench_function("recv_buf", |b| {
        let mut out = vec![0u8; MSG_SIZE];
        b.iter(|| {
            let mut buf = RecvBuf::new(MSG_SIZE);
            for i in (1..num_frags).step_by(2) {
                buf.write(frags[i].0, black_box(&frags[i].1), false)
                    .unwrap();
            }
            for i in (0..num_frags).step_by(2) {
                buf.write(frags[i].0, black_box(&frags[i].1), false)
                    .unwrap();
            }
            buf.read(&mut out);
            black_box(&out);
        });
    });

    group.finish();
}

fn send_with_retransmit(c: &mut Criterion) {
    let data = vec![0xABu8; MSG_SIZE];
    let mut out = [0u8; FRAG_SIZE];
    let chunks = MSG_SIZE / FRAG_SIZE;

    let mut group = c.benchmark_group("send_with_retransmit");

    group.bench_function("message_fragmenter", |b| {
        b.iter(|| {
            let mut f = MessageFragmenter::new(black_box(data.clone()));
            while f.emit(&mut out).is_some() {}
            // Lose every other chunk.
            for i in (0..chunks).step_by(2) {
                f.loss((i * FRAG_SIZE) as u64, FRAG_SIZE);
            }
            while f.emit(&mut out).is_some() {}
        });
    });

    group.bench_function("send_buf", |b| {
        b.iter(|| {
            let mut buf = SendBuf::new(MSG_SIZE);
            buf.write(black_box(&data), true);
            while buf.unsent() > 0 {
                buf.emit(&mut out);
            }
            for i in (0..chunks).step_by(2) {
                buf.loss((i * FRAG_SIZE) as u64, FRAG_SIZE);
            }
            while buf.has_retransmits() {
                buf.emit(&mut out);
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    send_fragment_emit,
    recv_assemble_in_order,
    recv_assemble_out_of_order,
    send_with_retransmit,
);
criterion_main!(benches);
