//! Benchmarks for LMS (Least Mean Squares) adaptive filter implementation

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use ntied::audio::codec::sea::lms::{LmsFilter, LmsFilterBank};

fn generate_test_signal(size: usize, pattern: &str) -> Vec<i32> {
    let mut signal = Vec::with_capacity(size);

    match pattern {
        "ar1" => {
            // AR(1) process: x[n] = 0.8 * x[n-1] + noise
            signal.push(1000);
            for i in 1..size {
                let prev = signal[i - 1] as f32;
                let next = (prev * 0.8) as i32 + ((i * 7) % 20 - 10);
                signal.push(next);
            }
        }
        "sine" => {
            // Sinusoidal signal
            for i in 0..size {
                let t = i as f32 * 0.05;
                let sample = (t.sin() * 10000.0) as i32;
                signal.push(sample);
            }
        }
        "mixed" => {
            // Mixed frequency signal
            for i in 0..size {
                let t = i as f32 * 0.05;
                let sample = ((t.sin() * 5000.0)
                    + (t * 2.0).cos() * 3000.0
                    + (t * 5.0).sin() * 1000.0) as i32;
                signal.push(sample);
            }
        }
        "noise" => {
            // Pseudo-random noise
            for i in 0..size {
                let sample = ((i * 31337 + 17) % 20000 - 10000) as i32;
                signal.push(sample);
            }
        }
        _ => {
            // Default: zeros
            signal.resize(size, 0);
        }
    }

    signal
}

fn bench_lms_predict(c: &mut Criterion) {
    c.bench_function("lms_predict", |b| {
        let mut filter = LmsFilter::new();
        // Pre-train the filter
        let training = generate_test_signal(100, "ar1");
        for &sample in &training {
            filter.update(sample);
        }

        b.iter(|| black_box(filter.predict()));
    });
}

fn bench_lms_update(c: &mut Criterion) {
    c.bench_function("lms_update_ar1", |b| {
        let signal = generate_test_signal(1000, "ar1");
        let mut idx = 0;

        b.iter_batched_ref(
            || LmsFilter::new(),
            |filter| {
                let sample = signal[idx % signal.len()];
                idx += 1;
                black_box(filter.update(sample))
            },
            BatchSize::SmallInput,
        );
    });

    c.bench_function("lms_update_sine", |b| {
        let signal = generate_test_signal(1000, "sine");
        let mut idx = 0;

        b.iter_batched_ref(
            || LmsFilter::new(),
            |filter| {
                let sample = signal[idx % signal.len()];
                idx += 1;
                black_box(filter.update(sample))
            },
            BatchSize::SmallInput,
        );
    });

    c.bench_function("lms_update_mixed", |b| {
        let signal = generate_test_signal(1000, "mixed");
        let mut idx = 0;

        b.iter_batched_ref(
            || LmsFilter::new(),
            |filter| {
                let sample = signal[idx % signal.len()];
                idx += 1;
                black_box(filter.update(sample))
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_lms_adapt_weights(c: &mut Criterion) {
    c.bench_function("lms_adapt_weights", |b| {
        let mut filter = LmsFilter::new();
        // Set up some history
        filter.push_sample(100);
        filter.push_sample(200);
        filter.push_sample(150);
        filter.push_sample(175);

        b.iter(|| {
            filter.adapt_weights(black_box(50));
        });
    });
}

fn bench_lms_convergence(c: &mut Criterion) {
    c.bench_function("lms_convergence_100_samples", |b| {
        let signal = generate_test_signal(100, "ar1");

        b.iter(|| {
            let mut filter = LmsFilter::new();
            for &sample in &signal {
                black_box(filter.update(sample));
            }
        });
    });

    c.bench_function("lms_convergence_500_samples", |b| {
        let signal = generate_test_signal(500, "ar1");

        b.iter(|| {
            let mut filter = LmsFilter::new();
            for &sample in &signal {
                black_box(filter.update(sample));
            }
        });
    });
}

fn bench_lms_filter_bank(c: &mut Criterion) {
    c.bench_function("lms_bank_2ch_interleaved", |b| {
        let signal = generate_test_signal(960, "mixed");

        b.iter_batched_ref(
            || LmsFilterBank::new(2),
            |bank| black_box(bank.process_interleaved(&signal, 2)),
            BatchSize::SmallInput,
        );
    });

    c.bench_function("lms_bank_4ch_interleaved", |b| {
        let signal = generate_test_signal(960, "mixed");

        b.iter_batched_ref(
            || LmsFilterBank::new(4),
            |bank| black_box(bank.process_interleaved(&signal, 4)),
            BatchSize::SmallInput,
        );
    });

    c.bench_function("lms_bank_8ch_interleaved", |b| {
        let signal = generate_test_signal(960, "mixed");

        b.iter_batched_ref(
            || LmsFilterBank::new(8),
            |bank| black_box(bank.process_interleaved(&signal, 8)),
            BatchSize::SmallInput,
        );
    });
}

fn bench_lms_compression_ratio(c: &mut Criterion) {
    c.bench_function("lms_compression_ar1", |b| {
        let signal = generate_test_signal(1000, "ar1");

        b.iter(|| {
            let mut filter = LmsFilter::new();
            let mut residuals = Vec::with_capacity(signal.len());

            for &sample in &signal {
                let prediction = filter.predict();
                let residual = sample - prediction;
                filter.push_sample(sample);
                filter.adapt_weights(residual);
                residuals.push(black_box(residual));
            }

            // Calculate compression ratio (energy reduction)
            let original_energy: i64 = signal.iter().map(|&x| (x as i64) * (x as i64)).sum();
            let residual_energy: i64 = residuals.iter().map(|&x| (x as i64) * (x as i64)).sum();
            black_box(original_energy as f64 / residual_energy.max(1) as f64)
        });
    });

    c.bench_function("lms_compression_sine", |b| {
        let signal = generate_test_signal(1000, "sine");

        b.iter(|| {
            let mut filter = LmsFilter::new();
            let mut residuals = Vec::with_capacity(signal.len());

            for &sample in &signal {
                let prediction = filter.predict();
                let residual = sample - prediction;
                filter.push_sample(sample);
                filter.adapt_weights(residual);
                residuals.push(black_box(residual));
            }

            let original_energy: i64 = signal.iter().map(|&x| (x as i64) * (x as i64)).sum();
            let residual_energy: i64 = residuals.iter().map(|&x| (x as i64) * (x as i64)).sum();
            black_box(original_energy as f64 / residual_energy.max(1) as f64)
        });
    });
}

fn bench_lms_memory_access(c: &mut Criterion) {
    c.bench_function("lms_push_sample", |b| {
        let mut filter = LmsFilter::new();

        b.iter(|| {
            filter.push_sample(black_box(12345));
        });
    });

    c.bench_function("lms_history_access", |b| {
        let mut filter = LmsFilter::new();
        filter.push_sample(100);
        filter.push_sample(200);
        filter.push_sample(300);
        filter.push_sample(400);

        b.iter(|| black_box(filter.history()));
    });

    c.bench_function("lms_weights_access", |b| {
        let mut filter = LmsFilter::new();
        filter.update(100);
        filter.update(200);

        b.iter(|| black_box(filter.weights()));
    });
}

fn bench_lms_reset(c: &mut Criterion) {
    c.bench_function("lms_reset", |b| {
        let mut filter = LmsFilter::new();
        // Pre-populate with data
        for i in 0..10 {
            filter.update(i * 100);
        }

        b.iter(|| {
            filter.reset();
        });
    });

    c.bench_function("lms_bank_reset", |b| {
        let mut bank = LmsFilterBank::new(8);
        // Pre-populate with data
        let signal = generate_test_signal(80, "mixed");
        bank.process_interleaved(&signal, 8);

        b.iter(|| {
            bank.reset();
        });
    });
}

criterion_group!(
    benches,
    bench_lms_predict,
    bench_lms_update,
    bench_lms_adapt_weights,
    bench_lms_convergence,
    bench_lms_filter_bank,
    bench_lms_compression_ratio,
    bench_lms_memory_access,
    bench_lms_reset,
);

criterion_main!(benches);
