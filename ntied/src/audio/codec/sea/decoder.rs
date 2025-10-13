//! SEA codec decoder implementation

use std::time::Instant;

use anyhow::{Result, anyhow};

use super::{SeaConfig, lms::LmsFilterBank, quantization};
use crate::audio::codec::traits::{AudioDecoder, CodecParams, CodecStats};

/// Simple bit unpacker for decoding
struct BitUnpacker<'a> {
    data: &'a [u8],
    byte_offset: usize,
    bit_offset: usize,
}

impl<'a> BitUnpacker<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_offset: 0,
            bit_offset: 0,
        }
    }

    fn read(&mut self, bits: usize) -> Result<u32> {
        if bits == 0 || bits > 32 {
            return Err(anyhow!("Invalid bit count: {}", bits));
        }

        let mut result = 0u32;
        let mut bits_read = 0;

        while bits_read < bits {
            if self.byte_offset >= self.data.len() {
                return Err(anyhow!("Insufficient data"));
            }

            let bits_available = 8 - self.bit_offset;
            let bits_to_read = (bits - bits_read).min(bits_available);

            let mask = ((1u32 << bits_to_read) - 1) as u8;
            let shift = bits_available - bits_to_read;
            let byte_val = (self.data[self.byte_offset] >> shift) & mask;

            result = (result << bits_to_read) | (byte_val as u32);
            bits_read += bits_to_read;
            self.bit_offset += bits_to_read;

            if self.bit_offset == 8 {
                self.byte_offset += 1;
                self.bit_offset = 0;
            }
        }

        Ok(result)
    }
}

/// SEA decoder implementation
pub struct SeaDecoder {
    params: CodecParams,
    config: SeaConfig,
    lms_filters: LmsFilterBank,
    stats: CodecStats,
    decode_times: Vec<f64>,
    last_chunk: Option<Vec<f32>>,
    plc_fade_samples: usize,
}

impl SeaDecoder {
    /// Create a new SEA decoder
    pub fn new(params: CodecParams) -> Result<Self> {
        // Create config based on params
        let config = if params.bitrate <= 16000 {
            SeaConfig::low_bandwidth()
        } else if params.bitrate <= 32000 {
            SeaConfig::voice()
        } else {
            SeaConfig::music()
        };

        let lms_filters = LmsFilterBank::new(params.channels as usize);

        Ok(Self {
            params,
            config,
            lms_filters,
            stats: CodecStats::default(),
            decode_times: Vec::with_capacity(100),
            last_chunk: None,
            plc_fade_samples: 0,
        })
    }

    /// Decode a chunk of encoded data
    fn decode_chunk(&mut self, data: &[u8]) -> Result<Vec<f32>> {
        if data.is_empty() {
            // Return silence for empty data
            return Ok(vec![
                0.0;
                self.config.chunk_size * self.params.channels as usize
            ]);
        }

        if data.len() < 4 {
            // Return silence for too short data instead of error
            return Ok(vec![
                0.0;
                self.config.chunk_size * self.params.channels as usize
            ]);
        }

        let channels = self.params.channels as usize;

        // Parse header
        let chunk_type = data[0];
        let has_lms = (chunk_type & 0x80) != 0; // Check LMS flag
        let is_vbr = (chunk_type & 0x02) != 0;
        let scale_factor_bits = (data[1] >> 4) as usize;
        let residual_bits = (data[1] & 0x0F) as usize;
        let scale_factor_distance = data[2] as usize;
        let magic = data[3];

        if magic != 0x5A {
            // For corrupted magic byte, try to decode anyway or return silence
            return Ok(vec![
                0.0;
                self.config.chunk_size * self.params.channels as usize
            ]);
        }

        // Validate parameters and use defaults for corrupted values
        let scale_factor_bits = if scale_factor_bits > 0 && scale_factor_bits <= 8 {
            scale_factor_bits
        } else {
            self.config.scale_factor_bits as usize // Use default
        };

        let residual_bits = if residual_bits > 0 && residual_bits <= 8 {
            residual_bits
        } else {
            self.config.bitrate as usize // Use default
        };

        let scale_factor_distance = if scale_factor_distance > 0 {
            scale_factor_distance
        } else {
            self.config.scale_factor_distance as usize // Use default
        };

        let mut offset = 4;

        // Note: We don't restore LMS filter states from bitstream
        // Encoder and decoder maintain synchronized states through deterministic updates

        // Calculate number of scale factors
        let num_scale_factors = ((self.config.chunk_size + scale_factor_distance - 1)
            / scale_factor_distance)
            * channels;
        let scale_factor_bytes = (num_scale_factors * scale_factor_bits + 7) / 8;

        if offset + scale_factor_bytes > data.len() {
            // Return silence for truncated data
            return Ok(vec![
                0.0;
                self.config.chunk_size * self.params.channels as usize
            ]);
        }

        // Unpack scale factors
        let scale_factors = match self.unpack_scale_factors(
            &data[offset..],
            num_scale_factors,
            scale_factor_bits,
        ) {
            Ok(sf) => sf,
            Err(_) => {
                // Return silence if unpacking fails
                return Ok(vec![
                    0.0;
                    self.config.chunk_size * self.params.channels as usize
                ]);
            }
        };
        offset += scale_factor_bytes;

        // Calculate residual data size
        let num_residuals = self.config.chunk_size * channels;
        let residual_bytes = (num_residuals * residual_bits + 7) / 8;

        if offset + residual_bytes > data.len() {
            // Return silence for truncated data
            return Ok(vec![
                0.0;
                self.config.chunk_size * self.params.channels as usize
            ]);
        }

        // Unpack residuals
        let residuals = if is_vbr {
            // VBR mode - for now, treat as CBR
            // TODO: Implement proper VBR decoding
            self.unpack_residuals(&data[offset..], num_residuals, residual_bits)?
        } else {
            self.unpack_residuals(&data[offset..], num_residuals, residual_bits)?
        };

        // Dequantize and reconstruct samples
        let samples = self.reconstruct_samples(&residuals, &scale_factors, channels, has_lms)?;

        // Convert to f32 with clamping to ensure valid range
        let samples_f32: Vec<f32> = samples
            .iter()
            .map(|&s| (s as f32 / 32767.0).clamp(-1.0, 1.0))
            .collect();

        // Store for PLC
        self.last_chunk = Some(samples_f32.clone());
        self.plc_fade_samples = 0;

        Ok(samples_f32)
    }

    /// Unpack scale factors from bitstream
    fn unpack_scale_factors(&self, data: &[u8], count: usize, bits: usize) -> Result<Vec<u16>> {
        let required_bytes = (count * bits + 7) / 8;
        if data.len() < required_bytes {
            return Err(anyhow!(
                "Insufficient data for scale factors: {} bytes available, {} required",
                data.len(),
                required_bytes
            ));
        }

        let mut unpacker = BitUnpacker::new(data);
        let mut scale_factors = Vec::with_capacity(count);

        for i in 0..count {
            let value = unpacker
                .read(bits)
                .map_err(|e| anyhow!("Failed to read scale factor {}/{}: {}", i, count, e))?
                as u16;
            // Denormalize scale factor
            let scale = if bits < 16 {
                value << (16 - bits)
            } else {
                value
            };
            scale_factors.push(scale.max(1)); // Ensure non-zero scale
        }

        Ok(scale_factors)
    }

    /// Unpack quantized residuals from bitstream
    fn unpack_residuals(&self, data: &[u8], count: usize, bits: usize) -> Result<Vec<u8>> {
        let required_bytes = (count * bits + 7) / 8;
        if data.len() < required_bytes {
            return Err(anyhow!(
                "Insufficient data for residuals: {} bytes available, {} required",
                data.len(),
                required_bytes
            ));
        }

        let mut unpacker = BitUnpacker::new(data);
        let mut residuals = Vec::with_capacity(count);

        for i in 0..count {
            let value = unpacker
                .read(bits)
                .map_err(|e| anyhow!("Failed to read residual {}/{}: {}", i, count, e))?
                as u8;
            residuals.push(value);
        }

        Ok(residuals)
    }

    /// Reconstruct samples from residuals and scale factors
    fn reconstruct_samples(
        &mut self,
        quantized: &[u8],
        scale_factors: &[u16],
        channels: usize,
        has_lms: bool,
    ) -> Result<Vec<i32>> {
        let quant_bits = self.config.bitrate;
        let mut samples = Vec::with_capacity(quantized.len());
        let scale_distance = self.config.scale_factor_distance as usize;

        for (i, &quant_idx) in quantized.iter().enumerate() {
            let channel = i % channels;

            // Get appropriate scale factor
            let scale_idx = (i / (scale_distance * channels)) * channels + channel;
            let scale = if scale_idx < scale_factors.len() {
                scale_factors[scale_idx] as i32
            } else {
                scale_factors.last().copied().unwrap_or(32768) as i32
            };

            // Dequantize the residual (this is the reconstructed residual)
            let residual = quantization::dequantize(quant_idx, quant_bits, scale);

            // Use LMS reconstruction if enabled
            let sample = if has_lms {
                if let Some(filter) = self.lms_filters.get_filter_mut(channel) {
                    // Get prediction BEFORE updating filter (same as encoder)
                    let prediction = filter.predict();

                    // Reconstruct sample from dequantized residual and prediction
                    let reconstructed_sample = residual + prediction;

                    // Update filter with reconstructed sample (EXACTLY as encoder does)
                    filter.push_sample(reconstructed_sample);

                    // Adapt weights using dequantized residual (EXACTLY as encoder does)
                    filter.adapt_weights(residual);

                    reconstructed_sample
                } else {
                    // Fallback if filter not available
                    residual
                }
            } else {
                // Non-LMS mode: direct dequantization
                // Still update filters for PLC
                if let Some(filter) = self.lms_filters.get_filter_mut(channel) {
                    let prediction = filter.predict();
                    filter.push_sample(residual);
                    let error = residual - prediction;
                    filter.adapt_weights(error);
                }
                residual
            };

            samples.push(sample);
        }

        Ok(samples)
    }

    /// Generate samples for packet loss concealment
    fn generate_plc_samples(&mut self) -> Vec<f32> {
        let chunk_size = self.config.chunk_size * self.params.channels as usize;

        // If we have a previous chunk, use it for PLC
        if let Some(ref last_chunk) = self.last_chunk {
            let mut plc_samples = Vec::with_capacity(chunk_size);

            // Use a combination of LMS prediction and last chunk repetition with fade
            for i in 0..chunk_size {
                let channel = i % self.params.channels as usize;

                // Get prediction from LMS filter
                let prediction = self
                    .lms_filters
                    .get_filter(channel)
                    .map(|f| {
                        let pred = f.predict();
                        // Clamp prediction to reasonable range
                        pred.clamp(-32767, 32767) as f32 / 32767.0
                    })
                    .unwrap_or(0.0);

                // Mix with last chunk samples if available (for stability)
                let last_sample = if i < last_chunk.len() {
                    last_chunk[i]
                } else {
                    0.0
                };

                // Blend prediction with last sample
                let blend_factor = 0.7; // More weight on prediction
                let blended = prediction * blend_factor + last_sample * (1.0 - blend_factor);

                // Apply exponential fade to reduce artifacts
                let fade_factor = (-(self.plc_fade_samples as f32) / 2400.0).exp(); // Faster fade
                let sample = (blended * fade_factor).clamp(-1.0, 1.0);

                plc_samples.push(sample);

                // Update LMS filter with attenuated sample for next prediction
                if let Some(filter) = self.lms_filters.get_filter_mut(channel) {
                    let sample_i32 = (sample * 32767.0).clamp(-32767.0, 32767.0) as i32;
                    filter.push_sample(sample_i32);
                    // Don't adapt weights during PLC to maintain stability
                }

                self.plc_fade_samples = self.plc_fade_samples.saturating_add(1);
            }

            plc_samples
        } else {
            // No previous data, return silence
            vec![0.0; chunk_size]
        }
    }

    fn update_stats(&mut self, decode_time: f64, input_size: usize, is_plc: bool) {
        if is_plc {
            self.stats.fec_recoveries += 1;
        } else {
            self.stats.frames_decoded += 1;
            self.stats.bytes_decoded += input_size as u64;
        }

        self.decode_times.push(decode_time);
        if self.decode_times.len() > 100 {
            self.decode_times.remove(0);
        }
        self.stats.avg_decode_time_us =
            self.decode_times.iter().sum::<f64>() / self.decode_times.len() as f64;
    }
}

impl AudioDecoder for SeaDecoder {
    fn decode(&mut self, data: &[u8]) -> Result<Vec<f32>> {
        let start = Instant::now();

        let decoded = self.decode_chunk(data)?;

        let decode_time = start.elapsed().as_micros() as f64;
        self.update_stats(decode_time, data.len(), false);

        Ok(decoded)
    }

    fn conceal_packet_loss(&mut self) -> Result<Vec<f32>> {
        let start = Instant::now();

        let plc_samples = self.generate_plc_samples();

        let decode_time = start.elapsed().as_micros() as f64;
        self.update_stats(decode_time, 0, true);

        Ok(plc_samples)
    }

    fn reset(&mut self) -> Result<()> {
        self.lms_filters.reset();
        self.last_chunk = None;
        self.plc_fade_samples = 0;
        Ok(())
    }

    fn params(&self) -> &CodecParams {
        &self.params
    }

    fn set_params(&mut self, params: CodecParams) -> Result<()> {
        // Update config based on new params
        self.config = if params.bitrate <= 16000 {
            SeaConfig::low_bandwidth()
        } else if params.bitrate <= 32000 {
            SeaConfig::voice()
        } else {
            SeaConfig::music()
        };

        // Recreate LMS filters if channel count changed
        if params.channels != self.params.channels {
            self.lms_filters = LmsFilterBank::new(params.channels as usize);
        }

        self.params = params;
        Ok(())
    }

    fn stats(&self) -> &CodecStats {
        &self.stats
    }
}

// Duplicate BitUnpacker removed - using the one defined at the top of the file

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bit_unpacker() {
        let data = vec![0b10111001, 0b11000000];
        let mut unpacker = BitUnpacker::new(&data);

        assert_eq!(unpacker.read(3).unwrap(), 0b101);
        assert_eq!(unpacker.read(4).unwrap(), 0b1100);
        assert_eq!(unpacker.read(1).unwrap(), 0b1);
    }

    #[test]
    fn test_decoder_creation() {
        let params = CodecParams::voice();
        let decoder = SeaDecoder::new(params);
        assert!(decoder.is_ok());
    }

    #[test]
    fn test_plc_generation() {
        let params = CodecParams::voice();
        let mut decoder = SeaDecoder::new(params).unwrap();

        // Generate PLC without previous data
        let plc_samples = decoder.conceal_packet_loss().unwrap();
        assert_eq!(plc_samples.len(), 960); // 20ms at 48kHz mono

        // Should be silence when no previous data
        assert!(plc_samples.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn test_plc_fade() {
        let params = CodecParams::voice();
        let mut decoder = SeaDecoder::new(params).unwrap();

        // Set up some previous chunk data
        decoder.last_chunk = Some(vec![0.5; 960]);

        // Generate PLC
        let plc_samples = decoder.conceal_packet_loss().unwrap();

        // Check that fade was applied
        let first_sample = plc_samples[0].abs();
        let last_sample = plc_samples[plc_samples.len() - 1].abs();

        // Last sample should be more faded than first
        assert!(last_sample < first_sample || last_sample == 0.0);
    }
}
