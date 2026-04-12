use std::time::Instant;

use ntied_transport::connection_v2::channel::message::{MessageAssembler, MessageFragmenter};

const MSG_SIZE: usize = 64 * 1024;
const FRAG_SIZE: usize = 1300;
const ITERS: usize = 10_000;

fn profile_assembler_in_order() {
    let frag_data = vec![0xABu8; FRAG_SIZE];
    let num_frags = (MSG_SIZE + FRAG_SIZE - 1) / FRAG_SIZE;

    let mut t_new = 0u128;
    let mut t_write = 0u128;
    let mut t_complete = 0u128;
    let mut t_take = 0u128;

    for _ in 0..ITERS {
        let t0 = Instant::now();
        let mut a = MessageAssembler::new(MSG_SIZE as u64);
        t_new += t0.elapsed().as_nanos();

        let t1 = Instant::now();
        for i in 0..num_frags {
            let off = (i * FRAG_SIZE) as u64;
            let fin = i == num_frags - 1;
            let len = FRAG_SIZE.min(MSG_SIZE - i * FRAG_SIZE);
            a.write(off, &frag_data[..len], fin).unwrap();
        }
        t_write += t1.elapsed().as_nanos();

        let t2 = Instant::now();
        let _ = a.is_complete();
        t_complete += t2.elapsed().as_nanos();

        let t3 = Instant::now();
        let _ = a.take();
        t_take += t3.elapsed().as_nanos();
    }

    println!("=== MessageAssembler IN-ORDER ({num_frags} frags × {FRAG_SIZE}B = {MSG_SIZE}B) ===");
    println!("  new():         {:>6.0} ns/iter", t_new as f64 / ITERS as f64);
    println!("  write() all:   {:>6.0} ns/iter  ({:.0} ns/frag)", t_write as f64 / ITERS as f64, t_write as f64 / ITERS as f64 / num_frags as f64);
    println!("  is_complete(): {:>6.0} ns/iter", t_complete as f64 / ITERS as f64);
    println!("  take():        {:>6.0} ns/iter", t_take as f64 / ITERS as f64);
    println!("  TOTAL:         {:>6.2} µs/iter", (t_new + t_write + t_complete + t_take) as f64 / ITERS as f64 / 1000.0);
}

fn profile_assembler_out_of_order() {
    let frag_data = vec![0xABu8; FRAG_SIZE];
    let num_frags = (MSG_SIZE + FRAG_SIZE - 1) / FRAG_SIZE;

    let mut t_new = 0u128;
    let mut t_write_odd = 0u128;
    let mut t_write_even = 0u128;
    let mut t_take = 0u128;

    for _ in 0..ITERS {
        let t0 = Instant::now();
        let mut a = MessageAssembler::new(MSG_SIZE as u64);
        t_new += t0.elapsed().as_nanos();

        let t1 = Instant::now();
        for i in (1..num_frags).step_by(2) {
            let off = (i * FRAG_SIZE) as u64;
            let fin = i == num_frags - 1;
            let len = FRAG_SIZE.min(MSG_SIZE - i * FRAG_SIZE);
            a.write(off, &frag_data[..len], fin).unwrap();
        }
        t_write_odd += t1.elapsed().as_nanos();

        let t2 = Instant::now();
        for i in (0..num_frags).step_by(2) {
            let off = (i * FRAG_SIZE) as u64;
            let fin = i == num_frags - 1;
            let len = FRAG_SIZE.min(MSG_SIZE - i * FRAG_SIZE);
            a.write(off, &frag_data[..len], fin).unwrap();
        }
        t_write_even += t2.elapsed().as_nanos();

        let t3 = Instant::now();
        let _ = a.take();
        t_take += t3.elapsed().as_nanos();
    }

    let odd_count = (1..num_frags).step_by(2).count();
    let even_count = (0..num_frags).step_by(2).count();

    println!("\n=== MessageAssembler OUT-OF-ORDER ({num_frags} frags) ===");
    println!("  new():          {:>6.0} ns/iter", t_new as f64 / ITERS as f64);
    println!("  write odd ({odd_count}):  {:>6.0} ns/iter  ({:.0} ns/frag)", t_write_odd as f64 / ITERS as f64, t_write_odd as f64 / ITERS as f64 / odd_count as f64);
    println!("  write even ({even_count}): {:>6.0} ns/iter  ({:.0} ns/frag)", t_write_even as f64 / ITERS as f64, t_write_even as f64 / ITERS as f64 / even_count as f64);
    println!("  take():         {:>6.0} ns/iter", t_take as f64 / ITERS as f64);
    println!("  TOTAL:          {:>6.2} µs/iter", (t_new + t_write_odd + t_write_even + t_take) as f64 / ITERS as f64 / 1000.0);
}

fn profile_fragmenter() {
    let data = vec![0xABu8; MSG_SIZE];
    let mut out = [0u8; FRAG_SIZE];

    let mut t_new = 0u128;
    let mut t_emit = 0u128;
    let mut t_emit_count = 0u64;

    for _ in 0..ITERS {
        let t0 = Instant::now();
        let mut f = MessageFragmenter::new(data.clone());
        t_new += t0.elapsed().as_nanos();

        let t1 = Instant::now();
        while f.emit(&mut out).is_some() {
            t_emit_count += 1;
        }
        t_emit += t1.elapsed().as_nanos();
    }

    let frags_per_iter = t_emit_count / ITERS as u64;
    println!("\n=== MessageFragmenter ({MSG_SIZE}B → {frags_per_iter} frags × {FRAG_SIZE}B) ===");
    println!("  new():        {:>6.0} ns/iter", t_new as f64 / ITERS as f64);
    println!("  emit() all:   {:>6.0} ns/iter  ({:.0} ns/frag)", t_emit as f64 / ITERS as f64, t_emit as f64 / t_emit_count as f64);
    println!("  TOTAL:        {:>6.2} µs/iter", (t_new + t_emit) as f64 / ITERS as f64 / 1000.0);
}

fn profile_fragmenter_retransmit() {
    let data = vec![0xABu8; MSG_SIZE];
    let mut out = [0u8; FRAG_SIZE];
    let num_frags = (MSG_SIZE + FRAG_SIZE - 1) / FRAG_SIZE;

    let mut t_emit1 = 0u128;
    let mut t_loss = 0u128;
    let mut t_emit2 = 0u128;

    for _ in 0..ITERS {
        let mut f = MessageFragmenter::new(data.clone());

        let t0 = Instant::now();
        while f.emit(&mut out).is_some() {}
        t_emit1 += t0.elapsed().as_nanos();

        let t1 = Instant::now();
        for i in (0..num_frags).step_by(2) {
            f.loss((i * FRAG_SIZE) as u64, FRAG_SIZE);
        }
        t_loss += t1.elapsed().as_nanos();

        let t2 = Instant::now();
        while f.emit(&mut out).is_some() {}
        t_emit2 += t2.elapsed().as_nanos();
    }

    let loss_count = (0..num_frags).step_by(2).count();
    println!("\n=== MessageFragmenter RETRANSMIT (50%% loss, {loss_count} frags) ===");
    println!("  emit() first:  {:>6.0} ns/iter", t_emit1 as f64 / ITERS as f64);
    println!("  loss() all:    {:>6.0} ns/iter  ({:.0} ns/loss)", t_loss as f64 / ITERS as f64, t_loss as f64 / ITERS as f64 / loss_count as f64);
    println!("  emit() retry:  {:>6.0} ns/iter", t_emit2 as f64 / ITERS as f64);
    println!("  TOTAL:         {:>6.2} µs/iter", (t_emit1 + t_loss + t_emit2) as f64 / ITERS as f64 / 1000.0);
}

fn main() {
    profile_assembler_in_order();
    profile_assembler_out_of_order();
    profile_fragmenter();
    profile_fragmenter_retransmit();
}
