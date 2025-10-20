use ntied::audio::{AdpcmCodecFactory, CodecFactory, CodecParams};

#[test]
fn test_audio_encoding_quality() {
    // Test that ADPCM codec provides good quality for voice
    let params = CodecParams::voice();
    let factory = AdpcmCodecFactory;

    let mut encoder = factory.create_encoder(params.clone()).unwrap();
    let mut decoder = factory.create_decoder(params).unwrap();

    // Generate test signal - simple sine wave to check for pitch shift
    let duration_ms = 100;
    let sample_rate = 48000;
    let num_samples = (sample_rate * duration_ms / 1000) as usize;
    let test_frequency = 440.0; // A4 note

    // Create a simple test signal
    let mut original = Vec::with_capacity(num_samples);
    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        let sample = 0.5 * (2.0 * std::f32::consts::PI * test_frequency * t).sin();
        original.push(sample);
    }

    // Encode and decode
    let encoded = encoder.encode(&original).unwrap();
    let decoded = decoder.decode(&encoded).unwrap();

    // Verify lengths match
    assert_eq!(
        original.len(),
        decoded.len(),
        "Decoded audio should have same length as original"
    );

    // Calculate Signal-to-Noise Ratio (SNR)
    let mut signal_power = 0.0f32;
    let mut noise_power = 0.0f32;

    for (orig, dec) in original.iter().zip(decoded.iter()) {
        signal_power += orig * orig;
        let error = orig - dec;
        noise_power += error * error;
    }

    signal_power /= original.len() as f32;
    noise_power /= original.len() as f32;

    // Avoid division by zero
    let snr_db = if noise_power > 0.0 {
        10.0 * (signal_power / noise_power).log10()
    } else {
        100.0 // Perfect reconstruction
    };

    println!("ADPCM codec SNR: {:.2} dB", snr_db);

    // ADPCM should provide at least 15 dB SNR for simple signals
    assert!(
        snr_db > 15.0,
        "SNR too low: {:.2} dB (expected > 15 dB)",
        snr_db
    );

    // Check that decoded signal has reasonable amplitude
    let max_decoded = decoded.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
    let max_original = original.iter().map(|x| x.abs()).fold(0.0f32, f32::max);

    println!("Max original amplitude: {:.3}", max_original);
    println!("Max decoded amplitude: {:.3}", max_decoded);

    // Check that amplitude didn't change dramatically (no major gain issues)
    let amplitude_ratio = max_decoded / max_original;
    assert!(
        (0.5..=2.0).contains(&amplitude_ratio),
        "Amplitude changed too much: ratio = {:.2} (expected 0.5-2.0)",
        amplitude_ratio
    );

    // Simple check: ensure no samples are clipping
    let clipped_samples = decoded.iter().filter(|&&x| x.abs() > 0.99).count();
    assert!(
        clipped_samples < decoded.len() / 100, // Less than 1% clipping
        "Too many clipped samples: {} out of {}",
        clipped_samples,
        decoded.len()
    );
}
