use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait as _, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, spawn_blocking};

use super::AudioFrame;

enum Command {
    Mute(bool),
}

pub struct PlaybackStream {
    command_tx: mpsc::Sender<Command>,
    volume: Arc<AtomicU32>,
    tx: mpsc::Sender<AudioFrame>,
    task: JoinHandle<()>,
    sample_rate: u32,
    channels: u16,
}

impl PlaybackStream {
    pub async fn new(device: Device, volume: f32) -> Result<Self> {
        let (command_tx, mut command_rx) = mpsc::channel(1);
        let (tx, mut rx) = mpsc::channel::<AudioFrame>(100);
        let volume = Arc::new(AtomicU32::new(f32::to_bits(volume)));
        let config = device
            .default_output_config()
            .map_err(|e| anyhow!("Failed to get default output config: {}", e))?;
        let sample_format = config.sample_format();
        let sample_rate = config.sample_rate().0;
        let channels = config.channels();

        // Log device configuration
        tracing::info!(
            "Playback device config: {} Hz, {} channels, format: {:?}",
            sample_rate,
            channels,
            sample_format
        );

        // Warn if unusual sample rate
        if sample_rate != 44100 && sample_rate != 48000 && sample_rate != 96000 {
            tracing::warn!(
                "Playback device using non-standard sample rate: {} Hz",
                sample_rate
            );
        }

        let stream_config: StreamConfig = config.into();
        let task = {
            let volume = volume.clone();
            spawn_blocking(move || {
                // Ring buffer for playback samples
                let buffer_size = ((sample_rate as usize) * channels as usize) / 5; // 200ms buffer for better stability
                let ring_buffer = Arc::new(std::sync::Mutex::new(Vec::<f32>::with_capacity(
                    buffer_size,
                )));
                let ring_buffer_clone = ring_buffer.clone();
                // Spawn task to receive audio frames and fill the buffer
                let runtime = tokio::runtime::Handle::current();
                let buffer_fill_task = runtime.spawn(async move {
                    let mut frame_count = 0u64;
                    tracing::info!("Playback buffer fill task started");
                    while let Some(frame) = rx.recv().await {
                        frame_count += 1;
                        if frame_count % 100 == 0 {
                            tracing::debug!(
                                "Playback received frame #{}, samples: {}",
                                frame_count,
                                frame.samples.len()
                            );
                        }
                        let mut buffer = ring_buffer_clone.lock().unwrap();
                        // Add samples to ring buffer
                        for sample in frame.samples {
                            // If buffer is full, drop oldest samples
                            if buffer.len() >= buffer_size {
                                buffer.remove(0);
                            }
                            buffer.push(sample);
                        }
                    }
                    tracing::warn!(
                        "Playback buffer fill task ended after {} frames",
                        frame_count
                    );
                });
                let stream = match sample_format {
                    SampleFormat::I8 => Self::build_output_stream::<i8>(
                        &device,
                        &stream_config,
                        ring_buffer.clone(),
                        volume,
                    ),
                    SampleFormat::I16 => Self::build_output_stream::<i16>(
                        &device,
                        &stream_config,
                        ring_buffer.clone(),
                        volume,
                    ),
                    SampleFormat::I32 => Self::build_output_stream::<i32>(
                        &device,
                        &stream_config,
                        ring_buffer.clone(),
                        volume,
                    ),
                    SampleFormat::I64 => Self::build_output_stream::<i64>(
                        &device,
                        &stream_config,
                        ring_buffer.clone(),
                        volume,
                    ),
                    SampleFormat::U8 => Self::build_output_stream::<u8>(
                        &device,
                        &stream_config,
                        ring_buffer.clone(),
                        volume,
                    ),
                    SampleFormat::U16 => Self::build_output_stream::<u16>(
                        &device,
                        &stream_config,
                        ring_buffer.clone(),
                        volume,
                    ),
                    SampleFormat::U32 => Self::build_output_stream::<u32>(
                        &device,
                        &stream_config,
                        ring_buffer.clone(),
                        volume,
                    ),
                    SampleFormat::U64 => Self::build_output_stream::<u64>(
                        &device,
                        &stream_config,
                        ring_buffer.clone(),
                        volume,
                    ),
                    SampleFormat::F32 => Self::build_output_stream::<f32>(
                        &device,
                        &stream_config,
                        ring_buffer.clone(),
                        volume,
                    ),
                    SampleFormat::F64 => Self::build_output_stream::<f64>(
                        &device,
                        &stream_config,
                        ring_buffer.clone(),
                        volume,
                    ),
                    _ => {
                        tracing::error!("Unsupported sample format: {:?}", sample_format);
                        return;
                    }
                };

                let stream = match stream {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::error!("Failed to build output stream: {}", err);
                        return;
                    }
                };

                tracing::info!("Starting playback stream with play()");
                if let Err(err) = stream.play() {
                    tracing::error!("Failed to play stream: {}", err);
                    return;
                }
                tracing::info!("Playback stream is now playing");

                while let Some(command) = command_rx.blocking_recv() {
                    match command {
                        Command::Mute(mute) => {
                            if mute {
                                if let Err(err) = stream.pause() {
                                    tracing::error!("Failed to pause stream: {}", err);
                                }
                            } else {
                                if let Err(err) = stream.play() {
                                    tracing::error!("Failed to play stream: {}", err);
                                }
                            }
                        }
                    }
                }
                tracing::debug!("Stopping playback stream");
                if let Err(err) = stream.pause() {
                    tracing::error!("Failed to stop stream: {}", err);
                }
                // Cancel the buffer fill task
                buffer_fill_task.abort();
            })
        };

        Ok(PlaybackStream {
            command_tx,
            volume,
            tx,
            task,
            sample_rate,
            channels,
        })
    }

    pub async fn send(&mut self, frame: AudioFrame) -> Result<()> {
        self.tx
            .send(frame)
            .await
            .map_err(|_| anyhow!("Failed to send audio frame: channel closed"))
    }

    pub fn try_send(&mut self, frame: AudioFrame) -> Result<()> {
        self.tx.try_send(frame).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => anyhow!("Audio buffer full"),
            mpsc::error::TrySendError::Closed(_) => anyhow!("Audio channel closed"),
        })
    }

    pub async fn set_mute(&mut self, mute: bool) {
        if let Err(err) = self.command_tx.send(Command::Mute(mute)).await {
            tracing::error!("Failed to send mute command: {}", err);
        }
    }

    pub async fn set_volume(&mut self, volume: f32) {
        self.volume.store(volume.to_bits(), Ordering::Relaxed);
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn get_buffer_space(&self) -> usize {
        self.tx.capacity()
    }

    fn build_output_stream<T>(
        device: &Device,
        config: &StreamConfig,
        ring_buffer: Arc<std::sync::Mutex<Vec<f32>>>,
        volume: Arc<AtomicU32>,
    ) -> Result<Stream>
    where
        T: SizedSample + FromSample<f32>,
        f32: FromSample<T>,
    {
        let callback_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let data_fn = move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let count = callback_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if count % 100 == 0 {
                tracing::debug!(
                    "Audio output callback #{}, buffer size needed: {}",
                    count,
                    data.len()
                );
            }
            let vol = f32::from_bits(volume.load(Ordering::Relaxed));
            let mut buffer = ring_buffer.lock().unwrap();

            // Process in chunks for better performance
            let samples_needed = data.len();
            let samples_available = buffer.len();

            if count % 100 == 0 {
                tracing::debug!(
                    "Playback buffer has {} samples, need {}",
                    samples_available,
                    samples_needed
                );
            }
            if samples_available >= samples_needed {
                // Fast path: we have enough samples
                for (i, sample) in data.iter_mut().enumerate() {
                    *sample = T::from_sample(buffer[i] * vol);
                }
                buffer.drain(0..samples_needed);
            } else if samples_available > 0 {
                // Partial buffer: play what we have, then silence
                // DO NOT stretch - stretching changes pitch!
                for (i, sample) in data.iter_mut().enumerate() {
                    if i < samples_available {
                        *sample = T::from_sample(buffer[i] * vol);
                    } else {
                        // Fill remainder with silence to avoid pitch shift
                        *sample = T::from_sample(0.0);
                    }
                }
                buffer.clear();

                // Log underrun for debugging
                tracing::trace!(
                    "Audio underrun: needed {}, had {} (filled rest with silence)",
                    samples_needed,
                    samples_available
                );
            } else {
                // No samples available: fill with silence
                for sample in data.iter_mut() {
                    *sample = T::from_sample(0.0);
                }

                // Don't log every underrun to avoid spam
                static UNDERRUN_COUNTER: std::sync::atomic::AtomicU32 =
                    std::sync::atomic::AtomicU32::new(0);
                if UNDERRUN_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 100 == 0 {
                    tracing::debug!("Audio buffer empty (underrun)");
                }
            }
        };
        let err_fn = |err| {
            tracing::error!("Audio playback stream error: {}", err);
        };
        device
            .build_output_stream(config, data_fn, err_fn, None)
            .map_err(|e| anyhow!("Failed to build output stream: {}", e))
    }
}

impl Drop for PlaybackStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}
