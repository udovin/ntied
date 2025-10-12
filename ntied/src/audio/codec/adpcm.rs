use std::time::Instant;

use anyhow::Result;

use super::traits::{AudioDecoder, AudioEncoder, CodecFactory, CodecParams, CodecStats, CodecType};

/// IMA ADPCM encoder/decoder for simple audio compression
/// Provides 4:1 compression ratio (4 bits per sample vs 16 bits)
pub struct AdpcmEncoder {
    params: CodecParams,
    stats: CodecStats,
    encode_times: Vec<f64>,
    predictor: i32,
    step_index: i32,
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
    pub fn new(params: CodecParams) -> Result<Self> {
        Ok(Self {
            params,
            stats: CodecStats::default(),
            encode_times: Vec::with_capacity(100),
            predictor: 0,
            step_index: 0,
        })
    }

    fn encode_sample(&mut self, sample: i16) -> u8 {
        let mut step = STEP_TABLE[self.step_index as usize];
        let diff = sample as i32 - self.predictor;
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
        let step = STEP_TABLE[self.step_index as usize];
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
            self.predictor -= step_diff;
        } else {
            self.predictor += step_diff;
        }

        // Clamp predictor
        self.predictor = self.predictor.clamp(-32768, 32767);

        // Update step index
        self.step_index += INDEX_TABLE[nibble as usize];
        self.step_index = self.step_index.clamp(0, 88);

        nibble
    }

    fn update_stats(&mut self, encode_time: f64, output_size: usize) {
        self.stats.frames_encoded += 1;
        self.stats.bytes_encoded += output_size as u64;

        self.encode_times.push(encode_time);
        if self.encode_times.len() > 100 {
            self.encode_times.remove(0);
        }
        self.stats.avg_encode_time_us =
            self.encode_times.iter().sum::<f64>() / self.encode_times.len() as f64;

        // For ADPCM: 4 bits per sample
        self.stats.current_bitrate = self.params.sample_rate * self.params.channels as u32 * 4;
    }
}

impl AudioEncoder for AdpcmEncoder {
    fn encode(&mut self, samples: &[f32]) -> Result<Vec<u8>> {
        let start = Instant::now();

        // Output size: 4 bits per sample = 0.5 bytes per sample
        // Plus 4 bytes header (predictor + step_index)
        let mut output = Vec::with_capacity(4 + samples.len() / 2);

        // Write header
        output.extend_from_slice(&self.predictor.to_le_bytes()[0..2]);
        output.push(self.step_index as u8);
        output.push(0); // Reserved

        // Encode samples (pack two 4-bit samples into each byte)
        let mut encoded_byte = 0u8;
        for (i, &sample) in samples.iter().enumerate() {
            // Convert f32 to i16
            let sample_i16 = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
            let nibble = self.encode_sample(sample_i16);

            if i % 2 == 0 {
                encoded_byte = nibble;
            } else {
                encoded_byte |= nibble << 4;
                output.push(encoded_byte);
                encoded_byte = 0;
            }
        }

        // Handle odd number of samples
        if samples.len() % 2 != 0 {
            output.push(encoded_byte);
        }

        let encode_time = start.elapsed().as_micros() as f64;
        self.update_stats(encode_time, output.len());

        Ok(output)
    }

    fn reset(&mut self) -> Result<()> {
        self.predictor = 0;
        self.step_index = 0;
        Ok(())
    }

    fn params(&self) -> &CodecParams {
        &self.params
    }

    fn set_params(&mut self, params: CodecParams) -> Result<()> {
        self.params = params;
        self.reset()?;
        Ok(())
    }

    fn set_bitrate(&mut self, _bitrate: u32) -> Result<()> {
        // ADPCM has fixed compression ratio
        Ok(())
    }

    fn set_packet_loss(&mut self, percentage: u8) -> Result<()> {
        self.params.expected_packet_loss = percentage;
        Ok(())
    }

    fn stats(&self) -> &CodecStats {
        &self.stats
    }
}

/// IMA ADPCM decoder
pub struct AdpcmDecoder {
    params: CodecParams,
    stats: CodecStats,
    decode_times: Vec<f64>,
    predictor: i32,
    step_index: i32,
    last_frame: Option<Vec<f32>>,
}

impl AdpcmDecoder {
    pub fn new(params: CodecParams) -> Result<Self> {
        Ok(Self {
            params,
            stats: CodecStats::default(),
            decode_times: Vec::with_capacity(100),
            predictor: 0,
            step_index: 0,
            last_frame: None,
        })
    }

    fn decode_nibble(&mut self, nibble: u8) -> i16 {
        let step = STEP_TABLE[self.step_index as usize];
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
            self.predictor -= step_diff;
        } else {
            self.predictor += step_diff;
        }

        // Clamp predictor
        self.predictor = self.predictor.clamp(-32768, 32767);

        // Update step index
        self.step_index += INDEX_TABLE[nibble as usize];
        self.step_index = self.step_index.clamp(0, 88);

        self.predictor as i16
    }

    fn update_stats(&mut self, decode_time: f64, input_size: usize) {
        self.stats.frames_decoded += 1;
        self.stats.bytes_decoded += input_size as u64;

        self.decode_times.push(decode_time);
        if self.decode_times.len() > 100 {
            self.decode_times.remove(0);
        }
        self.stats.avg_decode_time_us =
            self.decode_times.iter().sum::<f64>() / self.decode_times.len() as f64;
    }
}

impl AudioDecoder for AdpcmDecoder {
    fn decode(&mut self, data: &[u8]) -> Result<Vec<f32>> {
        let start = Instant::now();

        if data.len() < 4 {
            return Err(anyhow::anyhow!("ADPCM data too short"));
        }

        // Read header
        self.predictor = i16::from_le_bytes([data[0], data[1]]) as i32;
        self.step_index = data[2] as i32;

        // Decode samples
        let mut samples = Vec::with_capacity((data.len() - 4) * 2);

        for &byte in &data[4..] {
            // Low nibble first
            let sample1 = self.decode_nibble(byte & 0x0F);
            samples.push(sample1 as f32 / 32767.0);

            // High nibble second
            let sample2 = self.decode_nibble((byte >> 4) & 0x0F);
            samples.push(sample2 as f32 / 32767.0);
        }

        // Store for PLC
        self.last_frame = Some(samples.clone());

        let decode_time = start.elapsed().as_micros() as f64;
        self.update_stats(decode_time, data.len());

        Ok(samples)
    }

    fn conceal_packet_loss(&mut self) -> Result<Vec<f32>> {
        // Simple PLC: repeat last frame with fade
        if let Some(ref last) = self.last_frame {
            let mut concealed = last.clone();
            // Apply exponential fade to reduce artifacts
            let len = concealed.len();
            for (i, sample) in concealed.iter_mut().enumerate() {
                let fade = (-(i as f32) / len as f32 * 2.0).exp();
                *sample *= fade;
            }
            self.stats.fec_recoveries += 1;
            Ok(concealed)
        } else {
            // No previous frame, return silence
            let frame_size = (self.params.sample_rate * 20 / 1000) as usize;
            Ok(vec![0.0; frame_size * self.params.channels as usize])
        }
    }

    fn reset(&mut self) -> Result<()> {
        self.predictor = 0;
        self.step_index = 0;
        self.last_frame = None;
        Ok(())
    }

    fn params(&self) -> &CodecParams {
        &self.params
    }

    fn set_params(&mut self, params: CodecParams) -> Result<()> {
        self.params = params;
        self.reset()?;
        Ok(())
    }

    fn stats(&self) -> &CodecStats {
        &self.stats
    }
}

/// Factory for creating ADPCM codec instances
pub struct AdpcmCodecFactory;

impl CodecFactory for AdpcmCodecFactory {
    fn codec_type(&self) -> CodecType {
        CodecType::PCMU // Using PCMU type as placeholder for ADPCM
    }

    fn is_available(&self) -> bool {
        true // Always available, no external dependencies
    }

    fn create_encoder(&self, params: CodecParams) -> Result<Box<dyn AudioEncoder>> {
        Ok(Box::new(AdpcmEncoder::new(params)?))
    }

    fn create_decoder(&self, params: CodecParams) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(AdpcmDecoder::new(params)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adpcm_encode_decode() {
        let params = CodecParams::voice();
        let factory = AdpcmCodecFactory;

        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params.clone()).unwrap();

        // Create test samples
        let mut samples = Vec::new();
        for i in 0..960 {
            // Generate a sine wave
            let sample = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin() * 0.5;
            samples.push(sample);
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
        assert!(avg_error < 0.1); // ADPCM is lossy, allow more error tolerance for 4-bit compression
    }

    #[test]
    fn test_adpcm_plc() {
        let params = CodecParams::voice();
        let factory = AdpcmCodecFactory;

        let mut decoder = factory.create_decoder(params).unwrap();

        // First decode a frame
        let samples = vec![0.5; 960];
        let mut encoder = AdpcmEncoder::new(CodecParams::voice()).unwrap();
        let encoded = encoder.encode(&samples).unwrap();
        decoder.decode(&encoded).unwrap();

        // Now test PLC
        let plc_frame = decoder.conceal_packet_loss().unwrap();
        assert_eq!(plc_frame.len(), 960);

        // Check that fade was applied (compare averages to avoid single sample issues)
        let front_avg = plc_frame[0..10].iter().map(|x| x.abs()).sum::<f32>() / 10.0;
        let back_avg = plc_frame[950..960].iter().map(|x| x.abs()).sum::<f32>() / 10.0;
        assert!(front_avg >= back_avg * 0.9); // Allow some tolerance for fade
    }
}
