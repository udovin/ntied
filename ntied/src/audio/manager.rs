use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait};
use tokio::sync::{RwLock, mpsc};

use super::jitter_buffer::{JitterBuffer, JitterBufferStats};
use super::resampler::Resampler;
use super::{AudioFrame, CaptureStream, PlaybackStream};

pub struct AudioManager {
    capture_stream: Arc<RwLock<Option<CaptureStream>>>,
    playback_stream: Arc<RwLock<Option<PlaybackStream>>>,
    jitter_buffer: Arc<RwLock<JitterBuffer>>,
    sequence_counter: Arc<AtomicU32>,
    current_input_device: Arc<RwLock<Option<String>>>,
    current_output_device: Arc<RwLock<Option<String>>>,
    playback_resampler: Arc<RwLock<Option<Resampler>>>,
}

impl AudioManager {
    pub fn new() -> Self {
        Self {
            capture_stream: Arc::new(RwLock::new(None)),
            playback_stream: Arc::new(RwLock::new(None)),
            jitter_buffer: Arc::new(RwLock::new(JitterBuffer::with_config(100, 400))),
            sequence_counter: Arc::new(AtomicU32::new(0)),
            current_input_device: Arc::new(RwLock::new(None)),
            current_output_device: Arc::new(RwLock::new(None)),
            playback_resampler: Arc::new(RwLock::new(None)),
        }
    }

    /// List available input devices
    pub async fn list_input_devices() -> Result<Vec<AudioDevice>> {
        tokio::task::spawn_blocking(|| {
            let host = cpal::default_host();
            let mut devices = Vec::new();

            for device in host.input_devices()? {
                if let Ok(name) = device.name() {
                    let is_default = host
                        .default_input_device()
                        .and_then(|d| d.name().ok())
                        .map(|n| n == name)
                        .unwrap_or(false);

                    devices.push(AudioDevice {
                        name,
                        is_default,
                        device_type: DeviceType::Input,
                    });
                }
            }

            Ok::<Vec<AudioDevice>, anyhow::Error>(devices)
        })
        .await?
    }

    /// List available output devices
    pub async fn list_output_devices() -> Result<Vec<AudioDevice>> {
        tokio::task::spawn_blocking(|| {
            let host = cpal::default_host();
            let mut devices = Vec::new();

            for device in host.output_devices()? {
                if let Ok(name) = device.name() {
                    let is_default = host
                        .default_output_device()
                        .and_then(|d| d.name().ok())
                        .map(|n| n == name)
                        .unwrap_or(false);

                    devices.push(AudioDevice {
                        name,
                        is_default,
                        device_type: DeviceType::Output,
                    });
                }
            }

            Ok::<Vec<AudioDevice>, anyhow::Error>(devices)
        })
        .await?
    }

    /// Start audio capture with optional device selection
    pub async fn start_capture(
        &self,
        device_name: Option<String>,
        volume: f32,
    ) -> Result<mpsc::Receiver<AudioFrame>> {
        // Stop existing capture if any
        self.stop_capture().await?;

        // Get device in a blocking task
        let device_name_clone = device_name.clone();
        let device = tokio::task::spawn_blocking(move || {
            let host = cpal::default_host();
            if let Some(name) = device_name_clone {
                tracing::info!("Starting capture with device: {}", name);
                host.input_devices()?
                    .find(|d| d.name().ok() == Some(name.clone()))
                    .ok_or_else(|| anyhow!("Input device not found: {}", name))
            } else {
                tracing::info!("Starting capture with default input device");
                host.default_input_device()
                    .ok_or_else(|| anyhow!("No default input device"))
            }
        })
        .await??;

        // Create capture stream
        let capture_stream = CaptureStream::new(device, volume).await?;

        // Get stream info before storing
        let sample_rate = capture_stream.sample_rate();
        let channels = capture_stream.channels();

        // Store capture stream handle
        let mut stream_guard = self.capture_stream.write().await;
        *stream_guard = Some(capture_stream);

        // Create channel for forwarding frames
        let (tx, rx) = mpsc::channel(100);

        // Spawn task to receive from capture stream and forward
        let capture_stream_clone = self.capture_stream.clone();
        let _capture_task = tokio::spawn(async move {
            loop {
                // Get frame from capture stream
                let frame = {
                    let mut guard = capture_stream_clone.write().await;
                    if let Some(ref mut stream) = *guard {
                        stream.recv().await
                    } else {
                        None
                    }
                };

                match frame {
                    Some(frame) => {
                        if tx.send(frame).await.is_err() {
                            break; // Receiver dropped
                        }
                    }
                    None => {
                        break; // Stream ended
                    }
                }
            }
        });

        // Store current device name
        let mut current = self.current_input_device.write().await;
        *current = device_name.or_else(|| {
            // Try to get default device name
            std::thread::spawn(|| {
                cpal::default_host()
                    .default_input_device()
                    .and_then(|d| d.name().ok())
            })
            .join()
            .ok()
            .flatten()
        });

        tracing::info!(
            "Audio capture started: {} Hz, {} channels",
            sample_rate,
            channels
        );

        Ok(rx)
    }

    /// Stop audio capture
    pub async fn stop_capture(&self) -> Result<()> {
        let mut stream_guard = self.capture_stream.write().await;
        if stream_guard.take().is_some() {
            tracing::info!("Audio capture stopped");
        }

        let mut current = self.current_input_device.write().await;
        *current = None;

        Ok(())
    }

    /// Start audio playback with optional device selection
    pub async fn start_playback(&self, device_name: Option<String>, volume: f32) -> Result<()> {
        // Stop existing playback if any
        self.stop_playback().await?;

        // Get device in a blocking task
        let device_name_clone = device_name.clone();
        let device = tokio::task::spawn_blocking(move || {
            let host = cpal::default_host();
            if let Some(name) = device_name_clone {
                tracing::info!("Starting playback with device: {}", name);
                host.output_devices()?
                    .find(|d| d.name().ok() == Some(name.clone()))
                    .ok_or_else(|| anyhow!("Output device not found: {}", name))
            } else {
                tracing::info!("Starting playback with default output device");
                host.default_output_device()
                    .ok_or_else(|| anyhow!("No default output device"))
            }
        })
        .await??;

        // Create playback stream
        let playback_stream = PlaybackStream::new(device, volume).await?;

        let sample_rate = playback_stream.sample_rate();
        let channels = playback_stream.channels();

        // Store playback stream
        let mut stream_guard = self.playback_stream.write().await;
        *stream_guard = Some(playback_stream);

        // Initialize resampler for playback (will configure it when we know the input rate)
        let mut resampler_guard = self.playback_resampler.write().await;
        *resampler_guard = None; // Will be created when we receive first audio frame

        // Store current device name
        let mut current = self.current_output_device.write().await;
        *current = device_name.or_else(|| {
            // Try to get default device name
            std::thread::spawn(|| {
                cpal::default_host()
                    .default_output_device()
                    .and_then(|d| d.name().ok())
            })
            .join()
            .ok()
            .flatten()
        });

        // Reset jitter buffer for new session
        let mut jitter = self.jitter_buffer.write().await;
        jitter.reset();

        tracing::info!(
            "Audio playback started: {} Hz, {} channels",
            sample_rate,
            channels
        );

        Ok(())
    }

    /// Stop audio playback
    pub async fn stop_playback(&self) -> Result<()> {
        let mut stream_guard = self.playback_stream.write().await;
        if stream_guard.take().is_some() {
            tracing::info!("Audio playback stopped");
        }

        let mut current = self.current_output_device.write().await;
        *current = None;

        // Clear jitter buffer
        let mut jitter = self.jitter_buffer.write().await;
        jitter.reset();

        // Clear resampler
        let mut resampler = self.playback_resampler.write().await;
        *resampler = None;

        Ok(())
    }

    /// Queue received audio frame for playback (with jitter buffer)
    pub async fn queue_audio_frame(
        &self,
        sequence: u32,
        samples: Vec<f32>,
        sample_rate: u32,
        channels: u16,
    ) -> Result<()> {
        // Get playback stream sample rate to check if resampling is needed
        let playback_sample_rate = if let Some(ref stream) = *self.playback_stream.read().await {
            stream.sample_rate()
        } else {
            return Err(anyhow!("Playback stream not initialized"));
        };

        // Initialize or update resampler if needed
        let mut resampler_guard = self.playback_resampler.write().await;
        if resampler_guard.is_none()
            || resampler_guard.as_ref().unwrap().input_rate() != sample_rate
            || resampler_guard.as_ref().unwrap().output_rate() != playback_sample_rate
        {
            let ratio = sample_rate as f64 / playback_sample_rate as f64;
            let resampling_type = if ratio > 1.0 {
                "downsampling"
            } else if ratio < 1.0 {
                "upsampling"
            } else {
                "no resampling"
            };

            tracing::info!(
                "Initializing resampler: {} Hz -> {} Hz, {} channels, ratio: {:.4} ({})",
                sample_rate,
                playback_sample_rate,
                channels,
                ratio,
                resampling_type
            );
            // Create new resampler and reset its state for clean transitions
            let mut new_resampler = Resampler::new(sample_rate, playback_sample_rate, channels)?;
            new_resampler.reset();
            *resampler_guard = Some(new_resampler);
        }

        // Resample if necessary
        let resampled_samples = if sample_rate != playback_sample_rate {
            if let Some(ref mut resampler) = *resampler_guard {
                let original_len = samples.len();
                let expected_output_len = (original_len as f64 * playback_sample_rate as f64
                    / sample_rate as f64) as usize;
                let resampled = resampler.resample(&samples)?;

                // Check for unexpected output size
                let size_diff = (resampled.len() as i32 - expected_output_len as i32).abs();
                if size_diff > 2 {
                    tracing::warn!(
                        "Unexpected resampled size: {} samples @ {}Hz -> {} samples @ {}Hz (expected ~{})",
                        original_len,
                        sample_rate,
                        resampled.len(),
                        playback_sample_rate,
                        expected_output_len
                    );
                } else {
                    tracing::debug!(
                        "Resampled audio: {} samples @ {}Hz -> {} samples @ {}Hz (expected ~{})",
                        original_len,
                        sample_rate,
                        resampled.len(),
                        playback_sample_rate,
                        expected_output_len
                    );
                }
                resampled
            } else {
                tracing::warn!("Resampler not available, using original samples");
                samples
            }
        } else {
            samples
        };
        drop(resampler_guard);

        let frame = AudioFrame {
            samples: resampled_samples,
            sample_rate: playback_sample_rate, // Use playback sample rate after resampling
            channels,
            timestamp: Instant::now(),
        };

        // Add to jitter buffer
        let mut jitter = self.jitter_buffer.write().await;
        let is_new = jitter.push(sequence, frame);

        if !is_new {
            tracing::debug!("Duplicate packet received: sequence {}", sequence);
        }

        // Try to drain jitter buffer and send to playback
        if let Some(ref mut stream) = *self.playback_stream.write().await {
            // Start draining once buffer is ready (has minimum frames)
            // This provides consistent initial buffering
            if jitter.is_ready() {
                // Pop frames one at a time to maintain steady flow
                // Limit frames per cycle to prevent overwhelming playback buffer
                let mut frames_sent = 0;
                let max_frames_per_cycle = 3; // Conservative limit to prevent bursts

                // Keep trying to pop frames while available and not hitting limits
                while frames_sent < max_frames_per_cycle {
                    if let Some(buffered_frame) = jitter.pop() {
                        // Try to send without blocking the audio thread
                        match stream.try_send(buffered_frame) {
                            Ok(_) => {
                                frames_sent += 1;
                            }
                            Err(_) => {
                                // Playback buffer is full, stop sending
                                // This is normal backpressure
                                if frames_sent == 0 {
                                    tracing::trace!(
                                        "Playback buffer full, will retry on next packet"
                                    );
                                }
                                break;
                            }
                        }
                    } else {
                        // No more frames available in jitter buffer
                        break;
                    }
                }

                if frames_sent > 0 {
                    tracing::trace!(
                        "Sent {} frames to playback, jitter buffer: {} frames, avg jitter: {:.1}ms",
                        frames_sent,
                        jitter.len(),
                        jitter.stats().average_jitter_ms
                    );
                }
            } else {
                // Buffer is still warming up
                tracing::trace!(
                    "Jitter buffer warming up: {}/{} frames",
                    jitter.len(),
                    (jitter.stats().current_delay_ms / 20.0).ceil() as usize
                );
            }

            // Periodically cleanup old packets that weren't played
            jitter.cleanup_old_packets();
        }

        Ok(())
    }

    /// Send captured audio frame (returns sequence number)
    pub async fn prepare_audio_frame(&self, frame: AudioFrame) -> (u32, AudioFrame) {
        let sequence = self.sequence_counter.fetch_add(1, Ordering::Relaxed);
        (sequence, frame)
    }

    /// Set capture volume (0.0 to 2.0)
    pub async fn set_capture_volume(&self, volume: f32) -> Result<()> {
        if let Some(ref mut stream) = *self.capture_stream.write().await {
            stream.set_volume(volume).await;
        }
        Ok(())
    }

    /// Set playback volume (0.0 to 2.0)
    pub async fn set_playback_volume(&self, volume: f32) -> Result<()> {
        if let Some(ref mut stream) = *self.playback_stream.write().await {
            stream.set_volume(volume).await;
        }
        Ok(())
    }

    /// Mute/unmute capture
    pub async fn set_capture_mute(&self, mute: bool) -> Result<()> {
        if let Some(ref mut stream) = *self.capture_stream.write().await {
            stream.set_mute(mute).await;
        }
        Ok(())
    }

    /// Mute/unmute playback
    pub async fn set_playback_mute(&self, mute: bool) -> Result<()> {
        if let Some(ref mut stream) = *self.playback_stream.write().await {
            stream.set_mute(mute).await;
        }
        Ok(())
    }

    /// Get current input device name
    pub async fn get_current_input_device(&self) -> Option<String> {
        self.current_input_device.read().await.clone()
    }

    /// Get current output device name
    pub async fn get_current_output_device(&self) -> Option<String> {
        self.current_output_device.read().await.clone()
    }

    /// Get jitter buffer statistics
    pub async fn get_stats(&self) -> JitterBufferStats {
        self.jitter_buffer.read().await.stats().clone()
    }

    /// Reset sequence counter (useful when starting a new call)
    pub fn reset_sequence(&self) {
        self.sequence_counter.store(0, Ordering::Relaxed);
    }

    /// Is capture active
    pub async fn is_capturing(&self) -> bool {
        self.capture_stream.read().await.is_some()
    }

    /// Is playback active
    pub async fn is_playing(&self) -> bool {
        self.playback_stream.read().await.is_some()
    }
}

#[derive(Clone, Debug)]
pub struct AudioDevice {
    pub name: String,
    pub is_default: bool,
    pub device_type: DeviceType,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeviceType {
    Input,
    Output,
}

impl Default for AudioManager {
    fn default() -> Self {
        Self::new()
    }
}
