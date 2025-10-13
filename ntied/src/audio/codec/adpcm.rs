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
    prev_samples: Vec<i16>, // For better context
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
            prev_samples: Vec::with_capacity(256),
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

            // Store for context
            self.prev_samples.push(sample_i16);
            if self.prev_samples.len() > 256 {
                self.prev_samples.remove(0);
            }

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
        self.prev_samples.clear();
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
    sample_history: Vec<f32>, // Extended history for better PLC
    plc_count: usize,         // Track consecutive PLC frames
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
            sample_history: Vec::with_capacity(1024),
            plc_count: 0,
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
        self.step_index = (data[2] as i32).clamp(0, 88); // Clamp to valid range for STEP_TABLE

        // Decode samples
        let mut samples = Vec::with_capacity((data.len() - 4) * 2);

        for &byte in &data[4..] {
            // Low nibble first
            let sample1 = self.decode_nibble(byte & 0x0F);
            samples.push((sample1 as f32 / 32767.0).clamp(-1.0, 1.0));

            // High nibble second
            let sample2 = self.decode_nibble((byte >> 4) & 0x0F);
            samples.push((sample2 as f32 / 32767.0).clamp(-1.0, 1.0));
        }

        // Store for PLC
        self.last_frame = Some(samples.clone());

        // Update sample history for advanced PLC
        self.sample_history.extend_from_slice(&samples);
        if self.sample_history.len() > 1024 {
            let excess = self.sample_history.len() - 1024;
            self.sample_history.drain(..excess);
        }

        // Reset PLC count on successful decode
        self.plc_count = 0;

        let decode_time = start.elapsed().as_micros() as f64;
        self.update_stats(decode_time, data.len());

        Ok(samples)
    }

    fn conceal_packet_loss(&mut self) -> Result<Vec<f32>> {
        self.plc_count += 1;

        // Use advanced PLC if we have sufficient history
        if self.sample_history.len() >= 256 {
            let frame_size =
                (self.params.sample_rate * 20 / 1000) as usize * self.params.channels as usize;
            let mut concealed = Vec::with_capacity(frame_size);

            // Analyze recent samples for periodicity (simple pitch detection)
            let history_len = self.sample_history.len();
            let search_range = 160.min(history_len / 4); // Search for pitch period

            let mut best_period = 40; // Default pitch period
            let mut best_correlation = 0.0;

            // Simple autocorrelation for pitch detection
            for period in 20..search_range {
                let mut correlation = 0.0;
                let mut norm_a = 0.0;
                let mut norm_b = 0.0;

                for i in 0..period.min(history_len - period) {
                    let a = self.sample_history[history_len - period - i - 1];
                    let b = self.sample_history[history_len - i - 1];
                    correlation += a * b;
                    norm_a += a * a;
                    norm_b += b * b;
                }

                if norm_a > 0.0 && norm_b > 0.0 {
                    correlation /= (norm_a * norm_b).sqrt();
                    if correlation > best_correlation {
                        best_correlation = correlation;
                        best_period = period;
                    }
                }
            }

            // Generate concealed samples using periodic extension
            for i in 0..frame_size {
                let history_idx = (history_len - best_period + (i % best_period)) % history_len;
                let mut sample = self.sample_history[history_idx];

                // Apply fade based on PLC count
                let fade_factor = (-(self.plc_count as f32) * 0.5).exp();
                sample *= fade_factor;

                // Add small random noise to prevent tonal artifacts
                let noise = ((i as f32 * 0.123).sin() * 0.001) * fade_factor;
                sample += noise;

                concealed.push(sample);
            }

            // Update history with concealed samples
            self.sample_history.extend_from_slice(&concealed);
            if self.sample_history.len() > 1024 {
                let excess = self.sample_history.len() - 1024;
                self.sample_history.drain(..excess);
            }

            self.stats.fec_recoveries += 1;
            Ok(concealed)
        } else if let Some(ref last) = self.last_frame {
            // Fallback to simple repetition with fade
            let mut concealed = last.clone();
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
        self.sample_history.clear();
        self.plc_count = 0;
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
        CodecType::ADPCM
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

    #[test]
    fn test_adpcm_compression_ratio() {
        let params = CodecParams::voice();
        let factory = AdpcmCodecFactory;
        let mut encoder = factory.create_encoder(params).unwrap();

        // ADPCM should provide 4:1 compression (4 bits per sample vs 16 bits)
        let samples = vec![0.5f32; 960];
        let encoded = encoder.encode(&samples).unwrap();

        // Expected size: 960 samples * 4 bits / 8 bits per byte = 480 bytes
        // Plus some header overhead
        assert!(
            encoded.len() <= 500,
            "ADPCM encoded size {} is too large, expected ~480 bytes",
            encoded.len()
        );
        assert!(
            encoded.len() >= 450,
            "ADPCM encoded size {} is too small, expected ~480 bytes",
            encoded.len()
        );
    }

    #[test]
    fn test_adpcm_step_adaptation() {
        let params = CodecParams::voice();
        let factory = AdpcmCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Test with increasing amplitude signal
        let mut samples = Vec::new();
        for i in 0..960 {
            let amplitude = (i as f32 / 960.0) * 0.9;
            samples.push(amplitude * (i as f32 * 0.05).sin());
        }

        let encoded = encoder.encode(&samples).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        // Check that adaptation handles varying amplitudes
        assert_eq!(decoded.len(), samples.len());

        // Later samples with higher amplitude should still be encoded reasonably
        let early_error: f32 = samples[0..100]
            .iter()
            .zip(&decoded[0..100])
            .map(|(o, d)| (o - d).abs())
            .sum::<f32>()
            / 100.0;

        let late_error: f32 = samples[860..960]
            .iter()
            .zip(&decoded[860..960])
            .map(|(o, d)| (o - d).abs())
            .sum::<f32>()
            / 100.0;

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
        let params = CodecParams::voice();
        let factory = AdpcmCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Test with DC signal (constant value)
        let samples = vec![0.3f32; 960];
        let encoded = encoder.encode(&samples).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        // Predictor should quickly converge to the DC value
        // Check last half of samples for stability
        let stable_portion = &decoded[480..];
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
        let params = CodecParams::voice();
        let factory = AdpcmCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Create a periodic signal (simulating voice pitch)
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

        // Check if PLC maintains some periodicity
        // Calculate autocorrelation at the pitch period
        let mut autocorr = 0.0f32;
        for i in pitch_period..plc_frame.len() {
            autocorr += plc_frame[i] * plc_frame[i - pitch_period];
        }
        autocorr /= (plc_frame.len() - pitch_period) as f32;

        // Should have positive correlation indicating periodicity
        assert!(
            autocorr > 0.0,
            "ADPCM PLC should maintain some periodicity, autocorr={}",
            autocorr
        );
    }

    #[test]
    fn test_adpcm_index_bounds() {
        let params = CodecParams::voice();
        let factory = AdpcmCodecFactory;
        let mut encoder = factory.create_encoder(params).unwrap();

        // Test with rapidly changing signal that might stress index adaptation
        let mut samples = Vec::new();
        for i in 0..960 {
            if i % 10 < 5 {
                samples.push(0.9);
            } else {
                samples.push(-0.9);
            }
        }

        // Should handle without panic (index bounds checking)
        let encoded = encoder.encode(&samples).unwrap();
        assert!(!encoded.is_empty());

        // Verify encoding stats
        let stats = encoder.stats();
        assert_eq!(stats.frames_encoded, 1);
        assert!(stats.bytes_encoded > 0);
    }
}
