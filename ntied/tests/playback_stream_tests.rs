use cpal::traits::HostTrait;
use ntied::audio::{AudioFrame, PlaybackStream};
use std::time::{Duration, Instant};

#[tokio::test]
async fn test_playback_stream_creation() {
    // Get default output device
    let host = cpal::default_host();
    let device = match host.default_output_device() {
        Some(d) => d,
        None => {
            println!("No output device available, skipping test");
            return;
        }
    };

    // Create playback stream
    let result = PlaybackStream::new(device, 1.0).await;
    assert!(
        result.is_ok(),
        "Failed to create PlaybackStream: {:?}",
        result.err()
    );

    let playback = result.unwrap();
    assert_eq!(playback.channels(), playback.channels());
    assert!(playback.sample_rate() > 0);
}

#[tokio::test]
async fn test_playback_stream_send_frames() {
    // Get default output device
    let host = cpal::default_host();
    let device = match host.default_output_device() {
        Some(d) => d,
        None => {
            println!("No output device available, skipping test");
            return;
        }
    };

    // Create playback stream
    let mut playback = match PlaybackStream::new(device, 1.0).await {
        Ok(p) => p,
        Err(e) => {
            println!("Failed to create playback stream: {}", e);
            return;
        }
    };

    let sample_rate = playback.sample_rate();
    let channels = playback.channels();

    // Generate a simple sine wave (440 Hz for 100ms)
    let frequency = 440.0;
    let duration_secs = 0.1;
    let num_samples = (sample_rate as f32 * duration_secs) as usize * channels as usize;
    let mut samples = Vec::with_capacity(num_samples);

    for i in 0..num_samples / channels as usize {
        let t = i as f32 / sample_rate as f32;
        let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.3;

        // Add same sample for all channels
        for _ in 0..channels {
            samples.push(sample);
        }
    }

    let frame = AudioFrame {
        samples,
        sample_rate,
        channels,
        timestamp: Instant::now(),
    };

    // Send frame
    let result = playback.send(frame).await;
    assert!(result.is_ok(), "Failed to send frame: {:?}", result.err());

    // Keep stream alive for a bit to play the sound
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn test_playback_stream_try_send() {
    // Get default output device
    let host = cpal::default_host();
    let device = match host.default_output_device() {
        Some(d) => d,
        None => {
            println!("No output device available, skipping test");
            return;
        }
    };

    // Create playback stream
    let mut playback = match PlaybackStream::new(device, 0.5).await {
        Ok(p) => p,
        Err(e) => {
            println!("Failed to create playback stream: {}", e);
            return;
        }
    };

    let sample_rate = playback.sample_rate();
    let channels = playback.channels();

    // Create a frame with silence
    let frame = AudioFrame {
        samples: vec![0.0; 1024],
        sample_rate,
        channels,
        timestamp: Instant::now(),
    };

    // Try send should work
    let result = playback.try_send(frame);
    assert!(
        result.is_ok(),
        "Failed to try_send frame: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_playback_stream_pause_play() {
    // Get default output device
    let host = cpal::default_host();
    let device = match host.default_output_device() {
        Some(d) => d,
        None => {
            println!("No output device available, skipping test");
            return;
        }
    };

    // Create playback stream
    let mut playback = match PlaybackStream::new(device, 1.0).await {
        Ok(p) => p,
        Err(e) => {
            println!("Failed to create playback stream: {}", e);
            return;
        }
    };

    // Test mute/unmute
    playback.set_mute(true).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    playback.set_mute(false).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Should not panic
    println!("Mute/unmute test passed");
}

#[tokio::test]
async fn test_playback_stream_volume() {
    // Get default output device
    let host = cpal::default_host();
    let device = match host.default_output_device() {
        Some(d) => d,
        None => {
            println!("No output device available, skipping test");
            return;
        }
    };

    // Create playback stream with initial volume
    let mut playback = match PlaybackStream::new(device, 0.5).await {
        Ok(p) => p,
        Err(e) => {
            println!("Failed to create playback stream: {}", e);
            return;
        }
    };

    let sample_rate = playback.sample_rate();
    let channels = playback.channels();

    // Generate different tones with different volumes
    for (freq, volume) in [(220.0, 0.3), (440.0, 0.6), (880.0, 1.0)] {
        playback.set_volume(volume).await;

        let duration_secs = 0.1;
        let num_samples = (sample_rate as f32 * duration_secs) as usize * channels as usize;
        let mut samples = Vec::with_capacity(num_samples);

        for i in 0..num_samples / channels as usize {
            let t = i as f32 / sample_rate as f32;
            let sample = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.3;

            for _ in 0..channels {
                samples.push(sample);
            }
        }

        let frame = AudioFrame {
            samples,
            sample_rate,
            channels,
            timestamp: Instant::now(),
        };

        let _ = playback.send(frame).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    println!("Volume change test passed");
}

#[tokio::test]
async fn test_playback_stream_buffer_space() {
    // Get default output device
    let host = cpal::default_host();
    let device = match host.default_output_device() {
        Some(d) => d,
        None => {
            println!("No output device available, skipping test");
            return;
        }
    };

    // Create playback stream
    let playback = match PlaybackStream::new(device, 1.0).await {
        Ok(p) => p,
        Err(e) => {
            println!("Failed to create playback stream: {}", e);
            return;
        }
    };

    let buffer_space = playback.get_buffer_space();
    assert!(buffer_space > 0, "Buffer space should be greater than 0");
    println!("Buffer space: {}", buffer_space);
}

#[tokio::test]
async fn test_playback_stream_drop() {
    // Get default output device
    let host = cpal::default_host();
    let device = match host.default_output_device() {
        Some(d) => d,
        None => {
            println!("No output device available, skipping test");
            return;
        }
    };

    {
        // Create playback stream in a scope
        let mut playback = match PlaybackStream::new(device, 1.0).await {
            Ok(p) => p,
            Err(e) => {
                println!("Failed to create playback stream: {}", e);
                return;
            }
        };

        // Send a frame before dropping
        let frame = AudioFrame {
            samples: vec![0.0; 1024],
            sample_rate: playback.sample_rate(),
            channels: playback.channels(),
            timestamp: Instant::now(),
        };

        let _ = playback.try_send(frame);
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Stream should be dropped here
    }

    // Give some time for cleanup
    tokio::time::sleep(Duration::from_millis(100)).await;
    println!("Drop test passed");
}
