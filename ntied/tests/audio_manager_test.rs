use ntied::audio::{AudioFrame, AudioManager};
use std::time::{Duration, Instant};

#[tokio::test]
async fn test_audio_manager_creation() {
    let manager = AudioManager::new();
    assert!(!manager.is_capturing().await);
    assert!(!manager.is_playing().await);
}

#[tokio::test]
async fn test_list_devices() {
    let input_devices = AudioManager::list_input_devices().await;
    let output_devices = AudioManager::list_output_devices().await;

    // Just check that the methods don't panic
    if let Ok(devices) = input_devices {
        println!("Found {} input devices", devices.len());
        for device in devices {
            println!("  - {} (default: {})", device.name, device.is_default);
        }
    }

    if let Ok(devices) = output_devices {
        println!("Found {} output devices", devices.len());
        for device in devices {
            println!("  - {} (default: {})", device.name, device.is_default);
        }
    }
}

#[tokio::test]
async fn test_capture_and_playback() {
    let manager = AudioManager::new();

    // Try to start capture (might fail if no device available)
    match manager.start_capture(None, 1.0).await {
        Ok(mut rx) => {
            assert!(manager.is_capturing().await);

            // Try to receive some frames
            let timeout = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await;
            if let Ok(Some(frame)) = timeout {
                println!("Received frame with {} samples", frame.samples.len());

                // Test prepare_audio_frame
                let (seq, _) = manager.prepare_audio_frame(frame).await;
                assert_eq!(seq, 0);

                // Get another sequence number
                let test_frame = AudioFrame {
                    samples: vec![0.0; 100],
                    sample_rate: 48000,
                    channels: 1,
                    timestamp: Instant::now(),
                };
                let (seq2, _) = manager.prepare_audio_frame(test_frame).await;
                assert_eq!(seq2, 1);
            }

            // Stop capture
            assert!(manager.stop_capture().await.is_ok());
            assert!(!manager.is_capturing().await);
        }
        Err(e) => {
            println!("Could not start capture: {}", e);
        }
    }

    // Try to start playback (might fail if no device available)
    match manager.start_playback(None, 1.0).await {
        Ok(_) => {
            assert!(manager.is_playing().await);

            // Test queueing frames with different sequences
            let test_samples = vec![0.0f32; 960]; // 20ms at 48kHz

            // Queue frames out of order to test jitter buffer
            let _ = manager
                .queue_audio_frame(2, test_samples.clone(), 48000, 1)
                .await;
            let _ = manager
                .queue_audio_frame(0, test_samples.clone(), 48000, 1)
                .await;
            let _ = manager
                .queue_audio_frame(1, test_samples.clone(), 48000, 1)
                .await;

            // Give some time for processing
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Check stats
            let stats = manager.get_stats().await;
            println!("JitterBuffer stats: {:?}", stats);
            assert!(stats.packets_received >= 3);

            // Stop playback
            assert!(manager.stop_playback().await.is_ok());
            assert!(!manager.is_playing().await);
        }
        Err(e) => {
            println!("Could not start playback: {}", e);
        }
    }
}

#[tokio::test]
async fn test_volume_and_mute() {
    let manager = AudioManager::new();

    // Test volume controls (even without active streams)
    assert!(manager.set_capture_volume(0.5).await.is_ok());
    assert!(manager.set_capture_volume(1.5).await.is_ok());
    assert!(manager.set_playback_volume(0.8).await.is_ok());

    // Test mute controls
    assert!(manager.set_capture_mute(true).await.is_ok());
    assert!(manager.set_capture_mute(false).await.is_ok());
    assert!(manager.set_playback_mute(true).await.is_ok());
    assert!(manager.set_playback_mute(false).await.is_ok());
}

#[tokio::test]
async fn test_sequence_counter() {
    let manager = AudioManager::new();

    let frame = AudioFrame {
        samples: vec![0.0; 100],
        sample_rate: 48000,
        channels: 1,
        timestamp: Instant::now(),
    };

    // Test sequence counter
    let (seq1, _) = manager.prepare_audio_frame(frame.clone()).await;
    let (seq2, _) = manager.prepare_audio_frame(frame.clone()).await;
    let (seq3, _) = manager.prepare_audio_frame(frame.clone()).await;

    assert_eq!(seq1, 0);
    assert_eq!(seq2, 1);
    assert_eq!(seq3, 2);

    // Reset and test again
    manager.reset_sequence();
    let (seq4, _) = manager.prepare_audio_frame(frame.clone()).await;
    assert_eq!(seq4, 0);
}

#[tokio::test]
async fn test_jitter_buffer_integration() {
    let manager = AudioManager::new();

    // Start playback to enable jitter buffer
    if manager.start_playback(None, 0.5).await.is_ok() {
        let test_samples = vec![0.0f32; 960];

        // Simulate packet loss (skip sequence 1)
        let _ = manager
            .queue_audio_frame(0, test_samples.clone(), 48000, 1)
            .await;
        let _ = manager
            .queue_audio_frame(2, test_samples.clone(), 48000, 1)
            .await;
        let _ = manager
            .queue_audio_frame(3, test_samples.clone(), 48000, 1)
            .await;

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Now send the missing packet (late arrival)
        let _ = manager
            .queue_audio_frame(1, test_samples.clone(), 48000, 1)
            .await;

        // Check stats
        let stats = manager.get_stats().await;
        println!("After packet loss simulation: {:?}", stats);

        // Simulate duplicate packet with a fresh sequence
        let _ = manager
            .queue_audio_frame(4, test_samples.clone(), 48000, 1)
            .await;
        // Send the same sequence again
        let _ = manager
            .queue_audio_frame(4, test_samples.clone(), 48000, 1)
            .await;

        let stats = manager.get_stats().await;
        println!("After duplicate packet: {:?}", stats);
        // The duplicate packet might be counted as 'late' since it was already processed
        assert!(stats.packets_duplicate > 0 || stats.packets_late > 0);

        let _ = manager.stop_playback().await;
    }
}

#[tokio::test]
async fn test_device_switching() {
    let manager = AudioManager::new();

    // Get available devices
    let input_devices = AudioManager::list_input_devices().await.unwrap_or_default();
    let output_devices = AudioManager::list_output_devices()
        .await
        .unwrap_or_default();

    // Test switching input device
    if !input_devices.is_empty() {
        // Start with default device
        if manager.start_capture(None, 1.0).await.is_ok() {
            let default_device = manager.get_current_input_device().await;
            println!("Default input device: {:?}", default_device);

            // If there's another device, try to switch to it
            if input_devices.len() > 1 {
                let other_device = input_devices
                    .iter()
                    .find(|d| !d.is_default)
                    .map(|d| d.name.clone());

                if let Some(device_name) = other_device {
                    println!("Switching to input device: {}", device_name);
                    let _ = manager.stop_capture().await;

                    if manager
                        .start_capture(Some(device_name.clone()), 1.0)
                        .await
                        .is_ok()
                    {
                        let current = manager.get_current_input_device().await;
                        assert_eq!(current, Some(device_name));
                    }
                }
            }

            let _ = manager.stop_capture().await;
        }
    }

    // Test switching output device
    if !output_devices.is_empty() {
        // Start with default device
        if manager.start_playback(None, 1.0).await.is_ok() {
            let default_device = manager.get_current_output_device().await;
            println!("Default output device: {:?}", default_device);

            // If there's another device, try to switch to it
            if output_devices.len() > 1 {
                let other_device = output_devices
                    .iter()
                    .find(|d| !d.is_default)
                    .map(|d| d.name.clone());

                if let Some(device_name) = other_device {
                    println!("Switching to output device: {}", device_name);
                    let _ = manager.stop_playback().await;

                    if manager
                        .start_playback(Some(device_name.clone()), 1.0)
                        .await
                        .is_ok()
                    {
                        let current = manager.get_current_output_device().await;
                        assert_eq!(current, Some(device_name));
                    }
                }
            }

            let _ = manager.stop_playback().await;
        }
    }
}
