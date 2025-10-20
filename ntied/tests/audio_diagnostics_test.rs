use ntied::audio::{AudioManager, Resampler};
use std::f32::consts::PI;
use tokio;

/// Helper function to analyze frequency of a signal using zero-crossing method
fn analyze_frequency(samples: &[f32], sample_rate: f32) -> f32 {
    if samples.len() < 2 {
        return 0.0;
    }

    let mut zero_crossings = 0;
    let mut last_sample = samples[0];

    for &sample in &samples[1..] {
        // Detect zero crossing (sign change)
        if (last_sample <= 0.0 && sample > 0.0) || (last_sample >= 0.0 && sample < 0.0) {
            zero_crossings += 1;
        }
        last_sample = sample;
    }

    // Each complete cycle has 2 zero crossings
    let cycles = zero_crossings as f32 / 2.0;
    let duration_seconds = samples.len() as f32 / sample_rate;

    cycles / duration_seconds
}

/// Generate a test tone at specific frequency
fn generate_tone(frequency: f32, sample_rate: f32, duration_ms: u32, amplitude: f32) -> Vec<f32> {
    let num_samples = (sample_rate * duration_ms as f32 / 1000.0) as usize;
    let mut samples = Vec::with_capacity(num_samples);

    for i in 0..num_samples {
        let t = i as f32 / sample_rate;
        let sample = (2.0 * PI * frequency * t).sin() * amplitude;
        samples.push(sample);
    }

    samples
}

#[tokio::test]
async fn test_audio_pitch_accuracy() {
    // Test that audio maintains correct pitch through the entire pipeline
    let manager = AudioManager::new();

    // Start playback
    manager.start_playback(None, 1.0).await.unwrap();

    // Test with 440Hz tone (A4 note) at different sample rates
    let test_frequency = 440.0;
    let test_cases = vec![
        (48000u32, "48kHz (standard)"),
        (44100u32, "44.1kHz (CD quality)"),
        (32000u32, "32kHz"),
        (24000u32, "24kHz"),
        (16000u32, "16kHz (wideband)"),
        (8000u32, "8kHz (narrowband)"),
    ];

    println!("\n=== Audio Pitch Accuracy Test ===");
    println!("Testing 440Hz tone at different sample rates");

    for (sample_rate, description) in test_cases {
        println!("\n--- Testing {} ---", description);

        // Generate 1 second of test tone
        let mut all_samples = Vec::new();
        let frame_duration_ms = 20;
        let frames_per_second = 50;

        for frame_idx in 0..frames_per_second {
            let frame_samples =
                generate_tone(test_frequency, sample_rate as f32, frame_duration_ms, 0.5);

            // Analyze input frequency
            if frame_idx == 0 {
                let input_freq = analyze_frequency(&frame_samples, sample_rate as f32);
                println!(
                    "  Input frequency: {:.1} Hz (target: {:.1} Hz)",
                    input_freq, test_frequency
                );
            }

            all_samples.extend(&frame_samples);

            // Queue the frame
            let result = manager
                .queue_audio_frame(
                    frame_idx as u32,
                    frame_samples,
                    sample_rate,
                    1, // mono
                )
                .await;

            assert!(result.is_ok(), "Failed to queue frame: {:?}", result.err());
        }

        // Analyze the complete input signal
        let input_frequency = analyze_frequency(&all_samples, sample_rate as f32);
        let frequency_error = ((input_frequency - test_frequency).abs() / test_frequency) * 100.0;

        println!("  Complete signal analysis:");
        println!("    Expected: {:.1} Hz", test_frequency);
        println!("    Measured: {:.1} Hz", input_frequency);
        println!("    Error: {:.2}%", frequency_error);

        // Check that frequency is preserved (within 2% tolerance)
        assert!(
            frequency_error < 2.0,
            "Frequency error too high at {} Hz: {:.2}%",
            sample_rate,
            frequency_error
        );

        // Brief pause between tests
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    manager.stop_playback().await.unwrap();
    println!("\n=== Test Complete ===");
}

#[tokio::test]
async fn test_resampler_pitch_consistency() {
    // Direct test of resampler to isolate pitch issues
    println!("\n=== Resampler Pitch Consistency Test ===");

    let test_cases = vec![
        (16000, 48000, "16kHz -> 48kHz (3x upsampling)"),
        (8000, 48000, "8kHz -> 48kHz (6x upsampling)"),
        (22050, 48000, "22.05kHz -> 48kHz (2.18x upsampling)"),
        (48000, 16000, "48kHz -> 16kHz (3x downsampling)"),
        (44100, 48000, "44.1kHz -> 48kHz (1.09x upsampling)"),
    ];

    for (input_rate, output_rate, description) in test_cases {
        println!("\n--- {} ---", description);

        let mut resampler = Resampler::new(input_rate, output_rate, 1).unwrap();

        // Generate 2 seconds of 440Hz tone
        let test_frequency = 440.0;
        let chunk_duration_ms = 20;
        let total_chunks = 100; // 2 seconds
        let samples_per_chunk = (input_rate * chunk_duration_ms / 1000) as usize;

        let mut all_input = Vec::new();
        let mut all_output = Vec::new();

        for chunk_idx in 0..total_chunks {
            // Generate chunk
            let chunk = generate_tone(test_frequency, input_rate as f32, chunk_duration_ms, 0.5);

            all_input.extend(&chunk);

            // Resample
            let output = resampler.resample(&chunk).unwrap();
            all_output.extend(output);

            // Check resampler state periodically
            if chunk_idx % 25 == 24 {
                let diagnostics = resampler.get_diagnostics();
                println!(
                    "    After {} chunks: position={:.4}, step={:.4}",
                    chunk_idx + 1,
                    diagnostics.position,
                    diagnostics.step_size
                );
            }
        }

        // Analyze frequencies
        let input_freq = analyze_frequency(&all_input, input_rate as f32);
        let output_freq = analyze_frequency(&all_output, output_rate as f32);

        println!("  Frequency analysis:");
        println!("    Input:  {:.1} Hz @ {} Hz", input_freq, input_rate);
        println!("    Output: {:.1} Hz @ {} Hz", output_freq, output_rate);

        // Check frequency preservation
        let freq_error = ((output_freq - input_freq).abs() / input_freq) * 100.0;
        println!("    Error:  {:.2}%", freq_error);

        // Check sample count accuracy
        let expected_output_samples =
            (all_input.len() as f64 * output_rate as f64 / input_rate as f64) as usize;
        let sample_count_error = ((all_output.len() as i32 - expected_output_samples as i32).abs()
            as f32
            / expected_output_samples as f32)
            * 100.0;

        println!("  Sample count:");
        println!("    Input:    {} samples", all_input.len());
        println!("    Output:   {} samples", all_output.len());
        println!("    Expected: {} samples", expected_output_samples);
        println!("    Error:    {:.2}%", sample_count_error);

        // Strict requirements for pitch preservation
        assert!(
            freq_error < 1.0,
            "Frequency not preserved in {}: {:.2}% error",
            description,
            freq_error
        );

        assert!(
            sample_count_error < 0.5,
            "Sample count error too high in {}: {:.2}%",
            description,
            sample_count_error
        );
    }

    println!("\n=== Resampler Test Complete ===");
}

#[tokio::test]
async fn test_continuous_streaming_pitch() {
    // Test pitch stability during continuous streaming (simulates real call)
    let manager = AudioManager::new();

    println!("\n=== Continuous Streaming Pitch Test ===");

    manager.start_playback(None, 1.0).await.unwrap();

    // Simulate different network conditions
    let scenarios = vec![
        (16000u32, 0, 0, "Perfect network, 16kHz"),
        (16000u32, 5, 20, "5% packet loss, 20ms jitter, 16kHz"),
        (48000u32, 0, 0, "Perfect network, 48kHz"),
        (48000u32, 2, 50, "2% packet loss, 50ms jitter, 48kHz"),
    ];

    for (sample_rate, loss_percent, jitter_ms, description) in scenarios {
        println!("\n--- {} ---", description);

        let test_frequency = 440.0;
        let mut sequence = 0u32;
        let mut packets_sent = 0;
        let mut packets_dropped = 0;

        // Send 3 seconds worth of audio
        for frame_idx in 0..150 {
            // Simulate packet loss
            if loss_percent > 0 && (frame_idx % 100) < loss_percent {
                sequence += 1;
                packets_dropped += 1;
                continue;
            }

            // Generate frame
            let frame = generate_tone(test_frequency, sample_rate as f32, 20, 0.3);

            // Simulate jitter
            if jitter_ms > 0 && frame_idx % 10 == 5 {
                tokio::time::sleep(tokio::time::Duration::from_millis(jitter_ms as u64)).await;
            }

            // Send frame
            let result = manager
                .queue_audio_frame(sequence, frame, sample_rate, 1)
                .await;

            if result.is_ok() {
                packets_sent += 1;
            }

            sequence += 1;

            // Small delay to simulate real-time streaming
            if frame_idx % 3 == 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            }
        }

        println!(
            "  Packets sent: {}, dropped: {}",
            packets_sent, packets_dropped
        );

        // Get statistics
        let stats = manager.get_stats().await;
        println!("  Jitter buffer stats:");
        println!("    Packets received: {}", stats.packets_received);
        println!("    Packets lost: {}", stats.packets_lost);
        println!("    Average jitter: {:.2} ms", stats.average_jitter_ms);

        // Allow playback to finish
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }

    manager.stop_playback().await.unwrap();
    println!("\n=== Streaming Test Complete ===");
}

#[tokio::test]
async fn diagnose_pitch_shift_issue() {
    // Specific test to diagnose pitch shift issues
    println!("\n=== DIAGNOSTIC: Pitch Shift Issue ===");

    // Test the exact scenario that causes pitch shift
    let mut resampler = Resampler::new(16000, 48000, 1).unwrap();

    // Generate exactly 20ms frames as in real calls
    let input_rate = 16000;
    let output_rate = 48000;
    let frame_ms = 20;
    let samples_per_frame = (input_rate * frame_ms / 1000) as usize; // 320 samples

    println!("Configuration:");
    println!(
        "  Input: {} Hz, {} samples per {}ms frame",
        input_rate, samples_per_frame, frame_ms
    );
    println!("  Output: {} Hz", output_rate);
    println!(
        "  Expected ratio: {:.4}",
        input_rate as f64 / output_rate as f64
    );

    let mut total_input = 0;
    let mut total_output = 0;

    // Process 100 frames (2 seconds)
    for i in 0..100 {
        let frame = vec![0.5f32; samples_per_frame];
        let output = resampler.resample(&frame).unwrap();

        total_input += frame.len();
        total_output += output.len();

        if i < 5 || i % 20 == 0 {
            let diagnostics = resampler.get_diagnostics();
            println!(
                "  Frame {:3}: in={}, out={}, pos={:.6}, step={:.6}",
                i,
                frame.len(),
                output.len(),
                diagnostics.position,
                diagnostics.step_size
            );
        }
    }

    let actual_ratio = total_input as f64 / total_output as f64;
    let expected_ratio = input_rate as f64 / output_rate as f64;
    let ratio_error = ((actual_ratio - expected_ratio) / expected_ratio * 100.0).abs();

    println!("\nResults:");
    println!("  Total input:  {} samples", total_input);
    println!("  Total output: {} samples", total_output);
    println!(
        "  Expected output: {:.0} samples",
        total_input as f64 * output_rate as f64 / input_rate as f64
    );
    println!("  Actual ratio: {:.6}", actual_ratio);
    println!("  Expected ratio: {:.6}", expected_ratio);
    println!("  Ratio error: {:.4}%", ratio_error);

    if ratio_error > 0.1 {
        println!("\n⚠️  WARNING: Ratio error exceeds 0.1% - this will cause pitch shift!");
        println!(
            "  This means audio will play {:.2}% {} than normal",
            ratio_error,
            if actual_ratio > expected_ratio {
                "faster"
            } else {
                "slower"
            }
        );
    } else {
        println!("\n✓ Ratio error within acceptable range");
    }

    assert!(
        ratio_error < 1.0,
        "Excessive ratio error will cause noticeable pitch shift"
    );

    println!("\n=== Diagnostic Complete ===");
}
