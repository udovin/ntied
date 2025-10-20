//! Simple benchmark utility for audio codecs
//!
//! This example demonstrates and benchmarks the performance of different audio codecs
//! available in the ntied audio system.

use std::time::Instant;

use anyhow::Result;
use ntied::audio::{AdpcmCodecFactory, CodecFactory, CodecParams, RawCodecFactory};

/// Benchmark results for a codec
#[derive(Debug)]
struct BenchmarkResult {
    codec_name: String,
    encode_time_ms: f64,
    decode_time_ms: f64,
    compression_ratio: f32,
    snr_db: f32,
    packet_loss_recovery_ms: f64,
}

/// Generate test audio samples
fn generate_test_samples(duration_ms: u32, sample_rate: u32, channels: u16) -> Vec<f32> {
    let total_samples = (sample_rate * duration_ms / 1000) as usize * channels as usize;
    let mut samples = Vec::with_capacity(total_samples);

    // Generate a complex test signal with multiple frequency components
    for i in 0..total_samples {
        let t = i as f32 / sample_rate as f32;
        let channel = i % channels as usize;

        // Different frequencies for different channels
        let base_freq = if channel == 0 { 440.0 } else { 554.0 };

        // Mix of fundamental and harmonics
        let sample = (2.0 * std::f32::consts::PI * base_freq * t).sin() * 0.3
            + (2.0 * std::f32::consts::PI * base_freq * 2.0 * t).sin() * 0.2
            + (2.0 * std::f32::consts::PI * base_freq * 3.0 * t).sin() * 0.1
            + (2.0 * std::f32::consts::PI * base_freq * 0.5 * t).sin() * 0.1;

        samples.push(sample.clamp(-1.0, 1.0));
    }

    samples
}

/// Calculate Signal-to-Noise Ratio in dB
fn calculate_snr(original: &[f32], decoded: &[f32]) -> f32 {
    if original.len() != decoded.len() {
        return 0.0;
    }

    let mut signal_power = 0.0f32;
    let mut noise_power = 0.0f32;

    for (orig, dec) in original.iter().zip(decoded.iter()) {
        signal_power += orig * orig;
        let noise = orig - dec;
        noise_power += noise * noise;
    }

    if noise_power > 0.0 {
        10.0 * (signal_power / noise_power).log10()
    } else {
        100.0 // Perfect reconstruction
    }
}

/// Benchmark a single codec
fn benchmark_codec(
    factory: &dyn CodecFactory,
    params: CodecParams,
    test_samples: &[f32],
    iterations: u32,
) -> Result<BenchmarkResult> {
    let codec_name = match factory.codec_type() {
        ntied::audio::CodecType::ADPCM => "ADPCM".to_string(),
        ntied::audio::CodecType::Raw => "Raw".to_string(),
    };

    println!("Benchmarking {} codec...", codec_name);

    let mut encoder = factory.create_encoder(params.clone())?;
    let mut decoder = factory.create_decoder(params.clone())?;

    // Warmup
    for _ in 0..5 {
        let encoded = encoder.encode(test_samples)?;
        let _ = decoder.decode(&encoded)?;
    }

    // Benchmark encoding
    let mut encode_times = Vec::new();
    let mut encoded_sizes = Vec::new();

    for _ in 0..iterations {
        let start = Instant::now();
        let encoded = encoder.encode(test_samples)?;
        let elapsed = start.elapsed();
        encode_times.push(elapsed);
        encoded_sizes.push(encoded.len());
    }

    let avg_encode_time = encode_times
        .iter()
        .map(|d| d.as_secs_f64() * 1000.0)
        .sum::<f64>()
        / iterations as f64;

    // Benchmark decoding
    let test_encoded = encoder.encode(test_samples)?;
    let mut decode_times = Vec::new();
    let mut decoded_samples = Vec::new();

    for _ in 0..iterations {
        decoder.reset()?;
        let start = Instant::now();
        let decoded = decoder.decode(&test_encoded)?;
        let elapsed = start.elapsed();
        decode_times.push(elapsed);
        if decoded_samples.is_empty() {
            decoded_samples = decoded;
        }
    }

    let avg_decode_time = decode_times
        .iter()
        .map(|d| d.as_secs_f64() * 1000.0)
        .sum::<f64>()
        / iterations as f64;

    // Benchmark packet loss concealment
    decoder.reset()?;
    // First, feed one good frame
    let _ = decoder.decode(&test_encoded)?;

    let mut plc_times = Vec::new();
    for _ in 0..iterations {
        let start = Instant::now();
        let _ = decoder.conceal_packet_loss()?;
        let elapsed = start.elapsed();
        plc_times.push(elapsed);
    }

    let avg_plc_time = plc_times
        .iter()
        .map(|d| d.as_secs_f64() * 1000.0)
        .sum::<f64>()
        / iterations as f64;

    // Calculate metrics
    let original_size = test_samples.len() * 4; // 4 bytes per f32
    let avg_encoded_size = encoded_sizes.iter().sum::<usize>() / iterations as usize;
    let compression_ratio = original_size as f32 / avg_encoded_size.max(1) as f32;

    let snr_db = if decoded_samples.len() == test_samples.len() {
        calculate_snr(test_samples, &decoded_samples)
    } else {
        0.0
    };

    Ok(BenchmarkResult {
        codec_name,
        encode_time_ms: avg_encode_time,
        decode_time_ms: avg_decode_time,
        compression_ratio,
        snr_db,
        packet_loss_recovery_ms: avg_plc_time,
    })
}

fn print_results(results: &[BenchmarkResult]) {
    println!("\n{:-<80}", "");
    println!("CODEC BENCHMARK RESULTS");
    println!("{:-<80}", "");

    println!(
        "\n{:<15} | {:>12} | {:>12} | {:>12} | {:>10} | {:>12}",
        "Codec", "Encode (ms)", "Decode (ms)", "Compress", "SNR (dB)", "PLC (ms)"
    );
    println!("{:-<80}", "");

    for result in results {
        println!(
            "{:<15} | {:>12.3} | {:>12.3} | {:>11.1}x | {:>10.1} | {:>12.3}",
            result.codec_name,
            result.encode_time_ms,
            result.decode_time_ms,
            result.compression_ratio,
            result.snr_db,
            result.packet_loss_recovery_ms,
        );
    }

    println!("{:-<80}", "");

    // Find best performers
    if let Some(best_compression) = results.iter().max_by(|a, b| {
        a.compression_ratio
            .partial_cmp(&b.compression_ratio)
            .unwrap()
    }) {
        println!(
            "Best Compression: {} ({:.1}x)",
            best_compression.codec_name, best_compression.compression_ratio
        );
    }

    if let Some(best_quality) = results
        .iter()
        .filter(|r| r.snr_db < 100.0) // Exclude lossless
        .max_by(|a, b| a.snr_db.partial_cmp(&b.snr_db).unwrap())
    {
        println!(
            "Best Quality (lossy): {} ({:.1} dB)",
            best_quality.codec_name, best_quality.snr_db
        );
    }

    if let Some(fastest_encode) = results
        .iter()
        .min_by(|a, b| a.encode_time_ms.partial_cmp(&b.encode_time_ms).unwrap())
    {
        println!(
            "Fastest Encoding: {} ({:.3} ms)",
            fastest_encode.codec_name, fastest_encode.encode_time_ms
        );
    }

    if let Some(fastest_plc) = results.iter().min_by(|a, b| {
        a.packet_loss_recovery_ms
            .partial_cmp(&b.packet_loss_recovery_ms)
            .unwrap()
    }) {
        println!(
            "Fastest PLC: {} ({:.3} ms)",
            fastest_plc.codec_name, fastest_plc.packet_loss_recovery_ms
        );
    }
}

fn main() -> Result<()> {
    println!("Audio Codec Benchmark Utility");
    println!("==============================\n");

    // Test parameters
    let duration_ms = 20; // 20ms frames (typical for VoIP)
    let iterations = 100;
    let params = CodecParams::voice();

    println!("Test Configuration:");
    println!("  Frame duration: {} ms", duration_ms);
    println!("  Sample rate: {} Hz", params.sample_rate);
    println!("  Channels: {}", params.channels);
    println!("  Iterations: {}", iterations);
    println!("  Bitrate target: {} bps", params.bitrate);

    // Generate test samples
    let test_samples = generate_test_samples(duration_ms, params.sample_rate, params.channels);
    println!("  Test samples: {}", test_samples.len());
    println!();

    let mut results = Vec::new();

    // SEA codec removed due to audio quality issues
    // Benchmark ADPCM codec first
    let adpcm_factory = AdpcmCodecFactory;
    if let Ok(result) = benchmark_codec(&adpcm_factory, params.clone(), &test_samples, iterations) {
        results.push(result);
    }

    // Benchmark Raw codec (baseline)
    let raw_factory = RawCodecFactory;
    if let Ok(result) = benchmark_codec(&raw_factory, params.clone(), &test_samples, iterations) {
        results.push(result);
    }

    // Print results
    print_results(&results);

    println!("\nNote: Lower encoding/decoding times are better.");
    println!("      Higher compression ratios and SNR values are better.");
    println!("      PLC time indicates packet loss recovery performance.");

    Ok(())
}
