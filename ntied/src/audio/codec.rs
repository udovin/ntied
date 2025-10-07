use anyhow::{Result, anyhow};

#[derive(Clone, Debug)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub bitrate: i32,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            sample_rate: 48000, // 48kHz
            channels: 1,        // Mono for voice calls
            bitrate: 32000,     // Not used for PCM but kept for compatibility
        }
    }
}

pub struct AudioCodec {
    format: AudioFormat,
}

impl AudioCodec {
    pub fn new(format: AudioFormat) -> Self {
        Self { format }
    }

    pub fn encode(&self, samples: &[f32]) -> Result<Vec<u8>> {
        // Convert f32 samples to i16 PCM
        let samples_i16: Vec<i16> = samples
            .iter()
            .map(|&s| {
                let clamped = s.max(-1.0).min(1.0);
                (clamped * 32767.0) as i16
            })
            .collect();

        // Convert i16 to bytes (little-endian)
        let mut bytes = Vec::with_capacity(samples_i16.len() * 2);
        for sample in samples_i16 {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }

        Ok(bytes)
    }

    pub fn decode(&self, encoded: &[u8]) -> Result<Vec<f32>> {
        if encoded.len() % 2 != 0 {
            return Err(anyhow!("Invalid PCM data: odd number of bytes"));
        }

        // Convert bytes to i16 samples
        let mut samples_i16 = Vec::with_capacity(encoded.len() / 2);
        for chunk in encoded.chunks_exact(2) {
            let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
            samples_i16.push(sample);
        }

        // Convert i16 to f32
        let output_f32: Vec<f32> = samples_i16.iter().map(|&s| s as f32 / 32767.0).collect();

        Ok(output_f32)
    }

    pub fn get_format(&self) -> &AudioFormat {
        &self.format
    }

    pub fn get_frame_size(&self) -> usize {
        // 20ms frame at current sample rate
        (self.format.sample_rate as usize * 20) / 1000
    }
}
