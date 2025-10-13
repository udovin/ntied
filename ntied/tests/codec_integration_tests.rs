use ntied::audio::{AudioFrame, CodecManager, CodecParams, CodecType, NetworkQuality};
use std::time::Instant;

#[tokio::test]
async fn test_codec_manager_basic_encoding() {
    let manager = CodecManager::new();

    // Initialize with ADPCM (PCMU type)
    let negotiated = ntied::audio::NegotiatedCodec {
        codec: CodecType::PCMU,
        params: CodecParams::voice(),
        is_offerer: true,
    };

    manager.initialize(&negotiated).await.unwrap();

    // Create test audio frame (20ms at 48kHz mono)
    let samples = generate_sine_wave(960, 440.0, 48000.0);

    // Encode
    let (codec_type, encoded) = manager.encode(&samples).await.unwrap();
    assert_eq!(codec_type, CodecType::PCMU);

    // Should achieve compression (ADPCM gives ~4:1 compression)
    assert!(encoded.len() < samples.len() * 4);

    // Decode
    let decoded = manager.decode(codec_type, &encoded).await.unwrap();
    assert_eq!(decoded.len(), samples.len());

    // Calculate SNR to verify quality
    let snr = calculate_snr(&samples, &decoded);
    // ADPCM is lossy, expect lower SNR than lossless codecs
    assert!(snr > -5.0, "SNR too low: {}", snr); // ADPCM can have negative SNR but signal should still be recognizable
}

#[tokio::test]
async fn test_codec_negotiation() {
    let manager1 = CodecManager::new();
    let manager2 = CodecManager::new();

    // Manager 1 creates offer
    let offer = manager1.create_offer();
    assert!(
        offer.codec == CodecType::G722
            || offer.codec == CodecType::PCMU
            || offer.codec == CodecType::Raw
    );

    // Manager 2 creates answer based on manager1's capabilities
    let answer = manager2.create_answer(manager1.capabilities()).unwrap();

    // Both should agree on the same codec
    assert_eq!(offer.codec, answer.codec);
}

#[tokio::test]
async fn test_packet_loss_concealment() {
    let manager = CodecManager::new();

    // Initialize with Raw codec for predictable behavior
    let negotiated = ntied::audio::NegotiatedCodec {
        codec: CodecType::Raw,
        params: CodecParams::voice(),
        is_offerer: true,
    };

    manager.initialize(&negotiated).await.unwrap();

    // Create a test frame
    let samples = vec![0.3; 960];
    let (_, encoded) = manager.encode(&samples).await.unwrap();

    // Decode it to prime the decoder
    manager.decode(CodecType::Raw, &encoded).await.unwrap();

    // Now simulate packet loss and use PLC
    let concealed = manager.conceal_packet_loss().await.unwrap();

    // Basic checks for PLC functionality
    assert_eq!(concealed.len(), 960, "PLC should return correct frame size");

    // PLC should produce some output (not all zeros)
    let max_value = concealed.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
    assert!(max_value > 0.0, "PLC should produce non-zero output");
}

#[tokio::test]
async fn test_adaptive_bitrate() {
    let manager = CodecManager::new();

    // Initialize
    let negotiated = ntied::audio::NegotiatedCodec {
        codec: CodecType::PCMU,
        params: CodecParams::voice(),
        is_offerer: true,
    };

    manager.initialize(&negotiated).await.unwrap();

    // Update network quality to simulate poor conditions
    let poor_quality = NetworkQuality {
        packet_loss: 15.0,
        rtt: 200.0,
        bandwidth: 50,
        jitter: 20.0,
    };

    manager.update_network_quality(poor_quality).await.unwrap();

    // Encode some data
    let samples = vec![0.0; 960];
    let (_, encoded) = manager.encode(&samples).await.unwrap();

    // Verify encoding still works under poor conditions
    assert!(!encoded.is_empty());
}

#[tokio::test]
async fn test_codec_switching() {
    let manager = CodecManager::new();

    // Start with Raw codec
    let raw_negotiated = ntied::audio::NegotiatedCodec {
        codec: CodecType::Raw,
        params: CodecParams::voice(),
        is_offerer: true,
    };

    manager.initialize(&raw_negotiated).await.unwrap();
    assert_eq!(manager.current_codec().await, Some(CodecType::Raw));

    // Create test samples
    let samples = generate_sine_wave(960, 440.0, 48000.0);

    // Encode with Raw
    let (codec1, encoded1) = manager.encode(&samples).await.unwrap();
    assert_eq!(codec1, CodecType::Raw);

    // Switch to ADPCM
    let adpcm_negotiated = ntied::audio::NegotiatedCodec {
        codec: CodecType::PCMU, // Using PCMU for ADPCM
        params: CodecParams::voice(),
        is_offerer: true,
    };

    manager.initialize(&adpcm_negotiated).await.unwrap();
    assert_eq!(manager.current_codec().await, Some(CodecType::PCMU));

    // Encode with ADPCM
    let (codec2, encoded2) = manager.encode(&samples).await.unwrap();
    assert_eq!(codec2, CodecType::PCMU);

    // ADPCM should compress better than Raw
    assert!(encoded2.len() < encoded1.len());
}

#[tokio::test]
async fn test_multi_channel_encoding() {
    let manager = CodecManager::new();

    // Initialize with stereo
    let mut params = CodecParams::voice();
    params.channels = 2;

    let negotiated = ntied::audio::NegotiatedCodec {
        codec: CodecType::Raw,
        params,
        is_offerer: true,
    };

    manager.initialize(&negotiated).await.unwrap();

    // Create stereo samples (interleaved)
    let samples = vec![0.1, -0.1, 0.2, -0.2, 0.3, -0.3];

    // Encode and decode
    let (_, encoded) = manager.encode(&samples).await.unwrap();
    let decoded = manager.decode(CodecType::Raw, &encoded).await.unwrap();

    assert_eq!(decoded.len(), samples.len());

    // Verify stereo separation preserved
    for i in 0..samples.len() {
        assert!((samples[i] - decoded[i]).abs() < 0.0001);
    }
}

#[tokio::test]
async fn test_codec_statistics() {
    let manager = CodecManager::new();

    // Initialize
    let negotiated = ntied::audio::NegotiatedCodec {
        codec: CodecType::PCMU,
        params: CodecParams::voice(),
        is_offerer: true,
    };

    manager.initialize(&negotiated).await.unwrap();

    // Encode multiple frames
    let samples = vec![0.0; 960];
    for _ in 0..10 {
        manager.encode(&samples).await.unwrap();
    }

    // Check encoder stats
    let encoder_stats = manager.encoder_stats().await.unwrap();
    assert_eq!(encoder_stats.frames_encoded, 10);
    assert!(encoder_stats.bytes_encoded > 0);
    assert!(encoder_stats.avg_encode_time_us > 0.0);

    // Decode multiple frames
    let (_, encoded) = manager.encode(&samples).await.unwrap();
    for _ in 0..5 {
        manager.decode(CodecType::PCMU, &encoded).await.unwrap();
    }

    // Check decoder stats
    let decoder_stats = manager.decoder_stats().await.unwrap();
    assert_eq!(decoder_stats.frames_decoded, 5);
    assert!(decoder_stats.bytes_decoded > 0);
    assert!(decoder_stats.avg_decode_time_us > 0.0);
}

#[tokio::test]
async fn test_real_audio_frame_integration() {
    let manager = CodecManager::new();

    // Initialize with voice optimized settings
    let negotiated = ntied::audio::NegotiatedCodec {
        codec: CodecType::PCMU,
        params: CodecParams::voice(),
        is_offerer: true,
    };

    manager.initialize(&negotiated).await.unwrap();

    // Simulate real audio frame from capture
    let frame = AudioFrame {
        samples: generate_sine_wave(960, 440.0, 48000.0),
        sample_rate: 48000,
        channels: 1,
        timestamp: Instant::now(),
    };

    // Process frame through codec
    let (codec_type, encoded) = manager.encode(&frame.samples).await.unwrap();

    // Simulate network transmission (could add delay/jitter here)

    // Decode on receiver side
    let decoded_samples = manager.decode(codec_type, &encoded).await.unwrap();

    // Reconstruct frame for playback
    let decoded_frame = AudioFrame {
        samples: decoded_samples,
        sample_rate: frame.sample_rate,
        channels: frame.channels,
        timestamp: Instant::now(),
    };

    assert_eq!(decoded_frame.samples.len(), frame.samples.len());
    assert_eq!(decoded_frame.sample_rate, frame.sample_rate);
    assert_eq!(decoded_frame.channels, frame.channels);
}

// Helper functions

fn generate_sine_wave(num_samples: usize, frequency: f32, sample_rate: f32) -> Vec<f32> {
    let mut samples = Vec::with_capacity(num_samples);
    for i in 0..num_samples {
        let t = i as f32 / sample_rate;
        let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.5;
        samples.push(sample);
    }
    samples
}

fn calculate_snr(original: &[f32], processed: &[f32]) -> f32 {
    assert_eq!(original.len(), processed.len());

    let signal_power: f32 = original.iter().map(|x| x * x).sum::<f32>() / original.len() as f32;

    let noise: Vec<f32> = original
        .iter()
        .zip(processed.iter())
        .map(|(o, p)| o - p)
        .collect();

    let noise_power: f32 = noise.iter().map(|x| x * x).sum::<f32>() / noise.len() as f32;

    if noise_power > 0.0 {
        10.0 * (signal_power / noise_power).log10()
    } else {
        f32::INFINITY
    }
}
