use anyhow::Result;
use cpal::traits::DeviceTrait;
use ntied::audio::{AudioManager, CaptureStream, PlaybackStream};
use std::time::Duration;
use tokio;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .init();

    println!("=== Audio System Diagnostic Tool ===\n");

    // Check audio host
    let host = cpal::default_host();
    println!("Audio Host: {:?}\n", host.id());

    // List and check all input devices
    println!("=== Input Devices ===");
    match AudioManager::list_input_devices().await {
        Ok(devices) => {
            if devices.is_empty() {
                println!("No input devices found!");
            } else {
                for (idx, device) in devices.iter().enumerate() {
                    println!("\n{}. {}", idx + 1, device.name);
                    if device.is_default {
                        println!("   [DEFAULT INPUT]");
                    }

                    // Get the actual cpal device to show config details
                    if let Ok(cpal_device) =
                        AudioManager::get_input_device(Some(device.name.clone())).await
                    {
                        // Get supported configs
                        if let Ok(configs) = cpal_device.supported_input_configs() {
                            println!("   Supported configurations:");
                            for config in configs {
                                let min_rate = config.min_sample_rate().0;
                                let max_rate = config.max_sample_rate().0;
                                let channels = config.channels();
                                let format = config.sample_format();

                                if min_rate == max_rate {
                                    println!(
                                        "     - {} Hz, {} ch, {:?}",
                                        min_rate, channels, format
                                    );
                                } else {
                                    println!(
                                        "     - {}-{} Hz, {} ch, {:?}",
                                        min_rate, max_rate, channels, format
                                    );
                                }
                            }
                        }

                        // Get default config
                        if let Ok(config) = cpal_device.default_input_config() {
                            println!("   Default configuration:");
                            println!(
                                "     - {} Hz, {} ch, {:?}",
                                config.sample_rate().0,
                                config.channels(),
                                config.sample_format()
                            );
                        }
                    }
                }
            }
        }
        Err(e) => {
            println!("Error listing input devices: {}", e);
        }
    }

    // List and check all output devices
    println!("\n=== Output Devices ===");
    match AudioManager::list_output_devices().await {
        Ok(devices) => {
            if devices.is_empty() {
                println!("No output devices found!");
            } else {
                for (idx, device) in devices.iter().enumerate() {
                    println!("\n{}. {}", idx + 1, device.name);
                    if device.is_default {
                        println!("   [DEFAULT OUTPUT]");
                    }

                    // Get the actual cpal device to show config details
                    if let Ok(cpal_device) =
                        AudioManager::get_output_device(Some(device.name.clone())).await
                    {
                        // Get supported configs
                        if let Ok(configs) = cpal_device.supported_output_configs() {
                            println!("   Supported configurations:");
                            for config in configs {
                                let min_rate = config.min_sample_rate().0;
                                let max_rate = config.max_sample_rate().0;
                                let channels = config.channels();
                                let format = config.sample_format();

                                if min_rate == max_rate {
                                    println!(
                                        "     - {} Hz, {} ch, {:?}",
                                        min_rate, channels, format
                                    );
                                } else {
                                    println!(
                                        "     - {}-{} Hz, {} ch, {:?}",
                                        min_rate, max_rate, channels, format
                                    );
                                }
                            }
                        }

                        // Get default config
                        if let Ok(config) = cpal_device.default_output_config() {
                            println!("   Default configuration:");
                            println!(
                                "     - {} Hz, {} ch, {:?}",
                                config.sample_rate().0,
                                config.channels(),
                                config.sample_format()
                            );
                        }
                    }
                }
            }
        }
        Err(e) => {
            println!("Error listing output devices: {}", e);
        }
    }

    // Test capture stream initialization
    println!("\n=== Testing Capture Stream ===");
    match AudioManager::get_input_device(None).await {
        Ok(device) => {
            println!("\nStarting capture with default device...");
            match CaptureStream::new(device, 1.0).await {
                Ok(mut capture_stream) => {
                    println!("✓ Capture started successfully");
                    println!("  Sample rate: {} Hz", capture_stream.sample_rate());
                    println!("  Channels: {}", capture_stream.channels());

                    // Receive a few frames to check actual parameters
                    println!("  Receiving test frames...");
                    for i in 0..5 {
                        tokio::select! {
                            frame = capture_stream.recv() => {
                                if let Some(frame) = frame {
                                    if i == 0 {
                                        println!("  First frame details:");
                                        println!("    Sample rate: {} Hz", frame.sample_rate);
                                        println!("    Channels: {}", frame.channels);
                                        println!("    Frame size: {} samples", frame.samples.len());
                                        println!("    Frame duration: {:.1} ms",
                                            (frame.samples.len() as f32 / frame.channels as f32) * 1000.0 / frame.sample_rate as f32);

                                        // Calculate RMS level
                                        let rms: f32 = if !frame.samples.is_empty() {
                                            let sum_squares: f32 = frame.samples.iter().map(|s| s * s).sum();
                                            (sum_squares / frame.samples.len() as f32).sqrt()
                                        } else {
                                            0.0
                                        };
                                        println!("    RMS level: {:.4} ({:.1} dB)", rms, 20.0 * rms.log10());
                                    }
                                }
                            }
                            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                                println!("  Timeout waiting for frame {}", i + 1);
                                break;
                            }
                        }
                    }

                    println!("✓ Capture test complete");
                }
                Err(e) => {
                    println!("✗ Failed to start capture: {}", e);
                }
            }
        }
        Err(e) => {
            println!("✗ No input device available: {}", e);
        }
    }

    // Test playback stream initialization
    println!("\n=== Testing Playback Stream ===");
    match AudioManager::get_output_device(None).await {
        Ok(device) => {
            println!("\nStarting playback with default device...");
            match PlaybackStream::new(device, 1.0).await {
                Ok(mut playback_stream) => {
                    println!("✓ Playback started successfully");
                    println!("  Sample rate: {} Hz", playback_stream.sample_rate());
                    println!("  Channels: {}", playback_stream.channels());

                    // Send a test tone
                    println!("  Sending test tone (440 Hz for 0.5 seconds)...");

                    let sample_rate = playback_stream.sample_rate();
                    let channels = playback_stream.channels();
                    let duration_secs = 0.5;
                    let frequency = 440.0;
                    let amplitude = 0.2;

                    let num_samples =
                        (sample_rate as f32 * duration_secs) as usize * channels as usize;
                    let mut samples = Vec::with_capacity(num_samples);

                    for i in 0..num_samples / channels as usize {
                        let t = i as f32 / sample_rate as f32;
                        let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * amplitude;

                        // Add same sample for all channels
                        for _ in 0..channels {
                            samples.push(sample);
                        }
                    }

                    let frame = ntied::audio::AudioFrame {
                        samples,
                        sample_rate,
                        channels,
                        timestamp: std::time::Instant::now(),
                    };

                    match playback_stream.send(frame).await {
                        Ok(_) => println!("  ✓ Test tone sent successfully"),
                        Err(e) => println!("  ✗ Failed to send test tone: {}", e),
                    }

                    // Wait for playback to complete
                    tokio::time::sleep(Duration::from_millis(600)).await;

                    println!("✓ Playback test complete");
                }
                Err(e) => {
                    println!("✗ Failed to start playback: {}", e);
                }
            }
        }
        Err(e) => {
            println!("✗ No output device available: {}", e);
        }
    }

    // Test simultaneous capture and playback (echo test)
    println!("\n=== Testing Simultaneous Capture & Playback ===");

    let input_device_result = AudioManager::get_input_device(None).await;
    let output_device_result = AudioManager::get_output_device(None).await;

    if let (Ok(input_device), Ok(output_device)) = (input_device_result, output_device_result) {
        match CaptureStream::new(input_device, 1.0).await {
            Ok(mut capture_stream) => {
                match PlaybackStream::new(output_device, 0.5).await {
                    Ok(mut playback_stream) => {
                        println!("✓ Both capture and playback started");
                        println!("  Processing audio through pipeline for 2 seconds...");
                        println!("  (You should hear your microphone input with slight delay)");

                        let start = std::time::Instant::now();
                        let mut frame_count = 0;
                        let mut total_latency_ms = 0.0;

                        while start.elapsed() < Duration::from_secs(2) {
                            tokio::select! {
                                frame = capture_stream.recv() => {
                                    if let Some(frame) = frame {
                                        let capture_time = std::time::Instant::now();

                                        // Forward captured audio to playback
                                        match playback_stream.try_send(frame) {
                                            Ok(_) => {
                                                frame_count += 1;
                                                let latency = capture_time.elapsed().as_secs_f32() * 1000.0;
                                                total_latency_ms += latency;
                                            }
                                            Err(e) => {
                                                if frame_count == 0 {
                                                    println!("  ⚠ Frame dropped: {}", e);
                                                }
                                            }
                                        }
                                    }
                                }
                                _ = tokio::time::sleep(Duration::from_millis(2100)) => {
                                    break;
                                }
                            }
                        }

                        println!("  Processed {} frames", frame_count);
                        if frame_count > 0 {
                            println!(
                                "  Average processing latency: {:.2} ms",
                                total_latency_ms / frame_count as f32
                            );
                        }

                        println!("✓ Simultaneous capture & playback test complete");
                    }
                    Err(e) => {
                        println!("✗ Failed to start playback: {}", e);
                    }
                }
            }
            Err(e) => {
                println!("✗ Failed to start capture: {}", e);
            }
        }
    } else {
        println!("✗ Cannot run simultaneous test - missing input or output device");
    }

    // Summary
    println!("\n=== Diagnostic Summary ===");
    println!(
        "
Audio system architecture:
- CaptureStream: Direct microphone capture with configurable volume
- PlaybackStream: Direct speaker playback with configurable volume
- AudioSession: Per-call encoder/decoder with fixed codec parameters
- No global state: Each call has independent audio processing

If you're experiencing audio issues, check:
1. Are your devices using standard sample rates (44100 or 48000 Hz)?
2. Were all tests above successful?
3. Did you hear the test tone during playback test?
4. Did you hear your microphone during the echo test?

Common issues:
- Some USB/Bluetooth devices may have higher latency
- Feedback/echo in simultaneous test is normal (use headphones)
- Windows audio exclusive mode may prevent multiple streams
- High CPU usage can cause audio glitches
"
    );

    println!("\nDiagnostic complete.");
    Ok(())
}
