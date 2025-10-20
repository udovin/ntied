use ntied::audio::{AdpcmCodecFactory, CodecFactory, CodecParams};
use std::f32::consts::PI;

#[test]
fn test_audio_stability_no_pitch_shift() {
    // Test that multiple encode/decode cycles don't cause pitch drift
    let params = CodecParams::voice();
    let factory = AdpcmCodecFactory;

    let mut encoder = factory.create_encoder(params.clone()).unwrap();
    let mut decoder = factory.create_decoder(params).unwrap();

    // Generate a test signal with known frequency
    let sample_rate = 48000;
    let test_frequency = 1000.0; // 1 kHz test tone
    let frame_size = 960; // 20ms at 48kHz
    let num_frames = 50; // 1 second of audio

    println!("Testing audio stability over {} frames", num_frames);

    // Track frequency stability
    let mut frequency_measurements = Vec::new();

    for frame_num in 0..num_frames {
        // Generate frame with continuous phase
        let mut frame = Vec::with_capacity(frame_size);
        for i in 0..frame_size {
            let global_sample_idx = frame_num * frame_size + i;
            let t = global_sample_idx as f32 / sample_rate as f32;
            let sample = 0.5 * (2.0 * PI * test_frequency * t).sin();
            frame.push(sample);
        }

        // Encode and decode
        let encoded = encoder.encode(&frame).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        // Measure frequency using zero-crossing detection
        let measured_freq = measure_frequency_zero_crossing(&decoded, sample_rate as f32);
        if measured_freq > 0.0 {
            frequency_measurements.push(measured_freq);
        }

        // Check frame size consistency
        assert_eq!(
            decoded.len(),
            frame_size,
            "Frame {} size mismatch: expected {}, got {}",
            frame_num,
            frame_size,
            decoded.len()
        );
    }

    // Analyze frequency stability
    if !frequency_measurements.is_empty() {
        let avg_frequency =
            frequency_measurements.iter().sum::<f32>() / frequency_measurements.len() as f32;
        let max_freq = frequency_measurements.iter().fold(0.0f32, |a, &b| a.max(b));
        let min_freq = frequency_measurements
            .iter()
            .fold(f32::MAX, |a, &b| a.min(b));

        println!("Frequency measurements:");
        println!(
            "  Average: {:.2} Hz (expected: {:.2} Hz)",
            avg_frequency, test_frequency
        );
        println!("  Min: {:.2} Hz", min_freq);
        println!("  Max: {:.2} Hz", max_freq);
        println!("  Variation: {:.2} Hz", max_freq - min_freq);

        // Check that frequency is stable (within 3% tolerance for ADPCM)
        // ADPCM quantization can slightly affect frequency measurement
        let tolerance = test_frequency * 0.03;
        assert!(
            (avg_frequency - test_frequency).abs() < tolerance,
            "Average frequency drift too large: {:.2} Hz (expected < {:.2} Hz)",
            (avg_frequency - test_frequency).abs(),
            tolerance
        );

        // Check that variation is small (allow up to 5% for ADPCM)
        let variation_tolerance = test_frequency * 0.05;
        assert!(
            max_freq - min_freq < variation_tolerance,
            "Frequency variation too large: {:.2} Hz (expected < {:.2} Hz)",
            max_freq - min_freq,
            variation_tolerance
        );
    }
}

#[test]
fn test_no_audio_interruptions() {
    // Test that codec doesn't introduce gaps or discontinuities
    let params = CodecParams::voice();
    let factory = AdpcmCodecFactory;

    let mut encoder = factory.create_encoder(params.clone()).unwrap();
    let mut decoder = factory.create_decoder(params).unwrap();

    let sample_rate = 48000;
    let frame_size = 960;

    // Generate multiple frames with a continuous signal
    let mut all_original = Vec::new();
    let mut all_decoded = Vec::new();

    for frame_num in 0..10 {
        // Generate smooth sine wave that continues across frames
        let mut frame = Vec::with_capacity(frame_size);
        for i in 0..frame_size {
            let global_idx = frame_num * frame_size + i;
            let t = global_idx as f32 / sample_rate as f32;
            let sample = 0.7 * (2.0 * PI * 440.0 * t).sin(); // A4 note
            frame.push(sample);
            all_original.push(sample);
        }

        let encoded = encoder.encode(&frame).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();
        all_decoded.extend_from_slice(&decoded);
    }

    // Check for discontinuities at frame boundaries
    let mut max_discontinuity = 0.0f32;
    for frame_num in 1..10 {
        let boundary_idx = frame_num * frame_size;
        if boundary_idx > 0 && boundary_idx < all_decoded.len() {
            let discontinuity = (all_decoded[boundary_idx] - all_decoded[boundary_idx - 1]).abs();
            max_discontinuity = max_discontinuity.max(discontinuity);

            // For a 440Hz sine at 0.7 amplitude, max sample difference should be small
            let max_expected_diff = 0.7 * 2.0 * PI * 440.0 / sample_rate as f32 * 2.0; // Conservative estimate
            assert!(
                discontinuity < max_expected_diff,
                "Large discontinuity at frame boundary {}: {:.4} (expected < {:.4})",
                frame_num,
                discontinuity,
                max_expected_diff
            );
        }
    }

    println!(
        "Maximum discontinuity at frame boundaries: {:.4}",
        max_discontinuity
    );
}

#[test]
fn test_encoder_decoder_state_consistency() {
    // Test that encoder and decoder maintain consistent state
    let params = CodecParams::voice();
    let factory = AdpcmCodecFactory;

    let mut encoder = factory.create_encoder(params.clone()).unwrap();
    let mut decoder = factory.create_decoder(params).unwrap();

    // Generate and process several frames
    let frame_size = 960;
    let mut previous_decoded: Option<Vec<f32>> = None;

    for i in 0..20 {
        // Alternate between different signal patterns
        let frame = if i % 2 == 0 {
            vec![0.3; frame_size] // Constant positive
        } else {
            vec![-0.3; frame_size] // Constant negative
        };

        let encoded = encoder.encode(&frame).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        // Check that decoder produces consistent output for similar input
        if let Some(prev) = previous_decoded.as_ref() {
            if i > 1 && i % 2 == 0 {
                // Compare with frame from 2 iterations ago (same pattern)
                let max_diff = decoded
                    .iter()
                    .zip(prev.iter())
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0f32, f32::max);

                // ADPCM is adaptive - state affects output even for identical input
                // Allow larger tolerance as the codec adapts to signal changes
                assert!(
                    max_diff < 0.35,
                    "Frame {} differs too much from similar previous frame: max_diff = {:.4}",
                    i,
                    max_diff
                );
            }
        }

        if i % 2 == 0 {
            previous_decoded = Some(decoded);
        }
    }
}

#[test]
fn test_varying_sample_rates() {
    // Test that codec works correctly with different sample rates
    let sample_rates = [8000, 16000, 24000, 48000];

    for &sample_rate in &sample_rates {
        println!("Testing sample rate: {} Hz", sample_rate);

        let mut params = CodecParams::voice();
        params.sample_rate = sample_rate;

        let factory = AdpcmCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Frame size for 20ms
        let frame_size = (sample_rate * 20 / 1000) as usize;

        // Generate test signal at appropriate frequency for sample rate
        let test_freq = sample_rate as f32 / 10.0; // 1/10 of sample rate
        let mut frame = Vec::with_capacity(frame_size);

        for i in 0..frame_size {
            let t = i as f32 / sample_rate as f32;
            let sample = 0.5 * (2.0 * PI * test_freq * t).sin();
            frame.push(sample);
        }

        let encoded = encoder.encode(&frame).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        // Verify output size
        assert_eq!(
            decoded.len(),
            frame_size,
            "Sample rate {}: frame size mismatch",
            sample_rate
        );

        // Verify signal integrity (rough check)
        let energy_original: f32 = frame.iter().map(|x| x * x).sum();
        let energy_decoded: f32 = decoded.iter().map(|x| x * x).sum();
        let energy_ratio = energy_decoded / energy_original;

        assert!(
            (0.5..=2.0).contains(&energy_ratio),
            "Sample rate {}: energy ratio {:.2} out of range",
            sample_rate,
            energy_ratio
        );
    }
}

// Helper function to measure frequency using zero-crossing detection
fn measure_frequency_zero_crossing(samples: &[f32], sample_rate: f32) -> f32 {
    // Use a small threshold to avoid noise affecting zero-crossing detection
    let threshold = 0.01;
    let mut zero_crossings = 0;
    let mut last_sample = samples[0];

    for &sample in &samples[1..] {
        // Detect zero crossing with threshold
        if (last_sample >= threshold && sample < -threshold)
            || (last_sample <= -threshold && sample > threshold)
        {
            zero_crossings += 1;
        }
        last_sample = sample;
    }

    // Each period has 2 zero crossings
    let periods = zero_crossings as f32 / 2.0;
    let duration = samples.len() as f32 / sample_rate;

    if duration > 0.0 && periods > 0.0 {
        periods / duration
    } else {
        0.0
    }
}
