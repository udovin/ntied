use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, SampleFormat, StreamConfig};
use tokio::sync::mpsc;

use super::AudioCodec;

pub struct AudioPlayback {
    codec: Arc<AudioCodec>,
    is_running: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    audio_tx: Option<mpsc::Sender<Vec<u8>>>,
    stop_tx: Option<mpsc::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl AudioPlayback {
    pub fn new(_device: Device, codec: Arc<AudioCodec>) -> Result<Self> {
        Ok(Self {
            codec,
            is_running: Arc::new(AtomicBool::new(false)),
            volume: Arc::new(AtomicU32::new(f32::to_bits(1.0))),
            audio_tx: None,
            stop_tx: None,
            handle: None,
        })
    }

    pub fn start(&mut self, device: Device) -> Result<()> {
        if self.is_running.load(Ordering::Relaxed) {
            return Ok(());
        }

        let config = device.default_output_config()?;
        let sample_format = config.sample_format();
        let config: StreamConfig = config.into();

        // Create channels
        let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<u8>>(100);
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);

        self.audio_tx = Some(audio_tx);
        self.stop_tx = Some(stop_tx);

        let codec = self.codec.clone();
        let is_running = self.is_running.clone();
        let volume = self.volume.clone();

        // Capture config details
        let channels = config.channels as usize;

        is_running.store(true, Ordering::Relaxed);

        // Spawn blocking task for audio playback
        let handle = tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Handle::current();

            // Ring buffer for decoded samples
            let buffer_size = 48000 * 2; // 1 second at 48kHz stereo
            let ring_buffer = Arc::new(std::sync::Mutex::new(Vec::<f32>::with_capacity(
                buffer_size,
            )));
            let ring_buffer_clone = ring_buffer.clone();

            // Spawn task to receive and decode audio
            let codec_clone = codec.clone();
            let is_running_clone = is_running.clone();
            runtime.spawn(async move {
                while is_running_clone.load(Ordering::Relaxed) {
                    tokio::select! {
                        Some(encoded) = audio_rx.recv() => {
                            // Decode audio data
                            if let Ok(samples) = codec_clone.decode(&encoded) {
                                let mut buffer = ring_buffer_clone.lock().unwrap();
                                for sample in samples {
                                    // If buffer is full, remove oldest samples
                                    if buffer.len() >= buffer_size {
                                        buffer.remove(0);
                                    }
                                    buffer.push(sample);
                                }
                            }
                        }
                        else => {
                            break;
                        }
                    }
                }
            });

            let err_fn = |err| {
                tracing::error!("Audio playback stream error: {}", err);
            };

            // Build the stream based on sample format
            let stream_result = match sample_format {
                SampleFormat::F32 => build_output_stream::<f32>(
                    &device,
                    &config,
                    ring_buffer.clone(),
                    channels,
                    volume.clone(),
                    is_running.clone(),
                    err_fn,
                ),
                SampleFormat::I16 => build_output_stream::<i16>(
                    &device,
                    &config,
                    ring_buffer.clone(),
                    channels,
                    volume.clone(),
                    is_running.clone(),
                    err_fn,
                ),
                SampleFormat::I32 => build_output_stream::<i32>(
                    &device,
                    &config,
                    ring_buffer.clone(),
                    channels,
                    volume.clone(),
                    is_running.clone(),
                    err_fn,
                ),
                SampleFormat::I8 => build_output_stream::<i8>(
                    &device,
                    &config,
                    ring_buffer.clone(),
                    channels,
                    volume.clone(),
                    is_running.clone(),
                    err_fn,
                ),
                _ => Err(anyhow!("Unsupported sample format")),
            };

            match stream_result {
                Ok(stream) => {
                    if let Err(e) = stream.play() {
                        tracing::error!("Failed to play stream: {}", e);
                        return;
                    }

                    // Block until stop signal
                    runtime.block_on(async {
                        stop_rx.recv().await;
                    });

                    // Stop the stream
                    let _ = stream.pause();
                }
                Err(e) => {
                    tracing::error!("Failed to build stream: {}", e);
                }
            }

            is_running.store(false, Ordering::Relaxed);
        });

        self.handle = Some(handle);
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        self.is_running.store(false, Ordering::Relaxed);

        // Close audio channel
        self.audio_tx = None;

        // Send stop signal
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(()).await;
        }

        // Wait for thread to finish
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }

        Ok(())
    }

    pub fn queue_audio(&self, encoded_data: Vec<u8>) -> Result<()> {
        if !self.is_running.load(Ordering::Relaxed) {
            return Err(anyhow!("Playback not running"));
        }

        if let Some(tx) = &self.audio_tx {
            // Use try_send to avoid blocking
            tx.try_send(encoded_data)
                .map_err(|e| anyhow!("Failed to queue audio: {}", e))?;
        } else {
            return Err(anyhow!("Audio channel not available"));
        }

        Ok(())
    }

    pub fn set_volume(&self, volume: f32) -> Result<()> {
        if volume < 0.0 || volume > 2.0 {
            return Err(anyhow!("Volume must be between 0.0 and 2.0"));
        }
        self.volume.store(volume.to_bits(), Ordering::Relaxed);
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::Relaxed)
    }

    pub fn get_buffer_level(&self) -> f32 {
        // TODO: Implement buffer level monitoring
        0.0
    }
}

fn build_output_stream<T>(
    device: &Device,
    config: &StreamConfig,
    ring_buffer: Arc<std::sync::Mutex<Vec<f32>>>,
    _channels: usize,
    volume: Arc<AtomicU32>,
    is_running: Arc<AtomicBool>,
    err_fn: impl Fn(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
    f32: cpal::FromSample<T>,
{
    let data_fn = move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
        if !is_running.load(Ordering::Relaxed) {
            // Fill with silence
            for sample in data.iter_mut() {
                *sample = T::from_sample(0.0);
            }
            return;
        }

        let vol = f32::from_bits(volume.load(Ordering::Relaxed));
        let mut buffer = ring_buffer.lock().unwrap();

        for sample in data.iter_mut() {
            // Get next sample from buffer or use silence
            let value = if !buffer.is_empty() {
                buffer.remove(0) * vol
            } else {
                0.0
            };
            *sample = T::from_sample(value);
        }
    };

    Ok(device.build_output_stream(config, data_fn, err_fn, None)?)
}

impl Drop for AudioPlayback {
    fn drop(&mut self) {
        // Cannot call async stop in drop, just mark as not running
        self.is_running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}
