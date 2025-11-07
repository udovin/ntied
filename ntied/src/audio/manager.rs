use anyhow::{Result, anyhow};
use cpal::Device;
use cpal::traits::{DeviceTrait, HostTrait};

/// Simplified audio manager for device management
pub struct AudioManager;

#[derive(Debug, Clone)]
pub struct AudioDevice {
    pub name: String,
    pub is_default: bool,
    pub device_type: DeviceType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    Input,
    Output,
}

impl AudioManager {
    /// List available input devices
    pub async fn list_input_devices() -> Result<Vec<AudioDevice>> {
        tokio::task::spawn_blocking(|| {
            let host = cpal::default_host();
            let mut devices = Vec::new();

            let default_name = host.default_input_device().and_then(|d| d.name().ok());

            for device in host.input_devices()? {
                if let Ok(name) = device.name() {
                    let is_default = default_name.as_ref().map(|n| n == &name).unwrap_or(false);

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

            let default_name = host.default_output_device().and_then(|d| d.name().ok());

            for device in host.output_devices()? {
                if let Ok(name) = device.name() {
                    let is_default = default_name.as_ref().map(|n| n == &name).unwrap_or(false);

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

    /// Get a specific input device by name, or default if name is None
    pub async fn get_input_device(device_name: Option<String>) -> Result<Device> {
        tokio::task::spawn_blocking(move || {
            let host = cpal::default_host();

            if let Some(name) = device_name {
                tracing::info!("Getting input device: {}", name);
                host.input_devices()?
                    .find(|d| d.name().ok() == Some(name.clone()))
                    .ok_or_else(|| anyhow!("Input device not found: {}", name))
            } else {
                tracing::info!("Getting default input device");
                host.default_input_device()
                    .ok_or_else(|| anyhow!("No default input device"))
            }
        })
        .await?
    }

    /// Get a specific output device by name, or default if name is None
    pub async fn get_output_device(device_name: Option<String>) -> Result<Device> {
        tokio::task::spawn_blocking(move || {
            let host = cpal::default_host();

            if let Some(name) = device_name {
                tracing::info!("Getting output device: {}", name);
                host.output_devices()?
                    .find(|d| d.name().ok() == Some(name.clone()))
                    .ok_or_else(|| anyhow!("Output device not found: {}", name))
            } else {
                tracing::info!("Getting default output device");
                host.default_output_device()
                    .ok_or_else(|| anyhow!("No default output device"))
            }
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_input_devices() {
        let result = AudioManager::list_input_devices().await;
        // May fail on systems without audio devices
        if let Ok(devices) = result {
            println!("Input devices: {:?}", devices);
        }
    }

    #[tokio::test]
    async fn test_list_output_devices() {
        let result = AudioManager::list_output_devices().await;
        // May fail on systems without audio devices
        if let Ok(devices) = result {
            println!("Output devices: {:?}", devices);
        }
    }

    #[tokio::test]
    async fn test_get_default_input_device() {
        let result = AudioManager::get_input_device(None).await;
        // May fail on systems without audio devices
        if let Ok(device) = result {
            println!("Default input device: {:?}", device.name());
        }
    }

    #[tokio::test]
    async fn test_get_default_output_device() {
        let result = AudioManager::get_output_device(None).await;
        // May fail on systems without audio devices
        if let Ok(device) = result {
            println!("Default output device: {:?}", device.name());
        }
    }
}
