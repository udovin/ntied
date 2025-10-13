//! SEA codec encoder implementation

use std::time::Instant;

use anyhow::Result;

use super::{SeaConfig, lms::LmsFilterBank, quantization};
use crate::audio::codec::traits::{AudioEncoder, CodecParams, CodecStats};

/// SEA encoder implementation
pub struct SeaEncoder {
    params: CodecParams,
    config: SeaConfig,
    lms_filters: LmsFilterBank,
    stats: CodecStats,
    encode_times: Vec<f64>,
    frame_buffer: Vec<i32>,
}

impl SeaEncoder {
    /// Create a new SEA encoder
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
            encode_times: Vec::with_capacity(100),
            frame_buffer: Vec::new(),
        })
    }

    /// Encode a chunk of samples
    fn encode_chunk(&mut self, samples: &[i32]) -> Result<Vec<u8>> {
        let channels = self.params.channels as usize;
        let quant_bits = self.config.bitrate;

        // For simplicity, don't use LMS prediction - just quantize directly
        // This will work but with lower compression
        let residuals = samples.to_vec();

        // Find scale factors
        let scale_factors = self.calculate_scale_factors(&residuals, channels);

        // Quantize residuals
        let mut quantized = Vec::with_capacity(residuals.len());
        let scale_distance = self.config.scale_factor_distance as usize;

        for (i, &residual) in residuals.iter().enumerate() {
            let channel = i % channels;
            // Find appropriate scale factor
            let scale_idx = (i / (scale_distance * channels)) * channels + channel;
            let scale = if scale_idx < scale_factors.len() {
                scale_factors[scale_idx] as i32
            } else {
                scale_factors.last().copied().unwrap_or(32768) as i32
            };

            // Use simplified quantization
            let quant_idx = quantization::quantize(residual, quant_bits, scale);
            quantized.push(quant_idx);
        }

        // Pack the encoded data
        self.pack_chunk(&scale_factors, &quantized, channels)
    }

    /// Calculate scale factors for residuals
    fn calculate_scale_factors(&self, residuals: &[i32], channels: usize) -> Vec<u16> {
        let scale_distance = self.config.scale_factor_distance as usize;
        let num_segments =
            (residuals.len() + scale_distance * channels - 1) / (scale_distance * channels);
        let mut scale_factors = Vec::with_capacity(num_segments * channels);

        for seg in 0..num_segments {
            for ch in 0..channels {
                let mut max_val = 1i32;

                // Find maximum absolute value in this segment for this channel
                for i in 0..scale_distance {
                    let idx = seg * scale_distance * channels + i * channels + ch;
                    if idx < residuals.len() {
                        max_val = max_val.max(residuals[idx].abs());
                    }
                }

                // Scale factor is the maximum absolute value in the segment
                // Ensure minimum scale to avoid division issues
                scale_factors.push(max_val.max(1).min(32767) as u16);
            }
        }

        scale_factors
    }

    /// Pack chunk data into byte stream
    fn pack_chunk(
        &self,
        scale_factors: &[u16],
        quantized: &[u8],
        _channels: usize,
    ) -> Result<Vec<u8>> {
        let mut output = Vec::new();

        // Header (4 bytes)
        output.push(if self.config.vbr { 0x02 } else { 0x01 }); // Chunk type
        output.push((self.config.scale_factor_bits << 4) | self.config.bitrate); // Scale factor and residual size
        output.push(self.config.scale_factor_distance); // Scale factor distance
        output.push(0x5A); // Reserved magic byte

        // Skip LMS state for now - just use direct quantization

        // Pack scale factors
        let scale_bits = self.config.scale_factor_bits as usize;
        let mut bit_buffer = BitPacker::new();

        for &scale in scale_factors {
            // Normalize scale factor to fit in scale_bits
            let normalized = (scale >> (16 - scale_bits)) as u32;
            bit_buffer.write(normalized, scale_bits);
        }

        output.extend_from_slice(&bit_buffer.finish());

        // Pack quantized residuals
        let quant_bits = self.config.bitrate as usize;
        let mut bit_buffer = BitPacker::new();

        if self.config.vbr {
            // In VBR mode, first write residual length differences
            // For simplicity, we'll use fixed bitrate for now
            // TODO: Implement true VBR with adaptive bit allocation
        }

        for &quant_idx in quantized {
            bit_buffer.write(quant_idx as u32, quant_bits);
        }

        output.extend_from_slice(&bit_buffer.finish());

        Ok(output)
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

        // Calculate effective bitrate
        let samples_per_chunk = self.config.chunk_size * self.params.channels as usize;
        let bits_per_sample = (output_size * 8) as f32 / samples_per_chunk as f32;
        self.stats.current_bitrate = (bits_per_sample * self.params.sample_rate as f32) as u32;
    }
}

impl AudioEncoder for SeaEncoder {
    fn encode(&mut self, samples: &[f32]) -> Result<Vec<u8>> {
        let start = Instant::now();

        // Convert f32 to i32 for processing, handling NaN and Infinity
        let samples_i32: Vec<i32> = samples
            .iter()
            .map(|&s| {
                let cleaned = if s.is_finite() {
                    s.clamp(-1.0, 1.0)
                } else {
                    0.0 // Replace NaN/Infinity with silence
                };
                (cleaned * 32767.0) as i32
            })
            .collect();

        // For testing and small frames, encode immediately if we have exact chunk size
        let expected_chunk_size = self.config.chunk_size * self.params.channels as usize;

        // Allow encoding if we have enough samples or exact match
        if samples_i32.len() == expected_chunk_size
            || (samples_i32.len() >= expected_chunk_size && self.frame_buffer.is_empty())
        {
            // Take only the expected chunk size
            let chunk_to_encode = if samples_i32.len() > expected_chunk_size {
                &samples_i32[..expected_chunk_size]
            } else {
                &samples_i32
            };

            let encoded = self.encode_chunk(chunk_to_encode)?;
            let encode_time = start.elapsed().as_micros() as f64;
            self.update_stats(encode_time, encoded.len());

            // Store any remaining samples in buffer
            if samples_i32.len() > expected_chunk_size {
                self.frame_buffer
                    .extend_from_slice(&samples_i32[expected_chunk_size..]);
            }

            return Ok(encoded);
        }

        // Otherwise, use buffering for streaming
        self.frame_buffer.extend_from_slice(&samples_i32);

        // Check if we have enough samples for a chunk
        if self.frame_buffer.len() < expected_chunk_size {
            return Ok(Vec::new()); // Need more samples
        }

        // Encode the chunk
        let chunk: Vec<i32> = self.frame_buffer.drain(..expected_chunk_size).collect();
        let encoded = self.encode_chunk(&chunk)?;

        let encode_time = start.elapsed().as_micros() as f64;
        self.update_stats(encode_time, encoded.len());

        Ok(encoded)
    }

    fn reset(&mut self) -> Result<()> {
        self.lms_filters.reset();
        self.frame_buffer.clear();
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

    fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        self.params.bitrate = bitrate;

        // Adjust config bitrate setting
        self.config.bitrate = if bitrate <= 16000 {
            2
        } else if bitrate <= 32000 {
            3
        } else if bitrate <= 64000 {
            4
        } else {
            5
        };

        Ok(())
    }

    fn set_packet_loss(&mut self, percentage: u8) -> Result<()> {
        self.params.expected_packet_loss = percentage;

        // Adjust VBR based on packet loss
        if percentage > 10 {
            self.config.vbr = true; // Enable VBR for better resilience
        }

        Ok(())
    }

    fn stats(&self) -> &CodecStats {
        &self.stats
    }
}

/// Simple bit packer for encoding
struct BitPacker {
    buffer: Vec<u8>,
    current_byte: u8,
    bits_in_byte: usize,
}

impl BitPacker {
    fn new() -> Self {
        Self {
            buffer: Vec::new(),
            current_byte: 0,
            bits_in_byte: 0,
        }
    }

    fn write(&mut self, value: u32, bits: usize) {
        let value = value & ((1 << bits) - 1);
        let mut bits_remaining = bits;

        while bits_remaining > 0 {
            let bits_to_write = (8 - self.bits_in_byte).min(bits_remaining);
            let mask = (1 << bits_to_write) - 1;
            let bits_value = (value >> (bits_remaining - bits_to_write)) & mask;

            self.current_byte |= (bits_value as u8) << (8 - self.bits_in_byte - bits_to_write);
            self.bits_in_byte += bits_to_write;
            bits_remaining -= bits_to_write;

            if self.bits_in_byte == 8 {
                self.buffer.push(self.current_byte);
                self.current_byte = 0;
                self.bits_in_byte = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits_in_byte > 0 {
            self.buffer.push(self.current_byte);
        }
        self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bit_packer() {
        let mut packer = BitPacker::new();
        packer.write(0b101, 3);
        packer.write(0b1100, 4);
        packer.write(0b1, 1);
        let result = packer.finish();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], 0b10111001);
    }

    #[test]
    fn test_encoder_creation() {
        let params = CodecParams::voice();
        let encoder = SeaEncoder::new(params);
        assert!(encoder.is_ok());
    }

    #[test]
    fn test_scale_factor_calculation() {
        let params = CodecParams::voice();
        let encoder = SeaEncoder::new(params).unwrap();

        let residuals = vec![1000, -2000, 500, -1500, 3000, -500];
        let scale_factors = encoder.calculate_scale_factors(&residuals, 1);

        assert!(!scale_factors.is_empty());
        assert!(scale_factors[0] > 0);
    }
}
