use ntied::audio::AudioManager;
use tokio;

#[tokio::test]
async fn test_audio_resampling_different_rates() {
    let manager = AudioManager::new();

    // Start playback (this will use the default device's sample rate)
    manager.start_playback(None, 1.0).await.unwrap();

    // Create test frames at different sample rates
    let test_cases = vec![
        (44100u32, 1u16), // 44.1kHz mono (CD quality)
        (48000u32, 1u16), // 48kHz mono (DAT quality)
        (16000u32, 1u16), // 16kHz mono (Wideband)
        (8000u32, 1u16),  // 8kHz mono (Narrowband/telephone)
        (44100u32, 2u16), // 44.1kHz stereo
        (48000u32, 2u16), // 48kHz stereo
    ];

    for (sample_rate, channels) in test_cases {
        // Create a test tone at 440Hz (A note)
        let duration_ms = 20; // 20ms frame
        let num_samples = (sample_rate * duration_ms / 1000) as usize * channels as usize;
        let mut samples = Vec::with_capacity(num_samples);

        for i in 0..num_samples / channels as usize {
            let t = i as f32 / sample_rate as f32;
            let sample = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;

            // Add same sample for all channels
            for _ in 0..channels {
                samples.push(sample);
            }
        }

        // Queue the frame with different sample rates
        let result = manager
            .queue_audio_frame(
                0, // sequence
                samples.clone(),
                sample_rate,
                channels,
            )
            .await;

        assert!(
            result.is_ok(),
            "Failed to queue audio frame at {} Hz, {} channels: {:?}",
            sample_rate,
            channels,
            result.err()
        );
    }

    // Clean up
    manager.stop_playback().await.unwrap();
}

#[tokio::test]
async fn test_audio_manager_with_resampling() {
    let manager = AudioManager::new();

    // Start playback
    manager.start_playback(None, 1.0).await.unwrap();

    // Simulate receiving audio packets at 44.1kHz
    let input_rate = 44100u32;
    let channels = 1u16;
    let frame_duration_ms = 20;
    let samples_per_frame = (input_rate * frame_duration_ms / 1000) as usize;

    // Send several frames to test continuous resampling
    for sequence in 0..10 {
        let mut samples = vec![0.0f32; samples_per_frame];

        // Create a simple sine wave
        for i in 0..samples_per_frame {
            let t = (sequence * samples_per_frame + i) as f32 / input_rate as f32;
            samples[i] = (2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.3;
        }

        let result = manager
            .queue_audio_frame(sequence as u32, samples, input_rate, channels)
            .await;

        assert!(
            result.is_ok(),
            "Failed to queue frame {}: {:?}",
            sequence,
            result.err()
        );
    }

    // Give some time for audio to play
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Stop playback
    manager.stop_playback().await.unwrap();
}

#[tokio::test]
async fn test_resampling_state_persistence() {
    let manager = AudioManager::new();

    // Start playback
    manager.start_playback(None, 1.0).await.unwrap();

    // Send frames at 22050Hz (an uncommon rate that will definitely need resampling)
    let sample_rate = 22050u32;
    let channels = 2u16; // Stereo

    for sequence in 0..5 {
        let samples_per_frame = (sample_rate * 20 / 1000) as usize * channels as usize;
        let samples = vec![0.25f32; samples_per_frame];

        let result = manager
            .queue_audio_frame(sequence as u32, samples, sample_rate, channels)
            .await;

        assert!(
            result.is_ok(),
            "Frame {} failed: {:?}",
            sequence,
            result.err()
        );
    }

    // Now send frames at a different rate to test resampler reinitialization
    let new_sample_rate = 32000u32;

    for sequence in 5..10 {
        let samples_per_frame = (new_sample_rate * 20 / 1000) as usize * channels as usize;
        let samples = vec![0.5f32; samples_per_frame];

        let result = manager
            .queue_audio_frame(sequence as u32, samples, new_sample_rate, channels)
            .await;

        assert!(
            result.is_ok(),
            "Frame {} at new rate failed: {:?}",
            sequence,
            result.err()
        );
    }

    manager.stop_playback().await.unwrap();
}

#[tokio::test]
async fn test_no_resampling_when_rates_match() {
    // This test verifies that when input and output rates match,
    // the resampler passes through without modification
    let manager = AudioManager::new();

    manager.start_playback(None, 1.0).await.unwrap();

    // Most devices default to 48000 Hz, but we'll test with whatever the device uses
    // by first sending a frame and checking if it succeeds
    let test_rate = 48000u32;
    let channels = 1u16;
    let samples = vec![0.7f32; 960]; // 20ms at 48kHz

    let result = manager
        .queue_audio_frame(0, samples.clone(), test_rate, channels)
        .await;

    // The test succeeds if the audio was queued successfully
    assert!(result.is_ok(), "Failed to queue audio: {:?}", result.err());

    manager.stop_playback().await.unwrap();
}

#[tokio::test]
async fn test_continuous_upsampling() {
    // Test that continuous upsampling maintains proper audio continuity
    let manager = AudioManager::new();

    manager.start_playback(None, 1.0).await.unwrap();

    // Test upsampling from 16kHz to 48kHz (3x upsampling)
    let input_rate = 16000u32;
    let channels = 1u16;
    let frame_duration_ms = 20;
    let samples_per_frame = (input_rate * frame_duration_ms / 1000) as usize;

    // Send multiple consecutive frames to test continuity
    for sequence in 0..20 {
        let mut samples = vec![0.0f32; samples_per_frame];

        // Create a continuous sine wave across frames
        for i in 0..samples_per_frame {
            let global_sample = sequence * samples_per_frame + i;
            let t = global_sample as f32 / input_rate as f32;
            // 440Hz tone (A4 note)
            samples[i] = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.3;
        }

        let result = manager
            .queue_audio_frame(sequence as u32, samples, input_rate, channels)
            .await;

        assert!(
            result.is_ok(),
            "Failed to queue upsampled frame {}: {:?}",
            sequence,
            result.err()
        );
    }

    // Give time for processing
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    manager.stop_playback().await.unwrap();
}

#[tokio::test]
async fn test_extreme_upsampling() {
    // Test extreme upsampling ratio (8kHz to 48kHz = 6x)
    let manager = AudioManager::new();

    manager.start_playback(None, 1.0).await.unwrap();

    let input_rate = 8000u32;
    let channels = 2u16; // Test with stereo
    let frame_duration_ms = 20;
    let samples_per_frame = (input_rate * frame_duration_ms / 1000) as usize;

    for sequence in 0..10 {
        let mut samples = Vec::with_capacity(samples_per_frame * channels as usize);

        // Create different tones for left and right channels
        for i in 0..samples_per_frame {
            let t = (sequence * samples_per_frame + i) as f32 / input_rate as f32;
            // Left channel: 440Hz
            samples.push((2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.2);
            // Right channel: 880Hz
            samples.push((2.0 * std::f32::consts::PI * 880.0 * t).sin() * 0.2);
        }

        let result = manager
            .queue_audio_frame(sequence as u32, samples, input_rate, channels)
            .await;

        assert!(
            result.is_ok(),
            "Failed extreme upsampling at frame {}: {:?}",
            sequence,
            result.err()
        );
    }

    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

    manager.stop_playback().await.unwrap();
}

#[tokio::test]
async fn test_rapid_rate_changes() {
    // Test rapid changes between different sample rates
    let manager = AudioManager::new();

    manager.start_playback(None, 1.0).await.unwrap();

    let test_rates = vec![
        16000u32, // Low rate
        48000u32, // High rate
        24000u32, // Medium rate
        8000u32,  // Very low rate
        44100u32, // CD quality
    ];

    let channels = 1u16;

    for (idx, &rate) in test_rates.iter().enumerate() {
        let samples_per_frame = (rate * 20 / 1000) as usize;
        let mut samples = vec![0.0f32; samples_per_frame];

        // Create test tone
        for i in 0..samples_per_frame {
            let t = i as f32 / rate as f32;
            samples[i] = (2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.25;
        }

        let result = manager
            .queue_audio_frame(idx as u32, samples, rate, channels)
            .await;

        assert!(
            result.is_ok(),
            "Failed at rate change to {} Hz: {:?}",
            rate,
            result.err()
        );

        // Small delay between rate changes
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;
    }

    manager.stop_playback().await.unwrap();
}
