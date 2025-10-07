use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait};
use tokio::sync::{RwLock, mpsc};

use super::{AudioCapture, AudioCodec, AudioFormat, AudioPlayback};

pub struct AudioManager {
    capture: Arc<RwLock<Option<AudioCapture>>>,
    playback: Arc<RwLock<Option<AudioPlayback>>>,
    codec: Arc<AudioCodec>,
    is_capturing: Arc<AtomicBool>,
    is_playing: Arc<AtomicBool>,
    current_input_device: Arc<RwLock<Option<String>>>,
    current_output_device: Arc<RwLock<Option<String>>>,
}

impl AudioManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            capture: Arc::new(RwLock::new(None)),
            playback: Arc::new(RwLock::new(None)),
            codec: Arc::new(AudioCodec::new(AudioFormat::default())),
            is_capturing: Arc::new(AtomicBool::new(false)),
            is_playing: Arc::new(AtomicBool::new(false)),
            current_input_device: Arc::new(RwLock::new(None)),
            current_output_device: Arc::new(RwLock::new(None)),
        })
    }

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

    pub async fn start_capture(
        &self,
        device_name: Option<String>,
    ) -> Result<mpsc::Receiver<Vec<u8>>> {
        if self.is_capturing.load(Ordering::Relaxed) {
            return Err(anyhow!("Already capturing"));
        }

        // Get device in a blocking task
        let device_name_clone = device_name.clone();
        let device = tokio::task::spawn_blocking(move || {
            let host = cpal::default_host();
            if let Some(name) = device_name_clone {
                tracing::info!("Switching to input device: {}", name);
                host.input_devices()?
                    .find(|d| d.name().ok() == Some(name.clone()))
                    .ok_or_else(|| anyhow!("Input device not found: {}", name))
            } else {
                tracing::info!("Switching to default input device");
                host.default_input_device()
                    .ok_or_else(|| anyhow!("No default input device"))
            }
        })
        .await??;

        let (tx, rx) = mpsc::channel(100);

        let mut capture = AudioCapture::new(device.clone(), self.codec.clone(), tx.clone())?;
        capture.start(device, tx)?;

        let mut capture_guard = self.capture.write().await;
        *capture_guard = Some(capture);

        // Store the current input device name
        let mut current_device = self.current_input_device.write().await;
        if let Some(name) = device_name {
            *current_device = Some(name);
        } else {
            // Get the default device name
            let default_name = tokio::task::spawn_blocking(|| {
                cpal::default_host()
                    .default_input_device()
                    .and_then(|d| d.name().ok())
            })
            .await
            .unwrap_or(None);
            *current_device = default_name;
        }

        self.is_capturing.store(true, Ordering::Relaxed);
        tracing::info!("Audio capture started successfully");

        Ok(rx)
    }

    pub async fn stop_capture(&self) -> Result<()> {
        tracing::debug!("Stopping audio capture");
        let mut capture = self.capture.write().await;
        if let Some(mut c) = capture.take() {
            c.stop().await?;
        }

        self.is_capturing.store(false, Ordering::Relaxed);

        // Clear current device
        let mut current_device = self.current_input_device.write().await;
        *current_device = None;

        tracing::info!("Audio capture stopped");
        Ok(())
    }

    pub async fn start_playback(&self, device_name: Option<String>) -> Result<()> {
        if self.is_playing.load(Ordering::Relaxed) {
            return Err(anyhow!("Already playing"));
        }

        // Get device in a blocking task
        let device_name_clone = device_name.clone();
        let device = tokio::task::spawn_blocking(move || {
            let host = cpal::default_host();
            if let Some(name) = device_name_clone {
                tracing::info!("Switching to output device: {}", name);
                host.output_devices()?
                    .find(|d| d.name().ok() == Some(name.clone()))
                    .ok_or_else(|| anyhow!("Output device not found: {}", name))
            } else {
                tracing::info!("Switching to default output device");
                host.default_output_device()
                    .ok_or_else(|| anyhow!("No default output device"))
            }
        })
        .await??;

        let mut playback = AudioPlayback::new(device.clone(), self.codec.clone())?;
        playback.start(device)?;

        let mut playback_guard = self.playback.write().await;
        *playback_guard = Some(playback);

        // Store the current output device name
        let mut current_device = self.current_output_device.write().await;
        if let Some(name) = device_name {
            *current_device = Some(name);
        } else {
            // Get the default device name
            let default_name = tokio::task::spawn_blocking(|| {
                cpal::default_host()
                    .default_output_device()
                    .and_then(|d| d.name().ok())
            })
            .await
            .unwrap_or(None);
            *current_device = default_name;
        }

        self.is_playing.store(true, Ordering::Relaxed);
        tracing::info!("Audio playback started successfully");

        Ok(())
    }

    pub async fn stop_playback(&self) -> Result<()> {
        tracing::debug!("Stopping audio playback");
        let mut playback = self.playback.write().await;
        if let Some(mut p) = playback.take() {
            p.stop().await?;
        }

        self.is_playing.store(false, Ordering::Relaxed);

        // Clear current device
        let mut current_device = self.current_output_device.write().await;
        *current_device = None;

        tracing::info!("Audio playback stopped");
        Ok(())
    }

    pub async fn play_audio(&self, data: Vec<u8>) -> Result<()> {
        let playback = self.playback.read().await;
        if let Some(p) = playback.as_ref() {
            p.queue_audio(data)?;
        } else {
            return Err(anyhow!("Playback not started"));
        }
        Ok(())
    }

    pub async fn set_input_volume(&self, volume: f32) -> Result<()> {
        let capture = self.capture.read().await;
        if let Some(c) = capture.as_ref() {
            c.set_volume(volume)?;
        }
        Ok(())
    }

    pub async fn set_output_volume(&self, volume: f32) -> Result<()> {
        let playback = self.playback.read().await;
        if let Some(p) = playback.as_ref() {
            p.set_volume(volume)?;
        }
        Ok(())
    }

    pub fn is_capturing(&self) -> bool {
        self.is_capturing.load(Ordering::Relaxed)
    }

    pub fn is_playing(&self) -> bool {
        self.is_playing.load(Ordering::Relaxed)
    }

    pub async fn get_current_input_device(&self) -> Option<String> {
        self.current_input_device.read().await.clone()
    }

    pub async fn get_current_output_device(&self) -> Option<String> {
        self.current_output_device.read().await.clone()
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

impl Drop for AudioManager {
    fn drop(&mut self) {
        // Cleanup will be handled by individual components
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let capture = self.capture.clone();
            let playback = self.playback.clone();

            // Clean up in background
            runtime.spawn(async move {
                if let Some(mut c) = capture.write().await.take() {
                    let _ = c.stop().await;
                }
                if let Some(mut p) = playback.write().await.take() {
                    let _ = p.stop().await;
                }
            });
        }
    }
}
