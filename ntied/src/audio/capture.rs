use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait as _, StreamTrait};
use cpal::{Device, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, spawn_blocking};

#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub samples: Vec<f32>, // Normalized samples [-1.0, 1.0]
    pub sample_rate: u32,
    pub channels: u16,
    pub timestamp: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioConfig {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate,
            channels,
        }
    }

    pub fn from_frame(frame: &AudioFrame) -> Self {
        Self {
            sample_rate: frame.sample_rate,
            channels: frame.channels,
        }
    }
}

enum Command {
    Mute(bool),
}

pub struct CaptureStream {
    command_tx: mpsc::Sender<Command>,
    volume: Arc<AtomicU32>,
    rx: mpsc::Receiver<AudioFrame>,
    task: JoinHandle<()>,
    sample_rate: u32,
    channels: u16,
}

impl CaptureStream {
    pub async fn new(device: Device, volume: f32) -> Result<Self> {
        let (command_tx, mut command_rx) = mpsc::channel(1);
        let (tx, rx) = mpsc::channel(100);
        let volume = Arc::new(AtomicU32::new(f32::to_bits(volume)));
        let config = device
            .default_input_config()
            .map_err(|e| anyhow!("Failed to get default input config: {}", e))?;
        let sample_format = config.sample_format();
        let sample_rate = config.sample_rate().0;
        let channels = config.channels();

        // Log device configuration
        tracing::info!(
            "Capture device config: {} Hz, {} channels, format: {:?}",
            sample_rate,
            channels,
            sample_format
        );

        // Warn if unusual sample rate
        if sample_rate != 44100 && sample_rate != 48000 && sample_rate != 96000 {
            tracing::warn!(
                "Capture device using non-standard sample rate: {} Hz",
                sample_rate
            );
        }

        let stream_config: StreamConfig = config.into();
        let task = {
            let volume = volume.clone();
            spawn_blocking(move || {
                let stream = match sample_format {
                    SampleFormat::I8 => Self::build_input_stream::<i8>(
                        &device,
                        &stream_config,
                        sample_rate,
                        channels,
                        volume,
                        tx,
                    ),
                    SampleFormat::I16 => Self::build_input_stream::<i16>(
                        &device,
                        &stream_config,
                        sample_rate,
                        channels,
                        volume,
                        tx,
                    ),
                    SampleFormat::I32 => Self::build_input_stream::<i32>(
                        &device,
                        &stream_config,
                        sample_rate,
                        channels,
                        volume,
                        tx,
                    ),
                    SampleFormat::I64 => Self::build_input_stream::<i64>(
                        &device,
                        &stream_config,
                        sample_rate,
                        channels,
                        volume,
                        tx,
                    ),
                    SampleFormat::U8 => Self::build_input_stream::<u8>(
                        &device,
                        &stream_config,
                        sample_rate,
                        channels,
                        volume,
                        tx,
                    ),
                    SampleFormat::U16 => Self::build_input_stream::<u16>(
                        &device,
                        &stream_config,
                        sample_rate,
                        channels,
                        volume,
                        tx,
                    ),
                    SampleFormat::U32 => Self::build_input_stream::<u32>(
                        &device,
                        &stream_config,
                        sample_rate,
                        channels,
                        volume,
                        tx,
                    ),
                    SampleFormat::U64 => Self::build_input_stream::<u64>(
                        &device,
                        &stream_config,
                        sample_rate,
                        channels,
                        volume,
                        tx,
                    ),
                    SampleFormat::F32 => Self::build_input_stream::<f32>(
                        &device,
                        &stream_config,
                        sample_rate,
                        channels,
                        volume,
                        tx,
                    ),
                    SampleFormat::F64 => Self::build_input_stream::<f64>(
                        &device,
                        &stream_config,
                        sample_rate,
                        channels,
                        volume,
                        tx,
                    ),
                    _ => {
                        tracing::error!("Unsupported sample format: {:?}", sample_format);
                        return;
                    }
                };
                let stream = match stream {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::error!("Failed to build input stream: {}", err);
                        return;
                    }
                };
                tracing::info!("Starting capture stream with play()");
                if let Err(err) = stream.play() {
                    tracing::error!("Failed to start stream: {}", err);
                    return;
                }
                tracing::info!("Capture stream is now playing");
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
                tracing::debug!("Stopping capture stream");
                if let Err(err) = stream.pause() {
                    tracing::error!("Failed to stop stream: {}", err);
                }
            })
        };
        Ok(CaptureStream {
            command_tx,
            volume,
            rx,
            task,
            sample_rate,
            channels,
        })
    }

    pub async fn recv(&mut self) -> Option<AudioFrame> {
        self.rx.recv().await
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

    fn build_input_stream<T>(
        device: &Device,
        config: &StreamConfig,
        sample_rate: u32,
        channels: u16,
        volume: Arc<AtomicU32>,
        tx: mpsc::Sender<AudioFrame>,
    ) -> Result<Stream>
    where
        T: SizedSample + FromSample<f32>,
        f32: FromSample<T>,
    {
        // Buffer for accumulating samples (20ms frames)
        let frame_size = ((sample_rate as usize) * 20 / 1000) * channels as usize;
        let sample_buffer = Arc::new(std::sync::Mutex::new(Vec::with_capacity(frame_size)));
        let callback_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let data_fn = move |data: &[T], _: &cpal::InputCallbackInfo| {
            let count = callback_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if count % 100 == 0 {
                tracing::debug!("Audio input callback #{}, samples: {}", count, data.len());
            }
            let vol = f32::from_bits(volume.load(Ordering::Relaxed));
            let mut buffer = sample_buffer.lock().unwrap();
            // Convert samples to f32 and apply volume
            for sample in data {
                let sample_f32 = f32::from_sample(*sample) * vol;
                buffer.push(sample_f32);
            }

            // Send complete frames
            while buffer.len() >= frame_size {
                let frame_samples: Vec<f32> = buffer.drain(..frame_size).collect();

                let frame = AudioFrame {
                    samples: frame_samples,
                    sample_rate,
                    channels,
                    timestamp: Instant::now(),
                };

                // Try to send, but don't block the audio thread
                match tx.try_send(frame) {
                    Ok(_) => {
                        if count % 100 == 0 {
                            tracing::debug!("Audio frame sent successfully");
                        }
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        tracing::warn!("Audio frame dropped: channel full");
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        tracing::warn!("Audio channel closed, stopping capture");
                    }
                }
            }
        };
        let err_fn = |err| {
            tracing::error!("Audio capture stream error: {}", err);
        };
        device
            .build_input_stream(config, data_fn, err_fn, None)
            .map_err(|e| anyhow!("Failed to build input stream: {}", e))
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}
