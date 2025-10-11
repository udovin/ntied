use cpal::traits::{DeviceTrait, HostTrait};
use ntied::audio::CaptureStream;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Get audio host and device
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("No input device available"))?;

    println!("Using input device: {}", device.name()?);

    // Create capture stream with initial volume of 1.0
    let mut capture_stream = CaptureStream::new(device, 1.0).await?;

    println!(
        "Capture stream created: {} Hz, {} channels",
        capture_stream.sample_rate(),
        capture_stream.channels()
    );
    println!("Starting audio capture for 10 seconds...\n");

    // Capture audio for 10 seconds
    let start = std::time::Instant::now();
    let mut frame_count = 0;
    let mut total_samples = 0;

    while start.elapsed() < Duration::from_secs(10) {
        // Try to receive audio frame with a timeout
        tokio::select! {
            frame = capture_stream.recv() => {
                if let Some(frame) = frame {
                    frame_count += 1;
                    total_samples += frame.samples.len();

                    // Calculate RMS (root mean square) for volume level
                    let rms = calculate_rms(&frame.samples);
                    let db = 20.0 * rms.log10();

                    // Print audio level meter
                    if frame_count % 10 == 0 {
                        print_level_meter(rms, db);
                    }

                    // Test volume control
                    if frame_count == 100 {
                        println!("\n>>> Reducing volume to 50%");
                        capture_stream.set_volume(0.5).await;
                    } else if frame_count == 200 {
                        println!("\n>>> Increasing volume to 150%");
                        capture_stream.set_volume(1.5).await;
                    } else if frame_count == 300 {
                        println!("\n>>> Muting audio");
                        capture_stream.set_mute(true).await;
                    } else if frame_count == 400 {
                        println!("\n>>> Unmuting audio");
                        capture_stream.set_mute(false).await;
                        capture_stream.set_volume(1.0).await;
                    }
                }
            }
            _ = sleep(Duration::from_millis(100)) => {
                // Timeout - no frame received
                print!(".");
            }
        }
    }

    println!("\n\nCapture complete!");
    println!("Total frames received: {}", frame_count);
    println!("Total samples processed: {}", total_samples);
    println!("Average frame rate: {:.1} fps", frame_count as f32 / 10.0);

    // Stream will be automatically stopped when dropped
    Ok(())
}

fn calculate_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_squares: f32 = samples.iter().map(|s| s * s).sum();
    (sum_squares / samples.len() as f32).sqrt()
}

fn print_level_meter(rms: f32, db: f32) {
    // Create a simple ASCII level meter
    let level = (rms * 50.0).min(50.0) as usize;
    let meter = "█".repeat(level) + &"░".repeat(50 - level);

    print!("\rLevel: {} {:6.2} dB", meter, db);
    use std::io::{self, Write};
    let _ = io::stdout().flush();
}
