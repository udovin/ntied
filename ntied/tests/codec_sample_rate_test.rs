//! Tests to verify that codec sample rate is correctly propagated through the audio chain
//! This is critical to prevent pitch shifting bugs

use ntied::audio::{AudioManager, CodecManager, CodecParams, CodecType, NegotiatedCodec};
use std::sync::Arc;
use tokio;

#[tokio::test]
async fn test_codec_decoder_output_sample_rate() {
    // Test that decoder reports correct output sample rate
    let codec_manager = Arc::new(CodecManager::new());

    // Initialize with 48kHz
    let negotiated = NegotiatedCodec {
        codec: CodecType::ADPCM,
        params: CodecParams {
            sample_rate: 48000,
            channels: 1,
            bitrate: 32000,
            fec: false,
            dtx: false,
            expected_packet_loss: 5,
            complexity: 10,
        },
        is_offerer: true,
    };

    codec_manager.initialize(&negotiated).await.unwrap();

    // Verify decoder reports correct sample rate
    let decoder_rate = codec_manager.decoder_output_sample_rate().await;
    assert_eq!(
        decoder_rate,
        Some(48000),
        "Decoder should report 48000 Hz output rate"
    );

    // Encode and decode some samples
    let input_samples = vec![0.5f32; 960]; // 20ms at 48kHz
    let (codec_type, encoded) = codec_manager.encode(&input_samples).await.unwrap();
    let decoded = codec_manager.decode(codec_type, &encoded).await.unwrap();

    // Decoded samples should match input length (same rate)
    assert_eq!(
        decoded.len(),
        input_samples.len(),
        "Decoded samples should have same length as input for same rate"
    );
}

#[tokio::test]
async fn test_codec_sample_rate_mismatch_handling() {
    // Test what happens when packet claims different rate than codec outputs
    let codec_manager = Arc::new(CodecManager::new());

    // Initialize codec at 48kHz
    let negotiated = NegotiatedCodec {
        codec: CodecType::ADPCM,
        params: CodecParams {
            sample_rate: 48000,
            channels: 1,
            bitrate: 32000,
            fec: false,
            dtx: false,
            expected_packet_loss: 5,
            complexity: 10,
        },
        is_offerer: true,
    };

    codec_manager.initialize(&negotiated).await.unwrap();

    // Create samples at 48kHz (what codec actually uses)
    let samples_48k = vec![0.3f32; 960]; // 20ms at 48kHz

    // Encode
    let (_, encoded) = codec_manager.encode(&samples_48k).await.unwrap();

    // Decode
    let decoded = codec_manager
        .decode(CodecType::ADPCM, &encoded)
        .await
        .unwrap();

    // The decoder output rate should match codec config, not packet metadata
    let decoder_rate = codec_manager.decoder_output_sample_rate().await.unwrap();
    assert_eq!(
        decoded.len(),
        960,
        "Decoded samples should be 960 (20ms at 48kHz)"
    );
    assert_eq!(
        decoder_rate, 48000,
        "Decoder should output at configured rate"
    );
}

#[tokio::test]
async fn test_audio_chain_with_correct_sample_rate() {
    // Integration test: verify correct sample rate flows through entire chain
    let manager = AudioManager::new();

    // Start playback (will use device's native rate)
    manager.start_playback(None, 1.0).await.unwrap();

    // Create codec manager
    let codec_manager = Arc::new(CodecManager::new());
    let negotiated = NegotiatedCodec {
        codec: CodecType::ADPCM,
        params: CodecParams {
            sample_rate: 48000,
            channels: 1,
            bitrate: 32000,
            fec: false,
            dtx: false,
            expected_packet_loss: 5,
            complexity: 10,
        },
        is_offerer: true,
    };
    codec_manager.initialize(&negotiated).await.unwrap();

    // Simulate encoding and decoding process
    let original_samples = vec![0.5f32; 960]; // 20ms at 48kHz
    let (codec_type, encoded) = codec_manager.encode(&original_samples).await.unwrap();
    let decoded_samples = codec_manager.decode(codec_type, &encoded).await.unwrap();

    // Get the actual decoder output rate
    let decoder_rate = codec_manager.decoder_output_sample_rate().await.unwrap();

    // Queue the decoded samples with CORRECT rate from decoder
    let result = manager
        .queue_audio_frame(0, decoded_samples.clone(), decoder_rate, 1)
        .await;

    assert!(
        result.is_ok(),
        "Should successfully queue with correct decoder rate: {:?}",
        result.err()
    );

    // Now test INCORRECT scenario (what the bug was)
    // If we use wrong rate, resampler will miscalculate
    let wrong_rate = 44100; // Pretend packet claimed this rate
    let result_wrong = manager
        .queue_audio_frame(1, decoded_samples, wrong_rate, 1)
        .await;

    // This should still work but would cause pitch shift in real playback
    // (we can't easily test for pitch shift without actual audio analysis)
    assert!(
        result_wrong.is_ok(),
        "Should accept frame even with wrong rate (but will cause pitch shift)"
    );

    manager.stop_playback().await.unwrap();
}

#[tokio::test]
async fn test_multiple_codec_rates() {
    // Test that different codec configurations maintain correct sample rates
    let test_rates = vec![48000, 24000, 16000];

    for rate in test_rates {
        let codec_manager = Arc::new(CodecManager::new());

        let negotiated = NegotiatedCodec {
            codec: CodecType::ADPCM,
            params: CodecParams {
                sample_rate: rate,
                channels: 1,
                bitrate: 32000,
                fec: false,
                dtx: false,
                expected_packet_loss: 5,
                complexity: 10,
            },
            is_offerer: true,
        };

        codec_manager.initialize(&negotiated).await.unwrap();

        // Verify decoder reports correct rate
        let decoder_rate = codec_manager.decoder_output_sample_rate().await;
        assert_eq!(
            decoder_rate,
            Some(rate),
            "Decoder should report {} Hz for codec configured at {} Hz",
            rate,
            rate
        );

        // Encode/decode samples
        let samples_per_frame = (rate * 20 / 1000) as usize; // 20ms
        let input_samples = vec![0.4f32; samples_per_frame];
        let (codec_type, encoded) = codec_manager.encode(&input_samples).await.unwrap();
        let decoded = codec_manager.decode(codec_type, &encoded).await.unwrap();

        // Decoded length should match input (same rate)
        assert_eq!(
            decoded.len(),
            input_samples.len(),
            "Decoded samples at {} Hz should match input length",
            rate
        );
    }
}

#[tokio::test]
async fn test_encoder_decoder_rate_consistency() {
    // Verify encoder and decoder use consistent sample rates
    let codec_manager = Arc::new(CodecManager::new());

    let negotiated = NegotiatedCodec {
        codec: CodecType::ADPCM,
        params: CodecParams {
            sample_rate: 48000,
            channels: 1,
            bitrate: 32000,
            fec: false,
            dtx: false,
            expected_packet_loss: 5,
            complexity: 10,
        },
        is_offerer: true,
    };

    codec_manager.initialize(&negotiated).await.unwrap();

    let encoder_rate = codec_manager.encoder_input_sample_rate().await;
    let decoder_rate = codec_manager.decoder_output_sample_rate().await;

    assert_eq!(
        encoder_rate, decoder_rate,
        "Encoder and decoder should use same sample rate"
    );
    assert_eq!(
        encoder_rate,
        Some(48000),
        "Both should be at configured 48000 Hz"
    );
}

#[tokio::test]
async fn test_resampler_with_codec_output() {
    // Test that resampler works correctly with actual codec output
    use ntied::audio::Resampler;

    let codec_manager = Arc::new(CodecManager::new());

    // Initialize codec at 16kHz (will need upsampling to 48kHz for most devices)
    let negotiated = NegotiatedCodec {
        codec: CodecType::ADPCM,
        params: CodecParams {
            sample_rate: 16000,
            channels: 1,
            bitrate: 32000,
            fec: false,
            dtx: false,
            expected_packet_loss: 5,
            complexity: 10,
        },
        is_offerer: true,
    };

    codec_manager.initialize(&negotiated).await.unwrap();

    // Generate input at 16kHz
    let samples_16k = vec![0.5f32; 320]; // 20ms at 16kHz

    // Encode and decode
    let (codec_type, encoded) = codec_manager.encode(&samples_16k).await.unwrap();
    let decoded = codec_manager.decode(codec_type, &encoded).await.unwrap();

    // Get actual decoder output rate
    let decoder_rate = codec_manager.decoder_output_sample_rate().await.unwrap();
    assert_eq!(decoder_rate, 16000, "Decoder outputs at 16kHz");
    assert_eq!(decoded.len(), 320, "Decoded should be 320 samples");

    // Now resample to 48kHz using CORRECT decoder rate
    let mut resampler = Resampler::new(decoder_rate, 48000, 1).unwrap();
    let resampled = resampler.resample(&decoded).unwrap();

    // Should be approximately 3x the input (16k->48k)
    let expected_len = (320 as f32 * 48000.0 / 16000.0) as usize;
    assert!(
        (resampled.len() as i32 - expected_len as i32).abs() <= 2,
        "Resampled length {} should be close to expected {} (3x upsampling)",
        resampled.len(),
        expected_len
    );

    // Now test WRONG scenario - using packet's claimed rate instead of decoder rate
    let wrong_rate = 44100; // Pretend packet claimed this
    let mut wrong_resampler = Resampler::new(wrong_rate, 48000, 1).unwrap();

    // This will produce wrong output length because input is actually 16kHz, not 44.1kHz
    let wrong_resampled = wrong_resampler.resample(&decoded).unwrap();

    // The wrong resampler thinks it's upsampling 44.1k->48k (minimal)
    // but actually has 16k data, so output will be too short
    assert_ne!(
        wrong_resampled.len(),
        resampled.len(),
        "Wrong rate produces different (incorrect) output"
    );
}

#[tokio::test]
async fn test_pitch_preservation_with_codec() {
    // Verify that a tone maintains its pitch through encode/decode/resample chain
    use ntied::audio::Resampler;

    let codec_manager = Arc::new(CodecManager::new());

    // Initialize at 16kHz
    let negotiated = NegotiatedCodec {
        codec: CodecType::ADPCM,
        params: CodecParams {
            sample_rate: 16000,
            channels: 1,
            bitrate: 32000,
            fec: false,
            dtx: false,
            expected_packet_loss: 5,
            complexity: 10,
        },
        is_offerer: true,
    };

    codec_manager.initialize(&negotiated).await.unwrap();

    // Generate 440Hz tone at 16kHz
    let mut input = Vec::with_capacity(320);
    for i in 0..320 {
        let t = i as f32 / 16000.0;
        input.push((2.0 * std::f32::consts::PI * 440.0 * t).sin());
    }

    // Encode and decode
    let (codec_type, encoded) = codec_manager.encode(&input).await.unwrap();
    let decoded = codec_manager.decode(codec_type, &encoded).await.unwrap();

    // Resample to 48kHz using correct decoder rate
    let decoder_rate = codec_manager.decoder_output_sample_rate().await.unwrap();
    let mut resampler = Resampler::new(decoder_rate, 48000, 1).unwrap();
    let resampled = resampler.resample(&decoded).unwrap();

    // Count zero crossings to verify frequency is preserved
    let mut crossings = 0;
    for i in 1..resampled.len() {
        if (resampled[i - 1] < 0.0 && resampled[i] >= 0.0)
            || (resampled[i - 1] >= 0.0 && resampled[i] < 0.0)
        {
            crossings += 1;
        }
    }

    // 320 samples at 16kHz = 20ms
    // After resampling to 48kHz: 960 samples = 20ms
    // 440Hz tone should have ~17-18 crossings in 20ms (440 * 0.02 * 2)
    assert!(
        crossings >= 15 && crossings <= 20,
        "Zero crossings {} should indicate 440Hz tone (expected 15-20 for 20ms)",
        crossings
    );
}
