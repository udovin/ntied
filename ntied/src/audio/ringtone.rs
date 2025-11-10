use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use tokio::task::{JoinHandle, spawn_blocking};

/// Ringtone player that generates and plays a dual-tone ringtone pattern
pub struct RingtonePlayer {
    is_playing: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
}

impl RingtonePlayer {
    pub fn new() -> Self {
        Self {
            is_playing: Arc::new(AtomicBool::new(false)),
            task: None,
        }
    }

    /// Start playing the ringtone
    pub fn start(&mut self) -> Result<()> {
        if self.is_playing.load(Ordering::Relaxed) {
            return Ok(()); // Already playing
        }

        self.is_playing.store(true, Ordering::Relaxed);

        let is_playing = self.is_playing.clone();

        self.task = Some(spawn_blocking(move || {
            if let Err(e) = Self::play_ringtone_blocking(is_playing) {
                tracing::error!("Failed to play ringtone: {}", e);
            }
        }));

        Ok(())
    }

    /// Stop playing the ringtone
    pub fn stop(&mut self) {
        self.is_playing.store(false, Ordering::Relaxed);

        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    /// Check if ringtone is currently playing
    pub fn is_playing(&self) -> bool {
        self.is_playing.load(Ordering::Relaxed)
    }

    fn play_ringtone_blocking(is_playing: Arc<AtomicBool>) -> Result<()> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("No output device available"))?;

        let config = device
            .default_output_config()
            .map_err(|e| anyhow!("Failed to get default output config: {}", e))?;

        let sample_format = config.sample_format();
        let sample_rate = config.sample_rate().0;
        let channels = config.channels();

        tracing::info!(
            "Ringtone playback: {} Hz, {} channels, format: {:?}",
            sample_rate,
            channels,
            sample_format
        );

        let stream_config: StreamConfig = config.into();

        let stream = match sample_format {
            SampleFormat::I8 => {
                Self::build_ringtone_stream::<i8>(&device, &stream_config, is_playing.clone())
            }
            SampleFormat::I16 => {
                Self::build_ringtone_stream::<i16>(&device, &stream_config, is_playing.clone())
            }
            SampleFormat::I32 => {
                Self::build_ringtone_stream::<i32>(&device, &stream_config, is_playing.clone())
            }
            SampleFormat::I64 => {
                Self::build_ringtone_stream::<i64>(&device, &stream_config, is_playing.clone())
            }
            SampleFormat::U8 => {
                Self::build_ringtone_stream::<u8>(&device, &stream_config, is_playing.clone())
            }
            SampleFormat::U16 => {
                Self::build_ringtone_stream::<u16>(&device, &stream_config, is_playing.clone())
            }
            SampleFormat::U32 => {
                Self::build_ringtone_stream::<u32>(&device, &stream_config, is_playing.clone())
            }
            SampleFormat::U64 => {
                Self::build_ringtone_stream::<u64>(&device, &stream_config, is_playing.clone())
            }
            SampleFormat::F32 => {
                Self::build_ringtone_stream::<f32>(&device, &stream_config, is_playing.clone())
            }
            SampleFormat::F64 => {
                Self::build_ringtone_stream::<f64>(&device, &stream_config, is_playing.clone())
            }
            _ => {
                return Err(anyhow!("Unsupported sample format: {:?}", sample_format));
            }
        }?;

        stream
            .play()
            .map_err(|e| anyhow!("Failed to play stream: {}", e))?;

        // Keep the stream alive while playing
        while is_playing.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        stream.pause().ok();

        Ok(())
    }

    fn build_ringtone_stream<T>(
        device: &Device,
        config: &StreamConfig,
        is_playing: Arc<AtomicBool>,
    ) -> Result<Stream>
    where
        T: SizedSample + FromSample<f32>,
    {
        let sample_rate = config.sample_rate.0 as f32;
        let channels = config.channels as usize;

        // Dual-tone frequencies (similar to phone ringtone)
        let freq1 = 480.0; // Hz
        let freq2 = 620.0; // Hz

        // Ring pattern: 2 seconds on, 4 seconds off
        let ring_duration = 2.0; // seconds
        let silence_duration = 4.0; // seconds
        let pattern_duration = ring_duration + silence_duration;

        let mut sample_clock = 0f32;

        let data_fn = move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            if !is_playing.load(Ordering::Relaxed) {
                // Fill with silence if stopped
                for sample in data.iter_mut() {
                    *sample = T::from_sample(0.0);
                }
                return;
            }

            for frame in data.chunks_mut(channels) {
                let time = sample_clock / sample_rate;
                let pattern_time = time % pattern_duration;

                let value = if pattern_time < ring_duration {
                    // Generate dual-tone signal
                    let t = time * 2.0 * std::f32::consts::PI;
                    let tone1 = (freq1 * t).sin();
                    let tone2 = (freq2 * t).sin();

                    // Mix the two tones and apply envelope
                    let mixed = (tone1 + tone2) * 0.15; // Reduced volume to 15%

                    // Apply fade in/out to avoid clicks
                    let fade_duration = 0.05; // 50ms fade
                    let fade_in = (pattern_time / fade_duration).min(1.0);
                    let fade_out = ((ring_duration - pattern_time) / fade_duration).min(1.0);
                    let envelope = fade_in * fade_out;

                    mixed * envelope
                } else {
                    // Silence
                    0.0
                };

                // Write the same value to all channels
                for sample in frame.iter_mut() {
                    *sample = T::from_sample(value);
                }

                sample_clock = (sample_clock + 1.0) % (sample_rate * pattern_duration);
            }
        };

        let err_fn = |err| {
            tracing::error!("Ringtone playback stream error: {}", err);
        };

        device
            .build_output_stream(config, data_fn, err_fn, None)
            .map_err(|e| anyhow!("Failed to build ringtone stream: {}", e))
    }
}

impl Drop for RingtonePlayer {
    fn drop(&mut self) {
        self.stop();
    }
}
