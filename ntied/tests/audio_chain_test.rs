use ntied::audio::{AudioManager, JitterBuffer, Resampler};
use tokio;

#[tokio::test]
async fn test_full_audio_chain() {
    // Test the full audio processing chain with different scenarios
    let manager = AudioManager::new();

    // Start playback
    manager.start_playback(None, 1.0).await.unwrap();

    // Simulate various network conditions and sample rates
    let test_scenarios = vec![
        // (sample_rate, channels, simulate_packet_loss, description)
        (48000u32, 1u16, false, "Normal 48kHz mono"),
        (44100u32, 2u16, false, "CD quality stereo"),
        (16000u32, 1u16, false, "Wideband mono"),
        (8000u32, 1u16, false, "Narrowband telephone"),
        (48000u32, 1u16, true, "48kHz with packet loss"),
        (16000u32, 2u16, true, "16kHz stereo with packet loss"),
    ];

    for (sample_rate, channels, simulate_loss, description) in test_scenarios {
        println!("Testing: {}", description);

        let mut sequence = 0u32;
        let frame_duration_ms = 20;
        let samples_per_frame = (sample_rate * frame_duration_ms / 1000) as usize;

        // Send 50 frames (1 second of audio)
        for frame_idx in 0..50 {
            // Simulate packet loss on some frames
            if simulate_loss && frame_idx % 7 == 3 {
                // Skip this frame to simulate packet loss
                sequence += 1;
                continue;
            }

            // Generate test audio (sine wave)
            let mut samples = Vec::with_capacity(samples_per_frame * channels as usize);
            for i in 0..samples_per_frame {
                let t = (frame_idx * samples_per_frame + i) as f32 / sample_rate as f32;
                let sample = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.3;

                // Add same sample for all channels
                for _ in 0..channels {
                    samples.push(sample);
                }
            }

            // Queue the audio frame
            let result = manager
                .queue_audio_frame(sequence, samples, sample_rate, channels)
                .await;

            assert!(
                result.is_ok(),
                "Failed to queue frame in scenario '{}': {:?}",
                description,
                result.err()
            );

            sequence += 1;

            // Simulate network jitter by adding random delays
            if frame_idx % 5 == 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
            }
        }

        // Allow time for processing
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Check statistics
        let stats = manager.get_stats().await;
        println!(
            "  Stats for '{}': received={}, lost={}, late={}, jitter={:.2}ms",
            description,
            stats.packets_received,
            stats.packets_lost,
            stats.packets_late,
            stats.average_jitter_ms
        );

        if simulate_loss {
            assert!(
                stats.packets_lost > 0,
                "Should have detected packet loss in '{}'",
                description
            );
        }
    }

    manager.stop_playback().await.unwrap();
}

#[tokio::test]
async fn test_audio_chain_with_rate_switching() {
    // Test switching between different sample rates during a call
    let manager = AudioManager::new();

    manager.start_playback(None, 1.0).await.unwrap();

    // Simulate a call where sample rate changes (e.g., codec renegotiation)
    let rate_sequence = vec![
        (48000u32, 10), // Start with 48kHz for 10 frames
        (16000u32, 10), // Switch to 16kHz
        (48000u32, 10), // Back to 48kHz
        (8000u32, 10),  // Drop to 8kHz
        (44100u32, 10), // Switch to CD quality
    ];

    let mut sequence = 0u32;

    for (sample_rate, num_frames) in rate_sequence {
        println!("Switching to {} Hz", sample_rate);

        for _ in 0..num_frames {
            let samples_per_frame = (sample_rate * 20 / 1000) as usize;
            let samples = vec![0.2f32; samples_per_frame];

            let result = manager
                .queue_audio_frame(sequence, samples, sample_rate, 1)
                .await;

            assert!(
                result.is_ok(),
                "Failed at rate {} Hz: {:?}",
                sample_rate,
                result.err()
            );

            sequence += 1;
        }
    }

    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    manager.stop_playback().await.unwrap();
}

#[tokio::test]
async fn test_audio_chain_burst_handling() {
    // Test handling of burst packets (multiple packets arriving at once)
    let manager = AudioManager::new();

    manager.start_playback(None, 1.0).await.unwrap();

    let sample_rate = 24000u32;
    let samples_per_frame = (sample_rate * 20 / 1000) as usize;

    // Send packets in bursts
    for burst_idx in 0..5 {
        println!("Sending burst {}", burst_idx);

        // Send 10 packets at once
        for i in 0..10 {
            let sequence = (burst_idx * 10 + i) as u32;
            let samples = vec![0.15f32; samples_per_frame];

            let result = manager
                .queue_audio_frame(sequence, samples, sample_rate, 1)
                .await;

            assert!(
                result.is_ok(),
                "Failed in burst {} packet {}: {:?}",
                burst_idx,
                i,
                result.err()
            );
        }

        // Wait before next burst
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
    }

    // Check that jitter buffer handled the bursts
    let stats = manager.get_stats().await;
    println!(
        "Burst handling stats: received={}, lost={}, out_of_order={}",
        stats.packets_received, stats.packets_lost, stats.packets_out_of_order
    );

    manager.stop_playback().await.unwrap();
}

#[tokio::test]
async fn test_audio_chain_out_of_order() {
    // Test handling of out-of-order packet delivery
    let manager = AudioManager::new();

    manager.start_playback(None, 1.0).await.unwrap();

    let sample_rate = 48000u32;
    let samples_per_frame = (sample_rate * 20 / 1000) as usize;

    // Send packets in deliberately wrong order
    let packet_order = vec![0, 2, 1, 4, 3, 6, 5, 8, 7, 9, 11, 10, 13, 12, 14];

    for &sequence in &packet_order {
        let samples = vec![0.25f32; samples_per_frame];

        let result = manager
            .queue_audio_frame(sequence, samples, sample_rate, 1)
            .await;

        assert!(
            result.is_ok(),
            "Failed with out-of-order sequence {}: {:?}",
            sequence,
            result.err()
        );

        // Small delay to simulate network timing
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Give time for reordering
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    let stats = manager.get_stats().await;
    println!(
        "Out-of-order handling: received={}, out_of_order={}",
        stats.packets_received, stats.packets_out_of_order
    );

    assert!(
        stats.packets_out_of_order > 0,
        "Should have detected out-of-order packets"
    );

    manager.stop_playback().await.unwrap();
}

#[tokio::test]
async fn test_audio_chain_stress() {
    // Stress test with rapid operations
    let manager = AudioManager::new();

    // Rapid start/stop cycles
    for cycle in 0..3 {
        println!("Stress test cycle {}", cycle);

        manager.start_playback(None, 1.0).await.unwrap();

        // Send frames with varying parameters
        for i in 0..20 {
            let sample_rate = if i % 2 == 0 { 48000 } else { 16000 };
            let channels = if i % 3 == 0 { 2 } else { 1 };
            let samples_per_frame = (sample_rate * 20 / 1000) as usize * channels as usize;
            let samples = vec![0.1f32; samples_per_frame];

            let _ = manager
                .queue_audio_frame(i as u32, samples, sample_rate, channels)
                .await;
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        manager.stop_playback().await.unwrap();

        // Brief pause between cycles
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn test_audio_chain_volume_changes() {
    // Test volume changes during playback
    let manager = AudioManager::new();

    manager.start_playback(None, 1.0).await.unwrap();

    let sample_rate = 48000u32;
    let samples_per_frame = (sample_rate * 20 / 1000) as usize;

    // Test different volume levels
    let volume_levels = vec![0.0, 0.5, 1.0, 1.5, 2.0, 0.1];

    for (idx, &volume) in volume_levels.iter().enumerate() {
        println!("Setting volume to {}", volume);
        manager.set_playback_volume(volume).await.unwrap();

        // Send a few frames at this volume
        for i in 0..5 {
            let sequence = (idx * 5 + i) as u32;
            let samples = vec![0.5f32; samples_per_frame]; // Full amplitude to test volume

            let result = manager
                .queue_audio_frame(sequence, samples, sample_rate, 1)
                .await;

            assert!(
                result.is_ok(),
                "Failed at volume {}: {:?}",
                volume,
                result.err()
            );
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }

    manager.stop_playback().await.unwrap();
}
