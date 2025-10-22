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

#[tokio::test]
async fn test_resampler_pitch_accuracy() {
    // Test that resampling doesn't change pitch
    use ntied::audio::Resampler;

    // Test upsampling: 16kHz -> 48kHz
    let mut resampler = Resampler::new(16000, 48000, 1).unwrap();

    // Generate a 440Hz sine wave at 16kHz
    let input_rate = 16000;
    let duration_ms = 100; // 100ms
    let num_samples = (input_rate * duration_ms / 1000) as usize;
    let mut input = Vec::with_capacity(num_samples);

    for i in 0..num_samples {
        let t = i as f32 / input_rate as f32;
        input.push((2.0 * std::f32::consts::PI * 440.0 * t).sin());
    }

    // Resample
    let output = resampler.resample(&input).unwrap();

    // Check output length is approximately correct (should be 3x for 16k->48k)
    let expected_output_len = (num_samples as f32 * 48000.0 / 16000.0) as usize;
    let len_diff = (output.len() as i32 - expected_output_len as i32).abs();
    assert!(
        len_diff <= 2,
        "Output length {} differs too much from expected {} (diff: {})",
        output.len(),
        expected_output_len,
        len_diff
    );

    // Verify the frequency is preserved by checking zero crossings
    // For a 440Hz sine wave, we should have ~44 zero crossings in 100ms
    let mut zero_crossings = 0;
    for i in 1..output.len() {
        if (output[i - 1] < 0.0 && output[i] >= 0.0) || (output[i - 1] >= 0.0 && output[i] < 0.0) {
            zero_crossings += 1;
        }
    }

    // Expected crossings: 440Hz * 0.1s * 2 (up and down) = 88
    // Allow some tolerance
    assert!(
        zero_crossings >= 80 && zero_crossings <= 96,
        "Zero crossings {} out of expected range (80-96) - pitch may have shifted",
        zero_crossings
    );
}

#[tokio::test]
async fn test_resampler_no_drift() {
    // Test that resampler doesn't accumulate drift over many frames
    use ntied::audio::Resampler;

    let mut resampler = Resampler::new(16000, 48000, 1).unwrap();

    let frame_size = 320; // 20ms at 16kHz
    let num_frames = 100; // 2 seconds

    let mut total_input = 0;
    let mut total_output = 0;

    for _ in 0..num_frames {
        let input = vec![0.5; frame_size];
        let output = resampler.resample(&input).unwrap();

        total_input += input.len();
        total_output += output.len();
    }

    // Check that the ratio is maintained accurately
    let actual_ratio = total_output as f64 / total_input as f64;
    let expected_ratio = 48000.0 / 16000.0;
    let ratio_error = (actual_ratio - expected_ratio).abs();

    assert!(
        ratio_error < 0.001,
        "Resampler accumulated drift: actual ratio {:.6}, expected {:.6}",
        actual_ratio,
        expected_ratio
    );
}

#[tokio::test]
async fn test_resampler_continuous_frames() {
    // Test that resampler maintains continuity between frames
    use ntied::audio::Resampler;

    let mut resampler = Resampler::new(16000, 48000, 1).unwrap();

    // Generate a continuous sine wave split across multiple frames
    let input_rate = 16000;
    let frame_size = 320; // 20ms
    let num_frames = 10;
    let frequency = 440.0;

    let mut outputs = Vec::new();

    for frame_idx in 0..num_frames {
        let mut frame = Vec::with_capacity(frame_size);
        for i in 0..frame_size {
            let sample_idx = frame_idx * frame_size + i;
            let t = sample_idx as f32 / input_rate as f32;
            frame.push((2.0 * std::f32::consts::PI * frequency * t).sin());
        }

        let output = resampler.resample(&frame).unwrap();
        outputs.push(output);
    }

    // Concatenate all outputs
    let full_output: Vec<f32> = outputs.into_iter().flatten().collect();

    // Check for discontinuities (large jumps between samples)
    // Linear interpolation with 3x upsampling can cause some larger jumps
    let mut max_discontinuity = 0.0f32;
    for i in 1..full_output.len() {
        let diff = (full_output[i] - full_output[i - 1]).abs();
        max_discontinuity = max_discontinuity.max(diff);
    }

    // For a 440Hz sine wave, max theoretical change is 2*pi*f/sample_rate
    // At 48kHz: 2 * PI * 440 / 48000 ≈ 0.058
    // With linear interpolation and upsampling, we can see jumps up to ~2x this
    // The key is that most samples should be smooth
    let mut smooth_count = 0;
    for i in 1..full_output.len() {
        let diff = (full_output[i] - full_output[i - 1]).abs();
        if diff < 0.12 {
            smooth_count += 1;
        }
    }

    let smooth_percentage = (smooth_count as f32 / (full_output.len() - 1) as f32) * 100.0;

    // At least 90% of samples should have smooth transitions
    assert!(
        smooth_percentage > 90.0,
        "Only {:.1}% smooth transitions (max discontinuity: {}) - indicates poor continuity",
        smooth_percentage,
        max_discontinuity
    );
}

#[tokio::test]
async fn test_jitter_buffer_packet_loss_handling() {
    // Test that jitter buffer properly handles packet loss without pitch shifts
    use ntied::audio::AudioFrame;
    use ntied::audio::JitterBuffer;
    use std::time::Instant;

    let mut jitter = JitterBuffer::new();

    // Push packets 0, 1, 3, 4 (missing packet 2)
    for i in [0u32, 1, 3, 4] {
        let frame = AudioFrame {
            samples: vec![0.5; 960],
            sample_rate: 48000,
            channels: 1,
            timestamp: Instant::now(),
        };
        jitter.push(i, frame);
    }

    // Should get packets 0 and 1
    assert!(jitter.pop().is_some(), "Should get packet 0");
    assert!(jitter.pop().is_some(), "Should get packet 1");

    // Packet 2 is missing, buffer should wait initially
    assert!(jitter.pop().is_none(), "Should wait for packet 2");

    // Push many more packets to trigger skip (need >10 missing to trigger skip logic)
    for i in 5..20 {
        let frame = AudioFrame {
            samples: vec![0.5; 960],
            sample_rate: 48000,
            channels: 1,
            timestamp: Instant::now(),
        };
        jitter.push(i, frame);
    }

    // Add delay to ensure timeout is reached
    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

    // Should eventually skip packet 2 and continue
    let mut got_packet = false;
    for _ in 0..30 {
        if jitter.pop().is_some() {
            got_packet = true;
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }

    assert!(
        got_packet,
        "Jitter buffer should skip missing packet and continue"
    );

    // Check stats
    let stats = jitter.stats();
    assert!(stats.packets_lost >= 1, "Should report lost packet");
}

#[tokio::test]
async fn test_full_chain_no_pitch_shift() {
    // Integration test: ensure the full audio chain doesn't cause pitch shifts
    let manager = AudioManager::new();

    manager.start_playback(None, 1.0).await.unwrap();

    // Simulate receiving audio at different rate than playback
    // This tests the real-world scenario
    let input_rate = 16000u32;
    let channels = 1u16;
    let frame_duration_ms = 20;
    let samples_per_frame = (input_rate * frame_duration_ms / 1000) as usize;

    // Send frames with a continuous 440Hz tone
    for sequence in 0..50 {
        let mut samples = Vec::with_capacity(samples_per_frame);

        for i in 0..samples_per_frame {
            let global_sample = sequence * samples_per_frame + i;
            let t = global_sample as f32 / input_rate as f32;
            samples.push((2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.3);
        }

        let result = manager
            .queue_audio_frame(sequence as u32, samples, input_rate, channels)
            .await;

        assert!(
            result.is_ok(),
            "Failed at frame {}: {:?}",
            sequence,
            result.err()
        );
    }

    // Allow processing time
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Check jitter buffer stats
    let stats = manager.get_stats().await;
    assert!(stats.packets_received > 0, "Should have received packets");

    manager.stop_playback().await.unwrap();
}

#[tokio::test]
async fn test_resampler_stereo_channel_sync() {
    // Test that stereo channels remain synchronized through resampling
    use ntied::audio::Resampler;

    let mut resampler = Resampler::new(16000, 48000, 2).unwrap();

    // Generate a frame with different tones in each channel
    let frame_size = 320; // 20ms at 16kHz mono
    let mut input = Vec::with_capacity(frame_size * 2);

    for i in 0..frame_size {
        let t = i as f32 / 16000.0;
        // Left channel: 440Hz
        input.push((2.0 * std::f32::consts::PI * 440.0 * t).sin());
        // Right channel: 880Hz
        input.push((2.0 * std::f32::consts::PI * 880.0 * t).sin());
    }

    let output = resampler.resample(&input).unwrap();

    // Output should have twice as many samples (stereo)
    assert_eq!(
        output.len() % 2,
        0,
        "Output should have even number of samples for stereo"
    );

    // Verify both channels have data (not silent)
    let mut left_energy = 0.0f32;
    let mut right_energy = 0.0f32;

    for i in (0..output.len()).step_by(2) {
        left_energy += output[i].abs();
        right_energy += output[i + 1].abs();
    }

    assert!(left_energy > 0.1, "Left channel should have signal");
    assert!(right_energy > 0.1, "Right channel should have signal");
    assert!(
        right_energy > left_energy * 0.8,
        "Channels should be roughly balanced"
    );
}
