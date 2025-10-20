use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait};
use ntied::audio::AudioManager;
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
    let input_devices = host.input_devices()?;
    for (idx, device) in input_devices.enumerate() {
        if let Ok(name) = device.name() {
            println!("\n{}. {}", idx + 1, name);

            // Check if this is the default device
            let is_default = host
                .default_input_device()
                .and_then(|d| d.name().ok())
                .map(|n| n == name)
                .unwrap_or(false);

            if is_default {
                println!("   [DEFAULT INPUT]");
            }

            // Get supported configs
            if let Ok(configs) = device.supported_input_configs() {
                println!("   Supported configurations:");
                for config in configs {
                    let min_rate = config.min_sample_rate().0;
                    let max_rate = config.max_sample_rate().0;
                    let channels = config.channels();
                    let format = config.sample_format();

                    if min_rate == max_rate {
                        println!("     - {} Hz, {} ch, {:?}", min_rate, channels, format);
                    } else {
                        println!(
                            "     - {}-{} Hz, {} ch, {:?}",
                            min_rate, max_rate, channels, format
                        );
                    }
                }
            }

            // Get default config
            if let Ok(config) = device.default_input_config() {
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

    // List and check all output devices
    println!("\n=== Output Devices ===");
    let output_devices = host.output_devices()?;
    for (idx, device) in output_devices.enumerate() {
        if let Ok(name) = device.name() {
            println!("\n{}. {}", idx + 1, name);

            // Check if this is the default device
            let is_default = host
                .default_output_device()
                .and_then(|d| d.name().ok())
                .map(|n| n == name)
                .unwrap_or(false);

            if is_default {
                println!("   [DEFAULT OUTPUT]");
            }

            // Get supported configs
            if let Ok(configs) = device.supported_output_configs() {
                println!("   Supported configurations:");
                for config in configs {
                    let min_rate = config.min_sample_rate().0;
                    let max_rate = config.max_sample_rate().0;
                    let channels = config.channels();
                    let format = config.sample_format();

                    if min_rate == max_rate {
                        println!("     - {} Hz, {} ch, {:?}", min_rate, channels, format);
                    } else {
                        println!(
                            "     - {}-{} Hz, {} ch, {:?}",
                            min_rate, max_rate, channels, format
                        );
                    }
                }
            }

            // Get default config
            if let Ok(config) = device.default_output_config() {
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

    // Test audio manager initialization
    println!("\n=== Testing Audio Manager ===");
    let manager = AudioManager::new();

    // Try to start capture with default device
    println!("\nStarting capture with default device...");
    match manager.start_capture(None, 1.0).await {
        Ok(mut rx) => {
            println!("✓ Capture started successfully");

            // Receive a few frames to check actual parameters
            println!("  Receiving test frames...");
            for i in 0..5 {
                tokio::select! {
                    frame = rx.recv() => {
                        if let Some(frame) = frame {
                            if i == 0 {
                                println!("  First frame details:");
                                println!("    Sample rate: {} Hz", frame.sample_rate);
                                println!("    Channels: {}", frame.channels);
                                println!("    Frame size: {} samples", frame.samples.len());
                                println!("    Frame duration: {:.1} ms",
                                    (frame.samples.len() as f32 / frame.channels as f32) * 1000.0 / frame.sample_rate as f32);
                            }
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        println!("  Timeout waiting for frame {}", i + 1);
                        break;
                    }
                }
            }

            manager.stop_capture().await?;
            println!("✓ Capture stopped");
        }
        Err(e) => {
            println!("✗ Failed to start capture: {}", e);
        }
    }

    // Try to start playback with default device
    println!("\nStarting playback with default device...");
    match manager.start_playback(None, 1.0).await {
        Ok(_) => {
            println!("✓ Playback started successfully");

            // Send a test frame to check resampling
            println!("  Testing audio frame queueing...");

            let test_rates = vec![8000, 16000, 44100, 48000];
            for rate in test_rates {
                let samples = vec![0.0f32; (rate * 20 / 1000) as usize]; // 20ms of silence
                match manager.queue_audio_frame(0, samples, rate, 1).await {
                    Ok(_) => println!("    ✓ {} Hz frame queued successfully", rate),
                    Err(e) => println!("    ✗ {} Hz frame failed: {}", rate, e),
                }
            }

            // Give time for processing
            tokio::time::sleep(Duration::from_millis(100)).await;

            manager.stop_playback().await?;
            println!("✓ Playback stopped");
        }
        Err(e) => {
            println!("✗ Failed to start playback: {}", e);
        }
    }

    // Test simultaneous capture and playback
    println!("\n=== Testing Simultaneous Capture & Playback ===");

    match manager.start_capture(None, 1.0).await {
        Ok(mut capture_rx) => {
            match manager.start_playback(None, 1.0).await {
                Ok(_) => {
                    println!("✓ Both capture and playback started");

                    // Process a few frames through the entire pipeline
                    println!("  Processing frames through pipeline...");
                    let mut sequence = 0u32;

                    for _ in 0..10 {
                        tokio::select! {
                            frame = capture_rx.recv() => {
                                if let Some(frame) = frame {
                                    let sample_rate = frame.sample_rate;
                                    let channels = frame.channels;
                                    let num_samples = frame.samples.len();

                                    // Queue for playback
                                    match manager.queue_audio_frame(sequence, frame.samples, sample_rate, channels).await {
                                        Ok(_) => {
                                            if sequence == 0 {
                                                println!("    Frame {}: {} samples @ {} Hz, {} ch - queued OK",
                                                    sequence, num_samples, sample_rate, channels);
                                            }
                                        }
                                        Err(e) => {
                                            println!("    Frame {} failed: {}", sequence, e);
                                        }
                                    }
                                    sequence += 1;
                                }
                            }
                            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                                break;
                            }
                        }
                    }

                    println!("  Processed {} frames", sequence);

                    // Check jitter buffer stats
                    let stats = manager.get_stats().await;
                    println!("\n  Jitter Buffer Statistics:");
                    println!("    Packets received: {}", stats.packets_received);
                    println!("    Packets lost: {}", stats.packets_lost);
                    println!("    Packets late: {}", stats.packets_late);
                    println!("    Average jitter: {:.2} ms", stats.average_jitter_ms);
                    println!("    Current delay: {:.2} ms", stats.current_delay_ms);

                    manager.stop_playback().await?;
                    println!("✓ Playback stopped");
                }
                Err(e) => {
                    println!("✗ Failed to start playback: {}", e);
                }
            }

            manager.stop_capture().await?;
            println!("✓ Capture stopped");
        }
        Err(e) => {
            println!("✗ Failed to start capture: {}", e);
        }
    }

    // Summary
    println!("\n=== Diagnostic Summary ===");
    println!(
        "
If you're experiencing pitch issues, check:
1. Are your input and output devices using standard sample rates (44100 or 48000 Hz)?
2. Are there any warnings about non-standard sample rates in the logs above?
3. Is the 'Frame duration' close to 20ms for captured frames?
4. Are the jitter buffer statistics showing high packet loss or jitter?

Common issues:
- Some USB/Bluetooth devices may report incorrect sample rates
- Some devices may not support the requested sample rate and fall back to another
- High CPU usage can cause audio processing delays
- Network issues can cause packet loss leading to audio artifacts
"
    );

    println!("\nDiagnostic complete.");
    Ok(())
}
