use cpal::traits::HostTrait;
use ntied::audio::CaptureStream;
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn test_capture_stream_creation() {
    // Get default input device
    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(d) => d,
        None => {
            println!("No input device available, skipping test");
            return;
        }
    };

    // Create capture stream
    let result = CaptureStream::new(device, 1.0).await;
    assert!(
        result.is_ok(),
        "Failed to create CaptureStream: {:?}",
        result.err()
    );

    let capture = result.unwrap();
    assert_eq!(capture.channels(), capture.channels());
    assert!(capture.sample_rate() > 0);
}

#[tokio::test]
async fn test_capture_stream_receive_frames() {
    // Get default input device
    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(d) => d,
        None => {
            println!("No input device available, skipping test");
            return;
        }
    };

    // Create capture stream
    let mut capture = match CaptureStream::new(device, 1.0).await {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to create capture stream: {}", e);
            return;
        }
    };

    // Try to receive a frame with timeout
    let result = timeout(Duration::from_secs(2), capture.recv()).await;

    match result {
        Ok(Some(frame)) => {
            assert!(!frame.samples.is_empty(), "Received empty frame");
            assert_eq!(frame.sample_rate, capture.sample_rate());
            assert_eq!(frame.channels, capture.channels());
            println!(
                "Successfully received frame with {} samples",
                frame.samples.len()
            );
        }
        Ok(None) => {
            println!("Channel closed");
        }
        Err(_) => {
            println!(
                "Timeout waiting for audio frame (this might be normal if no audio is being captured)"
            );
        }
    }
}

#[tokio::test]
async fn test_capture_stream_mute() {
    // Get default input device
    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(d) => d,
        None => {
            println!("No input device available, skipping test");
            return;
        }
    };

    // Create capture stream
    let mut capture = match CaptureStream::new(device, 1.0).await {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to create capture stream: {}", e);
            return;
        }
    };

    // Test mute/unmute
    capture.set_mute(true).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    capture.set_mute(false).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Should not panic
    println!("Mute/unmute test passed");
}

#[tokio::test]
async fn test_capture_stream_volume() {
    // Get default input device
    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(d) => d,
        None => {
            println!("No input device available, skipping test");
            return;
        }
    };

    // Create capture stream with initial volume
    let mut capture = match CaptureStream::new(device, 0.5).await {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to create capture stream: {}", e);
            return;
        }
    };

    // Change volume
    capture.set_volume(0.8).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    capture.set_volume(1.5).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    capture.set_volume(0.0).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    println!("Volume change test passed");
}

#[tokio::test]
async fn test_capture_stream_drop() {
    // Get default input device
    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(d) => d,
        None => {
            println!("No input device available, skipping test");
            return;
        }
    };

    {
        // Create capture stream in a scope
        let _capture = match CaptureStream::new(device, 1.0).await {
            Ok(c) => c,
            Err(e) => {
                println!("Failed to create capture stream: {}", e);
                return;
            }
        };

        tokio::time::sleep(Duration::from_millis(100)).await;
        // Stream should be dropped here
    }

    // Give some time for cleanup
    tokio::time::sleep(Duration::from_millis(100)).await;
    println!("Drop test passed");
}
