use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, Sample, SampleFormat, StreamConfig};
use tokio::sync::mpsc;

use super::AudioCodec;

pub struct AudioCapture {
    codec: Arc<AudioCodec>,
    is_running: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    stop_tx: Option<mpsc::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl AudioCapture {
    pub fn new(
        _device: Device,
        codec: Arc<AudioCodec>,
        _tx: mpsc::Sender<Vec<u8>>,
    ) -> Result<Self> {
        Ok(Self {
            codec,
            is_running: Arc::new(AtomicBool::new(false)),
            volume: Arc::new(AtomicU32::new(f32::to_bits(1.0))),
            stop_tx: None,
            handle: None,
        })
    }

    pub fn start(&mut self, device: Device, tx: mpsc::Sender<Vec<u8>>) -> Result<()> {
        if self.is_running.load(Ordering::Relaxed) {
            return Ok(());
        }

        let config = device.default_input_config()?;
        let sample_format = config.sample_format();
        let config: StreamConfig = config.into();

        // Create stop channel
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        self.stop_tx = Some(stop_tx);

        let codec = self.codec.clone();
        let is_running = self.is_running.clone();
        let volume = self.volume.clone();

        // Capture config details before moving
        let sample_rate = config.sample_rate.0;
        let channels = config.channels as usize;
        let samples_per_frame = (sample_rate as usize * 20) / 1000; // 20ms frames

        is_running.store(true, Ordering::Relaxed);

        // Spawn blocking task for audio capture
        let handle = tokio::task::spawn_blocking(move || {
            // Create runtime for this thread
            let runtime = tokio::runtime::Handle::current();

            // Buffer to accumulate samples
            let sample_buffer = Arc::new(std::sync::Mutex::new(Vec::new()));
            let sample_buffer_clone = sample_buffer.clone();

            let err_fn = |err| {
                tracing::error!("Audio capture stream error: {}", err);
            };

            // Build the stream based on sample format
            let stream_result = match sample_format {
                SampleFormat::F32 => build_input_stream::<f32>(
                    &device,
                    &config,
                    sample_buffer_clone,
                    samples_per_frame,
                    channels,
                    volume.clone(),
                    is_running.clone(),
                    codec.clone(),
                    tx.clone(),
                    runtime.clone(),
                    err_fn,
                ),
                SampleFormat::I16 => build_input_stream::<i16>(
                    &device,
                    &config,
                    sample_buffer_clone,
                    samples_per_frame,
                    channels,
                    volume.clone(),
                    is_running.clone(),
                    codec.clone(),
                    tx.clone(),
                    runtime.clone(),
                    err_fn,
                ),
                SampleFormat::I32 => build_input_stream::<i32>(
                    &device,
                    &config,
                    sample_buffer_clone,
                    samples_per_frame,
                    channels,
                    volume.clone(),
                    is_running.clone(),
                    codec.clone(),
                    tx.clone(),
                    runtime.clone(),
                    err_fn,
                ),
                SampleFormat::I8 => build_input_stream::<i8>(
                    &device,
                    &config,
                    sample_buffer_clone,
                    samples_per_frame,
                    channels,
                    volume.clone(),
                    is_running.clone(),
                    codec.clone(),
                    tx.clone(),
                    runtime.clone(),
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
}

fn build_input_stream<T>(
    device: &Device,
    config: &StreamConfig,
    sample_buffer: Arc<std::sync::Mutex<Vec<f32>>>,
    samples_per_frame: usize,
    channels: usize,
    volume: Arc<AtomicU32>,
    is_running: Arc<AtomicBool>,
    codec: Arc<AudioCodec>,
    tx: mpsc::Sender<Vec<u8>>,
    runtime: tokio::runtime::Handle,
    err_fn: impl Fn(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
    f32: cpal::FromSample<T>,
{
    let data_fn = move |data: &[T], _: &cpal::InputCallbackInfo| {
        if !is_running.load(Ordering::Relaxed) {
            return;
        }

        let vol = f32::from_bits(volume.load(Ordering::Relaxed));

        // Convert samples to f32 and apply volume
        let mut buffer = sample_buffer.lock().unwrap();
        for sample in data {
            let sample_f32 = f32::from_sample(*sample) * vol;
            buffer.push(sample_f32);
        }

        // Process complete frames
        while buffer.len() >= samples_per_frame * channels {
            let frame: Vec<f32> = buffer.drain(..samples_per_frame * channels).collect();

            // Encode the frame
            if let Ok(encoded) = codec.encode(&frame) {
                // Send encoded audio data using runtime handle
                let tx_clone = tx.clone();
                runtime.spawn(async move {
                    let _ = tx_clone.send(encoded).await;
                });
            }
        }
    };

    Ok(device.build_input_stream(config, data_fn, err_fn, None)?)
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        // Cannot call async stop in drop, just mark as not running
        self.is_running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}
