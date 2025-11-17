use anyhow::Result;

use super::traits::{AudioDecoder, AudioEncoder, CodecFactory, CodecType};
use crate::audio::AudioConfig;

/// IMA ADPCM encoder/decoder for simple audio compression
/// Provides 4:1 compression ratio (4 bits per sample vs 16 bits)
/// Configuration: 48kHz, 1-2 channels (configurable), 20ms frames (960 samples/channel)
pub struct AdpcmEncoder {
    channels: u16,
    predictor_l: i32,
    step_index_l: i32,
    predictor_r: i32,
    step_index_r: i32,
}

/// IMA ADPCM step table
const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// Index adjustment table
const INDEX_TABLE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

impl AdpcmEncoder {
    pub fn new(channels: u16) -> Result<Self> {
        if channels == 0 || channels > 2 {
            return Err(anyhow::anyhow!(
                "ADPCM only supports 1-2 channels, got {}",
                channels
            ));
        }
        Ok(Self {
            channels,
            predictor_l: 0,
            step_index_l: 0,
            predictor_r: 0,
            step_index_r: 0,
        })
    }

    fn encode_sample(&mut self, sample: i16, channel: u16) -> u8 {
        let (predictor, step_index) = if channel == 0 {
            (&mut self.predictor_l, &mut self.step_index_l)
        } else {
            (&mut self.predictor_r, &mut self.step_index_r)
        };

        let mut step = STEP_TABLE[*step_index as usize];
        let diff = sample as i32 - *predictor;
        let mut nibble = 0u8;

        if diff < 0 {
            nibble = 8;
        }

        let mut abs_diff = diff.abs();
        if abs_diff >= step {
            nibble |= 4;
            abs_diff -= step;
        }

        step >>= 1;
        if abs_diff >= step {
            nibble |= 2;
            abs_diff -= step;
        }

        step >>= 1;
        if abs_diff >= step {
            nibble |= 1;
        }

        // Update predictor
        let step = STEP_TABLE[*step_index as usize];
        let mut step_diff = step >> 3;
        if nibble & 4 != 0 {
            step_diff += step;
        }
        if nibble & 2 != 0 {
            step_diff += step >> 1;
        }
        if nibble & 1 != 0 {
            step_diff += step >> 2;
        }

        if nibble & 8 != 0 {
            *predictor -= step_diff;
        } else {
            *predictor += step_diff;
        }

        // Clamp predictor
        *predictor = (*predictor).clamp(-32768, 32767);

        // Update step index
        *step_index += INDEX_TABLE[nibble as usize];
        *step_index = (*step_index).clamp(0, 88);

        nibble
    }
}

impl AudioEncoder for AdpcmEncoder {
    fn encode(&mut self, samples: &[f32]) -> Result<Vec<u8>> {
        // Expected: 960 samples/channel (20ms at 48kHz)
        // For stereo: 1920 samples total (interleaved L, R, L, R, ...)
        // Output size: 4 bits per sample = 0.5 bytes per sample
        // Plus header: 8 bytes (2 predictors + 2 step_indexes for stereo) or 4 bytes (mono)
        let header_size = if self.channels == 2 { 8 } else { 4 };
        let mut output = Vec::with_capacity(header_size + samples.len() / 2);

        tracing::trace!(
            "ADPCM encode: {} samples, {} channels, expected output ~{} bytes",
            samples.len(),
            self.channels,
            header_size + samples.len() / 2
        );

        // Write header(s)
        output.extend_from_slice(&self.predictor_l.to_le_bytes()[0..2]);
        output.push(self.step_index_l as u8);
        output.push(0); // Reserved

        if self.channels == 2 {
            output.extend_from_slice(&self.predictor_r.to_le_bytes()[0..2]);
            output.push(self.step_index_r as u8);
            output.push(0); // Reserved
        }

        // Encode samples (pack two 4-bit samples into each byte)
        let mut encoded_byte = 0u8;
        let mut nibble_count = 0;

        for (i, &sample) in samples.iter().enumerate() {
            // Convert f32 to i16 with proper clamping to avoid overflow
            let clamped = sample.clamp(-0.999, 0.999);
            let sample_i16 = (clamped * 32767.0) as i16;

            // Determine channel (for stereo: even indices = left, odd = right)
            let channel = if self.channels == 2 {
                (i % 2) as u16
            } else {
                0
            };
            let nibble = self.encode_sample(sample_i16, channel);

            if nibble_count % 2 == 0 {
                encoded_byte = nibble;
            } else {
                encoded_byte |= nibble << 4;
                output.push(encoded_byte);
                encoded_byte = 0;
            }
            nibble_count += 1;
        }

        // Handle odd number of samples
        if nibble_count % 2 != 0 {
            output.push(encoded_byte);
        }

        tracing::trace!(
            "ADPCM encode complete: {} samples -> {} bytes (header: {}, data: {})",
            samples.len(),
            output.len(),
            header_size,
            output.len() - header_size
        );

        Ok(output)
    }

    fn reset(&mut self) -> Result<()> {
        self.predictor_l = 0;
        self.step_index_l = 0;
        self.predictor_r = 0;
        self.step_index_r = 0;
        Ok(())
    }

    fn codec_type(&self) -> CodecType {
        CodecType::ADPCM
    }

    fn codec_config(&self) -> AudioConfig {
        AudioConfig::new(48000, self.channels) // 48kHz, configurable channels
    }
}

/// IMA ADPCM decoder
pub struct AdpcmDecoder {
    channels: u16,
    predictor_l: i32,
    step_index_l: i32,
    predictor_r: i32,
    step_index_r: i32,
    last_frame: Vec<f32>,
    plc_count: usize,
}

impl AdpcmDecoder {
    pub fn new(channels: u16) -> Result<Self> {
        if channels == 0 || channels > 2 {
            return Err(anyhow::anyhow!(
                "ADPCM only supports 1-2 channels, got {}",
                channels
            ));
        }
        // 960 samples/channel for 20ms at 48kHz
        let frame_size = 960 * channels as usize;
        Ok(Self {
            channels,
            predictor_l: 0,
            step_index_l: 0,
            predictor_r: 0,
            step_index_r: 0,
            last_frame: vec![0.0; frame_size],
            plc_count: 0,
        })
    }

    fn decode_nibble(&mut self, nibble: u8, channel: u16) -> i16 {
        let (predictor, step_index) = if channel == 0 {
            (&mut self.predictor_l, &mut self.step_index_l)
        } else {
            (&mut self.predictor_r, &mut self.step_index_r)
        };

        let step = STEP_TABLE[*step_index as usize];
        let mut step_diff = step >> 3;

        if nibble & 4 != 0 {
            step_diff += step;
        }
        if nibble & 2 != 0 {
            step_diff += step >> 1;
        }
        if nibble & 1 != 0 {
            step_diff += step >> 2;
        }

        if nibble & 8 != 0 {
            *predictor -= step_diff;
        } else {
            *predictor += step_diff;
        }

        // Clamp predictor
        *predictor = (*predictor).clamp(-32768, 32767);

        // Update step index
        *step_index += INDEX_TABLE[nibble as usize];
        *step_index = (*step_index).clamp(0, 88);

        *predictor as i16
    }
}

impl AudioDecoder for AdpcmDecoder {
    fn decode(&mut self, data: &[u8]) -> Result<Vec<f32>> {
        let header_size = if self.channels == 2 { 8 } else { 4 };
        if data.len() < header_size {
            return Err(anyhow::anyhow!("ADPCM data too short"));
        }

        // Read header(s) with validation
        self.predictor_l = i16::from_le_bytes([data[0], data[1]]) as i32;
        self.step_index_l = (data[2] as i32).clamp(0, 88);

        let mut data_offset = 4;
        if self.channels == 2 {
            self.predictor_r = i16::from_le_bytes([data[4], data[5]]) as i32;
            self.step_index_r = (data[6] as i32).clamp(0, 88);
            data_offset = 8;
        }

        tracing::trace!(
            "ADPCM decode: {} bytes, {} channels, offset: {}",
            data.len(),
            self.channels,
            data_offset
        );

        // Decode samples (expected: 960 samples/channel for 20ms at 48kHz)
        let expected_samples = 960 * self.channels as usize;
        let mut samples = Vec::with_capacity(expected_samples);
        let mut sample_count = 0;

        for byte in &data[data_offset..] {
            // For stereo, alternate between channels
            // For mono, always use channel 0
            let channel1 = if self.channels == 2 {
                sample_count % 2
            } else {
                0
            };
            let channel2 = if self.channels == 2 {
                (sample_count + 1) % 2
            } else {
                0
            };

            // Low nibble first
            let sample1 = self.decode_nibble(byte & 0x0F, channel1 as u16);
            samples.push((sample1 as f32 / 32767.0).clamp(-1.0, 1.0));
            sample_count += 1;

            // High nibble second
            let sample2 = self.decode_nibble((byte >> 4) & 0x0F, channel2 as u16);
            samples.push((sample2 as f32 / 32767.0).clamp(-1.0, 1.0));
            sample_count += 1;
        }

        // Store for PLC and reset counter
        self.last_frame = samples.clone();
        self.plc_count = 0;

        tracing::trace!(
            "ADPCM decode complete: {} bytes -> {} samples",
            data.len(),
            samples.len()
        );

        Ok(samples)
    }

    fn conceal_packet_loss(&mut self) -> Result<Vec<f32>> {
        self.plc_count += 1;

        // Simple fade-to-silence PLC (320 samples for 20ms at 16kHz mono)
        let mut concealed = self.last_frame.clone();
        let fade_factor = (-(self.plc_count as f32) * 0.3).exp();

        for sample in concealed.iter_mut() {
            *sample *= fade_factor;
        }

        Ok(concealed)
    }

    fn reset(&mut self) -> Result<()> {
        self.predictor_l = 0;
        self.step_index_l = 0;
        self.predictor_r = 0;
        self.step_index_r = 0;
        let frame_size = 960 * self.channels as usize;
        self.last_frame = vec![0.0; frame_size];
        self.plc_count = 0;
        Ok(())
    }

    fn codec_type(&self) -> CodecType {
        CodecType::ADPCM
    }

    fn codec_config(&self) -> AudioConfig {
        AudioConfig::new(48000, self.channels) // 48kHz, configurable channels
    }
}

/// Factory for creating ADPCM codec instances
pub struct AdpcmCodecFactory {
    channels: u16,
}

impl AdpcmCodecFactory {
    pub fn new(channels: u16) -> Self {
        Self { channels }
    }
}

impl CodecFactory for AdpcmCodecFactory {
    fn codec_type(&self) -> CodecType {
        CodecType::ADPCM
    }

    fn is_available(&self) -> bool {
        true
    }

    fn create_encoder(&self, _params: super::traits::CodecParams) -> Result<Box<dyn AudioEncoder>> {
        Ok(Box::new(AdpcmEncoder::new(self.channels)?))
    }

    fn create_decoder(&self, _params: super::traits::CodecParams) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(AdpcmDecoder::new(self.channels)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adpcm_encode_decode() {
        let mut encoder = AdpcmEncoder::new(1).unwrap();
        let mut decoder = AdpcmDecoder::new(1).unwrap();

        // Create test samples (960 samples for 20ms at 48kHz)
        let mut samples = Vec::new();
        for i in 0..960 {
            // Generate a sine wave
            let t = i as f32 / 48000.0;
            samples.push((2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5);
        }

        // Encode
        let encoded = encoder.encode(&samples).unwrap();

        // Should achieve ~4:1 compression (4 bytes header + samples/2)
        assert!(encoded.len() < samples.len() * 4 / 2);

        // Decode
        let decoded = decoder.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), samples.len());

        // Check that signal is reasonably preserved (ADPCM is lossy)
        let mut error = 0.0f32;
        for i in 0..samples.len() {
            error += (samples[i] - decoded[i]).abs();
        }
        let avg_error = error / samples.len() as f32;
        assert!(avg_error < 0.1);
    }

    #[test]
    fn test_adpcm_plc() {
        let mut decoder = AdpcmDecoder::new(1).unwrap();

        // First decode a frame (960 samples for 20ms at 48kHz)
        let samples = vec![0.5; 960];
        let mut encoder = AdpcmEncoder::new(1).unwrap();
        let encoded = encoder.encode(&samples).unwrap();
        decoder.decode(&encoded).unwrap();

        // Now test PLC
        let plc_frame = decoder.conceal_packet_loss().unwrap();
        assert_eq!(plc_frame.len(), 960);

        // Check that all samples have reasonable values
        for &sample in &plc_frame {
            assert!(sample.abs() <= 1.0);
        }
    }

    #[test]
    fn test_adpcm_compression_ratio() {
        let mut encoder = AdpcmEncoder::new(1).unwrap();

        // ADPCM should provide 4:1 compression (4 bits per sample vs 16 bits)
        let samples = vec![0.5f32; 960];
        let encoded = encoder.encode(&samples).unwrap();

        // Expected size: 960 samples * 4 bits / 8 bits per byte = 480 bytes
        // Plus 4 bytes header overhead = 484 bytes
        assert!(
            encoded.len() <= 500,
            "ADPCM encoded size {} is too large, expected ~484 bytes",
            encoded.len()
        );
        assert!(
            encoded.len() >= 460,
            "ADPCM encoded size {} is too small, expected ~484 bytes",
            encoded.len()
        );
    }

    #[test]
    fn test_adpcm_step_adaptation() {
        let mut encoder = AdpcmEncoder::new(1).unwrap();
        let mut decoder = AdpcmDecoder::new(1).unwrap();

        // Test with increasing amplitude signal (960 samples for 20ms at 48kHz)
        let mut samples = Vec::new();
        for i in 0..960 {
            let amplitude = (i as f32 / 960.0) * 0.8;
            samples.push(amplitude * (i as f32 * 0.05).sin());
        }

        let encoded = encoder.encode(&samples).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        // Check that adaptation handles varying amplitudes
        assert_eq!(decoded.len(), samples.len());

        // Later samples with higher amplitude should still be encoded reasonably
        let early_error: f32 = samples[0..50]
            .iter()
            .zip(&decoded[0..50])
            .map(|(o, d)| (o - d).abs())
            .sum::<f32>()
            / 50.0;

        let late_error: f32 = samples[270..320]
            .iter()
            .zip(&decoded[270..320])
            .map(|(o, d)| (o - d).abs())
            .sum::<f32>()
            / 50.0;

        // Both should have reasonable error levels
        assert!(
            early_error < 0.1,
            "Early samples error too high: {}",
            early_error
        );
        assert!(
            late_error < 0.2,
            "Late samples error too high: {}",
            late_error
        );
    }

    #[test]
    fn test_adpcm_predictor_stability() {
        let mut encoder = AdpcmEncoder::new(1).unwrap();
        let mut decoder = AdpcmDecoder::new(1).unwrap();

        // Test with DC signal (constant value) (960 samples for 20ms at 48kHz)
        let samples = vec![0.3f32; 960];
        let encoded = encoder.encode(&samples).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        // Predictor should quickly converge to the DC value
        // Check last half of samples for stability
        let stable_portion = &decoded[160..];
        let mean: f32 = stable_portion.iter().sum::<f32>() / stable_portion.len() as f32;
        let variance: f32 = stable_portion
            .iter()
            .map(|s| (s - mean).powi(2))
            .sum::<f32>()
            / stable_portion.len() as f32;

        assert!(
            (mean - 0.3).abs() < 0.05,
            "ADPCM predictor mean {} doesn't match input 0.3",
            mean
        );
        assert!(
            variance < 0.01,
            "ADPCM predictor variance {} too high for DC signal",
            variance
        );
    }

    #[test]
    fn test_adpcm_pitch_detection_plc() {
        let mut encoder = AdpcmEncoder::new(1).unwrap();
        let mut decoder = AdpcmDecoder::new(1).unwrap();

        // Create a periodic signal (simulating voice pitch) (960 samples for 20ms at 48kHz)
        let pitch_period = 48usize; // ~1kHz at 48kHz sample rate
        let mut samples = Vec::new();
        for i in 0..960 {
            let phase = (i % pitch_period) as f32 / pitch_period as f32;
            samples.push((phase * 2.0 * std::f32::consts::PI).sin() * 0.5);
        }

        // Encode and decode first frame to establish history
        let encoded = encoder.encode(&samples).unwrap();
        let _ = decoder.decode(&encoded).unwrap();

        // Now generate PLC frame
        let plc_frame = decoder.conceal_packet_loss().unwrap();

        // Check that PLC frame has correct length
        assert_eq!(plc_frame.len(), 960);

        // Check that all samples are valid
        for &sample in &plc_frame {
            assert!(sample.abs() <= 1.0);
        }
    }

    #[test]
    fn test_adpcm_index_bounds() {
        let mut encoder = AdpcmEncoder::new(1).unwrap();

        // Test with rapidly changing signal that might stress index adaptation (320 samples for 20ms at 16kHz)
        let mut samples = Vec::new();
        for i in 0..320 {
            if i % 10 < 5 {
                samples.push(0.9);
            } else {
                samples.push(-0.9);
            }
        }

        // Should not panic with index out of bounds
        let result = encoder.encode(&samples);
        assert!(result.is_ok());
    }
}
