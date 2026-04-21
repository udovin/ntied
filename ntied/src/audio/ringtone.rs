//! Looping incoming-call ringtone.
//!
//! Reads the bundled WAV (`assets/sounds/ringtone.wav`), resamples to the
//! device, and plays it on repeat with a short silence gap between cycles
//! until `stop()` is called.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, StreamConfig};
use tokio::task::{JoinHandle, spawn_blocking};

use super::sound_effect::{decode_pcm16_mono, interleave_mono, resample_linear};

const RINGTONE_WAV: &[u8] = include_bytes!("../../assets/sounds/ringtone.wav");

/// Silence between repetitions of the bundled ringtone, in seconds.
const LOOP_GAP_SECS: f32 = 0.8;
/// Output level (0.0–1.0).
const LOOP_GAIN: f32 = 0.85;

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

    pub fn start(&mut self) -> Result<()> {
        if self.is_playing.swap(true, Ordering::Relaxed) {
            return Ok(()); // already playing
        }
        let is_playing = self.is_playing.clone();
        self.task = Some(spawn_blocking(move || {
            if let Err(e) = play_loop_blocking(is_playing) {
                tracing::error!("Failed to play ringtone: {}", e);
            }
        }));
        Ok(())
    }

    pub fn stop(&mut self) {
        self.is_playing.store(false, Ordering::Relaxed);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    pub fn is_playing(&self) -> bool {
        self.is_playing.load(Ordering::Relaxed)
    }
}

impl Default for RingtonePlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RingtonePlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn play_loop_blocking(is_playing: Arc<AtomicBool>) -> Result<()> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("no output device"))?;
    let config = device
        .default_output_config()
        .map_err(|e| anyhow!("default output config: {e}"))?;
    let sample_format = config.sample_format();
    let stream_config: StreamConfig = config.into();
    let sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;

    let (mono, src_rate) = decode_pcm16_mono(RINGTONE_WAV)?;
    let resampled = resample_linear(&mono, src_rate, sample_rate);
    let scaled: Vec<f32> = resampled.iter().map(|s| s * LOOP_GAIN).collect();
    let mut interleaved = interleave_mono(&scaled, channels);
    let gap_samples = (LOOP_GAP_SECS * sample_rate as f32) as usize * channels;
    interleaved.extend(std::iter::repeat(0.0).take(gap_samples));
    let pattern = Arc::new(interleaved);

    let stream = match sample_format {
        SampleFormat::I8 => build::<i8>(&device, &stream_config, pattern.clone(), is_playing.clone()),
        SampleFormat::I16 => build::<i16>(&device, &stream_config, pattern.clone(), is_playing.clone()),
        SampleFormat::I32 => build::<i32>(&device, &stream_config, pattern.clone(), is_playing.clone()),
        SampleFormat::I64 => build::<i64>(&device, &stream_config, pattern.clone(), is_playing.clone()),
        SampleFormat::U8 => build::<u8>(&device, &stream_config, pattern.clone(), is_playing.clone()),
        SampleFormat::U16 => build::<u16>(&device, &stream_config, pattern.clone(), is_playing.clone()),
        SampleFormat::U32 => build::<u32>(&device, &stream_config, pattern.clone(), is_playing.clone()),
        SampleFormat::U64 => build::<u64>(&device, &stream_config, pattern.clone(), is_playing.clone()),
        SampleFormat::F32 => build::<f32>(&device, &stream_config, pattern.clone(), is_playing.clone()),
        SampleFormat::F64 => build::<f64>(&device, &stream_config, pattern.clone(), is_playing.clone()),
        _ => return Err(anyhow!("unsupported sample format {:?}", sample_format)),
    }?;
    stream.play().map_err(|e| anyhow!("stream play: {e}"))?;

    while is_playing.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = stream.pause();
    Ok(())
}

fn build<T>(
    device: &Device,
    config: &StreamConfig,
    pattern: Arc<Vec<f32>>,
    is_playing: Arc<AtomicBool>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let total = pattern.len();
    let mut cursor: usize = 0;
    let data_fn = move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
        if !is_playing.load(Ordering::Relaxed) {
            for slot in data.iter_mut() {
                *slot = T::from_sample(0.0);
            }
            return;
        }
        for slot in data.iter_mut() {
            *slot = T::from_sample(pattern[cursor]);
            cursor += 1;
            if cursor >= total {
                cursor = 0;
            }
        }
    };
    let err_fn = |err| tracing::warn!(?err, "ringtone stream error");
    device
        .build_output_stream(config, data_fn, err_fn, None)
        .map_err(|e| anyhow!("build_output_stream: {e}"))
}
