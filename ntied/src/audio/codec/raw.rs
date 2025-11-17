use anyhow::Result;

use super::traits::{AudioDecoder, AudioEncoder, CodecFactory, CodecType};
use crate::audio::AudioConfig;

/// Raw PCM encoder (no compression)
/// Fixed configuration: 48kHz, 1-2 channels (configurable), 20ms frames
pub struct RawEncoder {
    channels: u16,
}

impl RawEncoder {
    pub fn new(channels: u16) -> Result<Self> {
        if channels == 0 || channels > 2 {
            return Err(anyhow::anyhow!(
                "Raw codec only supports 1-2 channels, got {}",
                channels
            ));
        }
        Ok(Self { channels })
    }
}

impl AudioEncoder for RawEncoder {
    fn encode(&mut self, samples: &[f32]) -> Result<Vec<u8>> {
        // Expected: 960 samples/channel for 20ms at 48kHz
        // Convert f32 samples to bytes (little-endian)
        let mut output = Vec::with_capacity(samples.len() * 4);
        for sample in samples {
            output.extend_from_slice(&sample.to_le_bytes());
        }
        Ok(output)
    }

    fn reset(&mut self) -> Result<()> {
        // No state to reset
        Ok(())
    }

    fn codec_type(&self) -> CodecType {
        CodecType::Raw
    }

    fn codec_config(&self) -> AudioConfig {
        AudioConfig::new(48000, self.channels)
    }
}

/// Raw PCM decoder (no compression)
pub struct RawDecoder {
    channels: u16,
    last_frame: Vec<f32>,
}

impl RawDecoder {
    pub fn new(channels: u16) -> Result<Self> {
        if channels == 0 || channels > 2 {
            return Err(anyhow::anyhow!(
                "Raw codec only supports 1-2 channels, got {}",
                channels
            ));
        }
        // 960 samples/channel for 20ms at 48kHz
        let frame_size = 960 * channels as usize;
        Ok(Self {
            channels,
            last_frame: vec![0.0; frame_size],
        })
    }
}

impl AudioDecoder for RawDecoder {
    fn decode(&mut self, data: &[u8]) -> Result<Vec<f32>> {
        // Convert bytes back to f32 samples
        let mut samples = Vec::with_capacity(data.len() / 4);
        for chunk in data.chunks_exact(4) {
            let bytes: [u8; 4] = chunk.try_into().unwrap();
            samples.push(f32::from_le_bytes(bytes));
        }

        // Store for potential PLC
        self.last_frame = samples.clone();

        Ok(samples)
    }

    fn conceal_packet_loss(&mut self) -> Result<Vec<f32>> {
        // Simple PLC: repeat last frame with fade
        let mut concealed = self.last_frame.clone();
        let len = concealed.len();
        for (i, sample) in concealed.iter_mut().enumerate() {
            let fade = 1.0 - (i as f32 / len as f32) * 0.3;
            *sample *= fade;
        }
        Ok(concealed)
    }
    

    fn reset(&mut self) -> Result<()> {
        let frame_size = 960 * self.channels as usize;
        self.last_frame = vec![0.0; frame_size];
        Ok(())
    }

    fn codec_type(&self) -> CodecType {
        CodecType::Raw
    }

    fn codec_config(&self) -> AudioConfig {
        AudioConfig::new(48000, self.channels)
    }
}

/// Factory for creating Raw codec instances
pub struct RawCodecFactory {
    channels: u16,
}

impl RawCodecFactory {
    pub fn new(channels: u16) -> Self {
        Self { channels }
    }
}

impl CodecFactory for RawCodecFactory {
    fn codec_type(&self) -> CodecType {
        CodecType::Raw
    }

    fn is_available(&self) -> bool {
        true
    }

    fn create_encoder(&self, _params: super::traits::CodecParams) -> Result<Box<dyn AudioEncoder>> {
        Ok(Box::new(RawEncoder::new(self.channels)?))
    }

    fn create_decoder(&self, _params: super::traits::CodecParams) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(RawDecoder::new(self.channels)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raw_encode_decode_mono() {
        let mut encoder = RawEncoder::new(1).unwrap();
        let mut decoder = RawDecoder::new(1).unwrap();

        // Create test samples (960 samples for 20ms at 48kHz mono)
        let samples = vec![0.1, -0.2, 0.3, -0.4, 0.5];

        // Encode
        let encoded = encoder.encode(&samples).unwrap();
        assert_eq!(encoded.len(), samples.len() * 4); // 4 bytes per f32

        // Decode
        let decoded = decoder.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), samples.len());

        // Check exact match (lossless)
        for i in 0..samples.len() {
            assert!((samples[i] - decoded[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn test_raw_encode_decode_stereo() {
        let mut encoder = RawEncoder::new(2).unwrap();
        let mut decoder = RawDecoder::new(2).unwrap();

        // Create test samples (interleaved stereo: L, R, L, R, ...)
        let samples = vec![0.1, -0.1, 0.2, -0.2, 0.3, -0.3];

        // Encode
        let encoded = encoder.encode(&samples).unwrap();
        assert_eq!(encoded.len(), samples.len() * 4);

        // Decode
        let decoded = decoder.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), samples.len());

        // Check exact match (lossless)
        for i in 0..samples.len() {
            assert!((samples[i] - decoded[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn test_raw_plc() {
        let mut decoder = RawDecoder::new(1).unwrap();

        // First decode a frame
        let samples = vec![0.5f32; 960];
        let encoded: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        decoder.decode(&encoded).unwrap();

        // Now test PLC
        let plc_frame = decoder.conceal_packet_loss().unwrap();
        assert_eq!(plc_frame.len(), 960);

        // Check that it applied fade
        assert!(plc_frame[0] > plc_frame[plc_frame.len() - 1]);
    }
}
