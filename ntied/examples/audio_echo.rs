use cpal::traits::{DeviceTrait, HostTrait};
use ntied::audio::{AudioFrame, CaptureStream, PlaybackStream};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    println!("Audio Echo Example");
    println!("==================");
    println!(
        "This example captures audio from your microphone and plays it back with a delay (echo effect).\n"
    );

    // Get audio host
    let host = cpal::default_host();

    // Get input device
    let input_device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("No input device available"))?;
    println!("Input device: {}", input_device.name()?);

    // Get output device
    let output_device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("No output device available"))?;
    println!("Output device: {}", output_device.name()?);

    // Create capture and playback streams
    let mut capture_stream = CaptureStream::new(input_device, 1.0).await?;
    let mut playback_stream = PlaybackStream::new(output_device, 0.5).await?;

    println!(
        "\nCapture: {} Hz, {} channels",
        capture_stream.sample_rate(),
        capture_stream.channels()
    );
    println!(
        "Playback: {} Hz, {} channels",
        playback_stream.sample_rate(),
        playback_stream.channels()
    );

    // Create delay buffer for echo effect
    let delay_ms = 500; // 500ms delay
    let delay_samples = (capture_stream.sample_rate() as usize * delay_ms / 1000)
        * capture_stream.channels() as usize;
    let echo_buffer = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(delay_samples)));

    // Initialize buffer with silence
    {
        let mut buffer = echo_buffer.lock().await;
        for _ in 0..delay_samples {
            buffer.push_back(0.0);
        }
    }

    println!("\nStarting echo effect with {}ms delay...", delay_ms);
    println!("Press Ctrl+C to stop\n");

    // Statistics
    let mut frame_count = 0u64;
    let mut dropped_frames = 0u64;
    let start_time = Instant::now();
    let mut last_stats_time = Instant::now();

    // Main loop
    loop {
        tokio::select! {
            // Receive audio from capture
            frame = capture_stream.recv() => {
                if let Some(frame) = frame {
                    frame_count += 1;

                    // Process with echo effect
                    let mut buffer = echo_buffer.lock().await;
                    let mut output_samples = Vec::with_capacity(frame.samples.len());

                    for sample in frame.samples {
                        // Add current sample to delay buffer
                        buffer.push_back(sample);

                        // Get delayed sample for echo
                        if let Some(delayed_sample) = buffer.pop_front() {
                            // Mix current sample with delayed sample (echo effect)
                            let mixed = sample * 0.7 + delayed_sample * 0.3;
                            output_samples.push(mixed);
                        } else {
                            output_samples.push(sample * 0.7);
                        }
                    }

                    // Create output frame
                    let output_frame = AudioFrame {
                        samples: output_samples,
                        sample_rate: frame.sample_rate,
                        channels: frame.channels,
                        timestamp: Instant::now(),
                    };

                    // Send to playback
                    match playback_stream.try_send(output_frame) {
                        Ok(_) => {},
                        Err(e) => {
                            dropped_frames += 1;
                            if dropped_frames % 100 == 0 {
                                tracing::warn!("Dropped {} frames: {}", dropped_frames, e);
                            }
                        }
                    }

                    // Print statistics every 5 seconds
                    if last_stats_time.elapsed() > Duration::from_secs(5) {
                        let elapsed = start_time.elapsed().as_secs_f64();
                        let fps = frame_count as f64 / elapsed;
                        let drop_rate = (dropped_frames as f64 / frame_count as f64) * 100.0;

                        println!(
                            "Stats: {} frames, {:.1} fps, {:.2}% dropped",
                            frame_count, fps, drop_rate
                        );

                        last_stats_time = Instant::now();
                    }
                }
            }

            // Handle Ctrl+C gracefully
            _ = tokio::signal::ctrl_c() => {
                println!("\n\nStopping echo effect...");
                break;
            }
        }
    }

    // Print final statistics
    let elapsed = start_time.elapsed().as_secs_f64();
    println!("\nFinal Statistics:");
    println!("  Duration: {:.1} seconds", elapsed);
    println!("  Total frames: {}", frame_count);
    println!("  Average FPS: {:.1}", frame_count as f64 / elapsed);
    println!(
        "  Dropped frames: {} ({:.2}%)",
        dropped_frames,
        (dropped_frames as f64 / frame_count as f64) * 100.0
    );

    // Streams will be automatically stopped when dropped
    Ok(())
}
