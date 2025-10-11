use cpal::traits::{DeviceTrait, HostTrait};
use ntied::audio::{AudioFrame, PlaybackStream};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Get audio host and device
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("No output device available"))?;

    println!("Using output device: {}", device.name()?);

    // Create playback stream with initial volume of 0.5
    let mut playback_stream = PlaybackStream::new(device, 0.5).await?;

    println!(
        "Playback stream created: {} Hz, {} channels",
        playback_stream.sample_rate(),
        playback_stream.channels()
    );
    println!("Playing various test sounds...\n");

    let _sample_rate = playback_stream.sample_rate();
    let _channels = playback_stream.channels();

    // 1. Play a sine wave (440 Hz - A note)
    println!("1. Playing 440 Hz sine wave (A note) for 1 second...");
    play_sine_wave(&mut playback_stream, 440.0, 1.0, 0.3).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    // 2. Play a sweep from low to high frequency
    println!("\n2. Playing frequency sweep (100 Hz to 2000 Hz)...");
    play_frequency_sweep(&mut playback_stream, 100.0, 2000.0, 2.0).await?;
    tokio::time::sleep(Duration::from_millis(2100)).await;

    // 3. Play white noise
    println!("\n3. Playing white noise for 0.5 seconds...");
    play_white_noise(&mut playback_stream, 0.5, 0.1).await?;
    tokio::time::sleep(Duration::from_millis(600)).await;

    // 4. Play a simple melody
    println!("\n4. Playing a simple melody...");
    play_melody(&mut playback_stream).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 5. Test volume control
    println!("\n5. Testing volume control with 440 Hz tone...");
    println!("   Volume: 0.1");
    playback_stream.set_volume(0.1).await;
    play_sine_wave(&mut playback_stream, 440.0, 0.5, 0.5).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    println!("   Volume: 0.5");
    playback_stream.set_volume(0.5).await;
    play_sine_wave(&mut playback_stream, 440.0, 0.5, 0.5).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    println!("   Volume: 1.0");
    playback_stream.set_volume(1.0).await;
    play_sine_wave(&mut playback_stream, 440.0, 0.5, 0.5).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 6. Test pause and play
    println!("\n6. Testing pause/play functionality...");
    println!("   Playing tone...");
    play_sine_wave(&mut playback_stream, 880.0, 3.0, 0.3).await?;

    tokio::time::sleep(Duration::from_millis(500)).await;
    println!("   Muting...");
    playback_stream.set_mute(true).await;

    tokio::time::sleep(Duration::from_secs(1)).await;
    println!("   Unmuting...");
    playback_stream.set_mute(false).await;

    tokio::time::sleep(Duration::from_millis(1500)).await;

    println!("\nPlayback demo complete!");

    // Stream will be automatically stopped when dropped
    Ok(())
}

async fn play_sine_wave(
    stream: &mut PlaybackStream,
    frequency: f32,
    duration_secs: f32,
    amplitude: f32,
) -> anyhow::Result<()> {
    let sample_rate = stream.sample_rate();
    let channels = stream.channels();
    let num_samples = (sample_rate as f32 * duration_secs) as usize * channels as usize;
    let mut samples = Vec::with_capacity(num_samples);

    for i in 0..num_samples / channels as usize {
        let t = i as f32 / sample_rate as f32;
        let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * amplitude;

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

    stream.send(frame).await?;
    Ok(())
}

async fn play_frequency_sweep(
    stream: &mut PlaybackStream,
    start_freq: f32,
    end_freq: f32,
    duration_secs: f32,
) -> anyhow::Result<()> {
    let sample_rate = stream.sample_rate();
    let channels = stream.channels();
    let num_samples = (sample_rate as f32 * duration_secs) as usize * channels as usize;
    let mut samples = Vec::with_capacity(num_samples);

    for i in 0..num_samples / channels as usize {
        let t = i as f32 / sample_rate as f32;
        let progress = t / duration_secs;
        let frequency = start_freq + (end_freq - start_freq) * progress;
        let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.2;

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

    stream.send(frame).await?;
    Ok(())
}

async fn play_white_noise(
    stream: &mut PlaybackStream,
    duration_secs: f32,
    amplitude: f32,
) -> anyhow::Result<()> {
    use rand::Rng;
    let mut rng = rand::thread_rng();

    let sample_rate = stream.sample_rate();
    let channels = stream.channels();
    let num_samples = (sample_rate as f32 * duration_secs) as usize * channels as usize;
    let mut samples = Vec::with_capacity(num_samples);

    for _ in 0..num_samples / channels as usize {
        let sample = rng.gen_range(-amplitude..amplitude);
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

    stream.send(frame).await?;
    Ok(())
}

async fn play_melody(stream: &mut PlaybackStream) -> anyhow::Result<()> {
    // Simple melody: C, E, G, C (C major chord)
    let notes = [
        (261.63, 0.3), // C4
        (329.63, 0.3), // E4
        (392.00, 0.3), // G4
        (523.25, 0.5), // C5
    ];

    for (frequency, duration) in notes {
        play_sine_wave(stream, frequency, duration, 0.3).await?;
        tokio::time::sleep(Duration::from_millis((duration * 1000.0) as u64 + 50)).await;
    }

    Ok(())
}
