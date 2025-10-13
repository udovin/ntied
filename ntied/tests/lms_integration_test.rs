//! Integration tests for LMS (Least Mean Squares) implementation in SEA codec

use ntied::audio::{CodecFactory, CodecParams, SeaCodecFactory};

/// Helper function to generate test signals
fn generate_test_signal(pattern: &str, size: usize) -> Vec<f32> {
    match pattern {
        "ar1" => {
            // AR(1) process: x[n] = 0.8 * x[n-1] + noise
            let mut signal = vec![0.5f32];
            for i in 1..size {
                let prev = signal[i - 1];
                let noise = ((i * 7) % 20) as f32 / 100.0 - 0.1;
                let next = (prev * 0.8 + noise).clamp(-0.95, 0.95);
                signal.push(next);
            }
            signal
        }
        "sine" => {
            // Pure sine wave
            (0..size)
                .map(|i| {
                    let t = i as f32 * 0.02;
                    (t.sin() * 0.7).clamp(-0.95, 0.95)
                })
                .collect()
        }
        "mixed" => {
            // Mixed frequency signal
            (0..size)
                .map(|i| {
                    let t = i as f32 * 0.01;
                    let signal = (t.sin() * 0.4 + (t * 2.0).cos() * 0.3).clamp(-0.95, 0.95);
                    signal
                })
                .collect()
        }
        "speech" => {
            // Simulated speech-like signal with formants
            (0..size)
                .map(|i| {
                    let t = i as f32 * 0.001;
                    let f1 = (t * 700.0).sin() * 0.3; // First formant
                    let f2 = (t * 1220.0).sin() * 0.2; // Second formant
                    let f3 = (t * 2600.0).sin() * 0.1; // Third formant
                    let envelope = (t * 5.0).sin().abs();
                    ((f1 + f2 + f3) * envelope).clamp(-0.95, 0.95)
                })
                .collect()
        }
        _ => vec![0.0f32; size],
    }
}

/// Calculate SNR (Signal-to-Noise Ratio) in dB
fn calculate_snr(original: &[f32], decoded: &[f32]) -> f32 {
    let signal_power: f32 = original.iter().map(|x| x * x).sum::<f32>() / original.len() as f32;
    let noise_power: f32 = original
        .iter()
        .zip(decoded.iter())
        .map(|(o, d)| {
            let diff = o - d;
            diff * diff
        })
        .sum::<f32>()
        / original.len() as f32;

    if noise_power > 0.0 {
        10.0 * (signal_power / noise_power).log10()
    } else {
        100.0 // Perfect reconstruction
    }
}

#[test]
fn test_lms_improves_compression_quality() {
    let factory = SeaCodecFactory;
    let params = CodecParams::voice();

    // Test with highly correlated signal (AR process)
    let signal = generate_test_signal("ar1", 960);

    let mut encoder = factory.create_encoder(params.clone()).unwrap();
    let mut decoder = factory.create_decoder(params).unwrap();

    // First frame - LMS is learning
    let encoded1 = encoder.encode(&signal[..480]).unwrap();
    let decoded1 = decoder.decode(&encoded1).unwrap();
    let snr1 = calculate_snr(&signal[..480], &decoded1);

    // Second frame - LMS should be adapted
    let encoded2 = encoder.encode(&signal[480..960]).unwrap();
    let decoded2 = decoder.decode(&encoded2).unwrap();
    let snr2 = calculate_snr(&signal[480..960], &decoded2);

    println!(
        "AR(1) signal - Frame 1 SNR: {:.2} dB, Frame 2 SNR: {:.2} dB",
        snr1, snr2
    );

    // Second frame should have better quality after adaptation
    assert!(
        snr2 >= snr1 * 0.9,
        "LMS should maintain or improve quality over time: {:.2} -> {:.2}",
        snr1,
        snr2
    );
}

#[test]
fn test_lms_multichannel_processing() {
    let factory = SeaCodecFactory;
    let mut params = CodecParams::voice();
    params.channels = 2;

    // Create different signals for each channel
    let left_signal = generate_test_signal("sine", 480);
    let right_signal = generate_test_signal("mixed", 480);

    // Interleave channels
    let mut stereo_signal = Vec::with_capacity(960);
    for i in 0..480 {
        stereo_signal.push(left_signal[i]);
        stereo_signal.push(right_signal[i]);
    }

    let mut encoder = factory.create_encoder(params.clone()).unwrap();
    let mut decoder = factory.create_decoder(params).unwrap();

    // Process multiple frames to let LMS adapt
    for _ in 0..3 {
        let encoded = encoder.encode(&stereo_signal).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        assert_eq!(decoded.len(), 960, "Stereo frame size should be correct");

        // Verify samples are valid
        for sample in &decoded {
            assert!(
                sample.is_finite() && *sample >= -1.0 && *sample <= 1.0,
                "Sample out of range: {}",
                sample
            );
        }
    }

    // Final quality check
    let encoded = encoder.encode(&stereo_signal).unwrap();
    let decoded = decoder.decode(&encoded).unwrap();

    // De-interleave and check each channel
    let mut left_decoded = Vec::with_capacity(480);
    let mut right_decoded = Vec::with_capacity(480);
    for i in (0..960).step_by(2) {
        left_decoded.push(decoded[i]);
        right_decoded.push(decoded[i + 1]);
    }

    let left_snr = calculate_snr(&left_signal, &left_decoded);
    let right_snr = calculate_snr(&right_signal, &right_decoded);

    println!(
        "Stereo - Left SNR: {:.2} dB, Right SNR: {:.2} dB",
        left_snr, right_snr
    );

    // Both channels should have reasonable quality
    assert!(left_snr > 5.0, "Left channel SNR too low: {:.2}", left_snr);
    assert!(
        right_snr > 5.0,
        "Right channel SNR too low: {:.2}",
        right_snr
    );
}

#[test]
fn test_lms_packet_loss_prediction() {
    let factory = SeaCodecFactory;
    let params = CodecParams::voice();

    // Use predictable signal for better PLC
    let signal = generate_test_signal("speech", 1920);

    let mut encoder = factory.create_encoder(params.clone()).unwrap();
    let mut decoder = factory.create_decoder(params).unwrap();

    // Train LMS with first frame
    let encoded1 = encoder.encode(&signal[..960]).unwrap();
    let _decoded1 = decoder.decode(&encoded1).unwrap();

    // Simulate packet loss - decoder should use LMS for prediction
    let plc_frame = decoder.conceal_packet_loss().unwrap();

    // Continue with next frame
    let encoded3 = encoder.encode(&signal[960..1920]).unwrap();
    let decoded3 = decoder.decode(&encoded3).unwrap();

    // PLC frame should have reasonable correlation with expected signal
    let expected = &signal[960..1920];
    let correlation: f32 = plc_frame
        .iter()
        .zip(expected.iter())
        .map(|(a, b)| a * b)
        .sum::<f32>()
        / 960.0;

    println!("PLC correlation with original: {:.4}", correlation);

    // Should maintain some correlation (not random noise)
    assert!(
        correlation.abs() > 0.01,
        "PLC should maintain signal characteristics: {}",
        correlation
    );

    // Recovery frame should be good quality
    let recovery_snr = calculate_snr(expected, &decoded3);
    println!("Recovery frame SNR: {:.2} dB", recovery_snr);

    assert!(
        recovery_snr > 5.0,
        "Recovery after PLC should be good: {:.2}",
        recovery_snr
    );
}

#[test]
fn test_lms_adaptation_to_signal_changes() {
    let factory = SeaCodecFactory;
    let params = CodecParams::voice();

    let mut encoder = factory.create_encoder(params.clone()).unwrap();
    let mut decoder = factory.create_decoder(params).unwrap();

    // Start with one type of signal
    let signal1 = generate_test_signal("sine", 960);
    let encoded1 = encoder.encode(&signal1).unwrap();
    let decoded1 = decoder.decode(&encoded1).unwrap();
    let snr1 = calculate_snr(&signal1, &decoded1);

    // Switch to different signal type
    let signal2 = generate_test_signal("mixed", 960);
    let encoded2 = encoder.encode(&signal2).unwrap();
    let decoded2 = decoder.decode(&encoded2).unwrap();
    let snr2 = calculate_snr(&signal2, &decoded2);

    // Continue with new signal type (should adapt)
    let signal3 = generate_test_signal("mixed", 960);
    let encoded3 = encoder.encode(&signal3).unwrap();
    let decoded3 = decoder.decode(&encoded3).unwrap();
    let snr3 = calculate_snr(&signal3, &decoded3);

    println!(
        "Adaptation - Sine SNR: {:.2}, Mixed 1 SNR: {:.2}, Mixed 2 SNR: {:.2}",
        snr1, snr2, snr3
    );

    // Quality should improve or stabilize after adaptation
    assert!(
        snr3 >= snr2 * 0.9,
        "LMS should adapt to new signal type: {:.2} -> {:.2}",
        snr2,
        snr3
    );
}

#[test]
fn test_lms_long_sequence_stability() {
    let factory = SeaCodecFactory;
    let params = CodecParams::voice();

    let mut encoder = factory.create_encoder(params.clone()).unwrap();
    let mut decoder = factory.create_decoder(params).unwrap();

    let mut snr_history = Vec::new();

    // Process many frames to test long-term stability
    for i in 0..20 {
        let signal = generate_test_signal("speech", 960);
        let encoded = encoder.encode(&signal).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        let snr = calculate_snr(&signal, &decoded);
        snr_history.push(snr);

        // Check for NaN or infinite values
        for sample in &decoded {
            assert!(sample.is_finite(), "Frame {} produced non-finite sample", i);
        }
    }

    // Calculate average SNR for different periods
    let early_avg = snr_history[..5].iter().sum::<f32>() / 5.0;
    let late_avg = snr_history[15..].iter().sum::<f32>() / 5.0;

    println!(
        "Long sequence - Early avg SNR: {:.2} dB, Late avg SNR: {:.2} dB",
        early_avg, late_avg
    );

    // Quality should remain stable or improve
    assert!(
        late_avg >= early_avg * 0.8,
        "LMS should remain stable over long sequences: {:.2} -> {:.2}",
        early_avg,
        late_avg
    );
}

#[test]
fn test_lms_reset_functionality() {
    let factory = SeaCodecFactory;
    let params = CodecParams::voice();

    let mut encoder = factory.create_encoder(params.clone()).unwrap();
    let mut decoder = factory.create_decoder(params).unwrap();

    // Process some frames to build up LMS state
    let signal1 = generate_test_signal("ar1", 960);
    for _ in 0..5 {
        let encoded = encoder.encode(&signal1).unwrap();
        let _ = decoder.decode(&encoded).unwrap();
    }

    // Reset encoder and decoder
    encoder.reset().unwrap();
    decoder.reset().unwrap();

    // Process new signal after reset
    let signal2 = generate_test_signal("sine", 960);
    let encoded = encoder.encode(&signal2).unwrap();
    let decoded = decoder.decode(&encoded).unwrap();

    // Should work correctly after reset
    assert_eq!(decoded.len(), 960);

    let snr = calculate_snr(&signal2, &decoded);
    println!("SNR after reset: {:.2} dB", snr);

    // Quality should be reasonable even right after reset
    assert!(
        snr > 3.0,
        "Should maintain basic quality after reset: {:.2}",
        snr
    );
}

#[test]
fn test_lms_different_signal_types_comparison() {
    let factory = SeaCodecFactory;
    let params = CodecParams::voice();

    let signal_types = vec![
        ("ar1", "Highly correlated"),
        ("sine", "Periodic"),
        ("mixed", "Multi-frequency"),
        ("speech", "Speech-like"),
    ];

    println!("\nLMS Performance across signal types:");
    println!("{:-<50}", "");

    for (signal_type, description) in signal_types {
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params.clone()).unwrap();

        let signal = generate_test_signal(signal_type, 960);

        // Let LMS adapt with a few frames
        for _ in 0..3 {
            let _ = encoder.encode(&signal).unwrap();
        }

        // Measure quality
        let encoded = encoder.encode(&signal).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();
        let snr = calculate_snr(&signal, &decoded);

        println!("{:15} ({:15}): {:.2} dB", signal_type, description, snr);

        // All signal types should achieve minimum quality
        assert!(
            snr > 2.0,
            "Signal type '{}' SNR too low: {:.2}",
            signal_type,
            snr
        );
    }
}
