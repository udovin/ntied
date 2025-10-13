//! SEA (Simple Embedded Audio) codec implementation
//!
//! A low-complexity, lossy audio codec using LMS (Least Mean Squares) filtering
//! with variable bitrate support and packet loss concealment.

mod decoder;
mod encoder;
mod lms;

pub use decoder::SeaDecoder;
pub use encoder::SeaEncoder;

use super::traits::{AudioDecoder, AudioEncoder, CodecFactory, CodecParams, CodecType};
use anyhow::Result;

/// SEA codec configuration
#[derive(Debug, Clone)]
pub struct SeaConfig {
    /// Bitrate (1-8, where higher is better quality)
    pub bitrate: u8,
    /// Scale factor bits (2-8)
    pub scale_factor_bits: u8,
    /// Distance between scale factors in frames
    pub scale_factor_distance: u8,
    /// Enable Variable Bit Rate
    pub vbr: bool,
    /// Chunk size in frames
    pub chunk_size: usize,
}

impl Default for SeaConfig {
    fn default() -> Self {
        Self {
            bitrate: 4,                // 4 bits gives 16 levels - good balance
            scale_factor_bits: 5,      // More precision for scale factors
            scale_factor_distance: 16, // Update scale factors more frequently
            vbr: false,
            chunk_size: 960, // 20ms at 48kHz
        }
    }
}

impl SeaConfig {
    /// Create config optimized for voice
    pub fn voice() -> Self {
        Self {
            bitrate: 4, // Better quality for voice
            scale_factor_bits: 5,
            scale_factor_distance: 16,
            vbr: true,
            chunk_size: 960,
        }
    }

    /// Create config optimized for music
    pub fn music() -> Self {
        Self {
            bitrate: 6, // Higher quality for music
            scale_factor_bits: 6,
            scale_factor_distance: 12, // More frequent updates for dynamic music
            vbr: false,
            chunk_size: 2048,
        }
    }

    /// Create config optimized for low bandwidth
    pub fn low_bandwidth() -> Self {
        Self {
            bitrate: 3, // Minimum reasonable quality
            scale_factor_bits: 4,
            scale_factor_distance: 24, // Less frequent updates to save bits
            vbr: true,
            chunk_size: 480,
        }
    }
}

/// Factory for creating SEA codec instances
pub struct SeaCodecFactory;

impl CodecFactory for SeaCodecFactory {
    fn codec_type(&self) -> CodecType {
        CodecType::G722 // Using G722 as placeholder for SEA
    }

    fn is_available(&self) -> bool {
        true // Pure Rust implementation, always available
    }

    fn create_encoder(&self, params: CodecParams) -> Result<Box<dyn AudioEncoder>> {
        Ok(Box::new(SeaEncoder::new(params)?))
    }

    fn create_decoder(&self, params: CodecParams) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(SeaDecoder::new(params)?))
    }
}

/// Simplified quantization for SEA codec
pub(crate) mod quantization {
    /// Direct uniform quantization - no tables needed
    pub fn quantize(value: i32, bits: u8, scale: i32) -> u8 {
        if scale <= 1 {
            return 1 << (bits - 1); // Middle value for zero scale
        }

        // Normalize to -1.0 to 1.0 range
        let normalized = (value as f64 / scale as f64).clamp(-1.0, 1.0);

        // Map to 0..levels-1 with rounding
        let levels = (1 << bits) as f64;
        let quantized = ((normalized + 1.0) * 0.5 * levels).floor() as u8;

        quantized.min(((1 << bits) - 1) as u8)
    }

    /// Direct uniform dequantization
    pub fn dequantize(quant: u8, bits: u8, scale: i32) -> i32 {
        if scale <= 1 {
            return 0;
        }

        let levels = (1 << bits) as f64;

        // Map from 0..levels-1 to -1.0 to 1.0
        // Add 0.5 to center the quantization bin
        let normalized = ((quant as f64 + 0.5) / levels) * 2.0 - 1.0;

        // Scale back to original range
        (normalized * scale as f64).round() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sea_config() {
        let voice = SeaConfig::voice();
        assert_eq!(voice.bitrate, 4);
        assert!(voice.vbr);

        let music = SeaConfig::music();
        assert_eq!(music.bitrate, 6);
        assert!(!music.vbr);

        let low_bw = SeaConfig::low_bandwidth();
        assert_eq!(low_bw.bitrate, 3);
        assert!(low_bw.vbr);
    }

    #[test]
    fn test_quantization() {
        // Test quantize and dequantize round-trip
        let test_values = [-1000, -500, 0, 500, 1000];
        let scale = 2000; // Use larger scale to cover value range
        let bits = 3;

        for &value in &test_values {
            let quant = quantization::quantize(value, bits, scale);
            let dequant = quantization::dequantize(quant, bits, scale);

            // Allow some quantization error (one quantization step)
            let step_size = (scale * 2) / (1 << bits);
            let error = (value - dequant).abs();
            assert!(
                error <= step_size,
                "Error {} exceeds step size {} for value {}",
                error,
                step_size,
                value
            );
        }
    }

    #[test]
    fn test_sea_encode_decode_voice() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;

        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params.clone()).unwrap();

        // Create test frame (20ms at 48kHz mono = 960 samples)
        let frame_size = 960;
        let mut samples = vec![0.0f32; frame_size];

        // Generate a complex test signal (mix of frequencies)
        for i in 0..frame_size {
            let t = i as f32 / 48000.0;
            // Mix of 440Hz, 880Hz, and 220Hz
            samples[i] = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.3
                + (2.0 * std::f32::consts::PI * 880.0 * t).sin() * 0.2
                + (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.1;
        }

        // Encode
        let encoded = encoder.encode(&samples).unwrap();
        assert!(!encoded.is_empty());

        // Check compression ratio
        let uncompressed_size = samples.len() * 4; // 4 bytes per f32
        let compression_ratio = uncompressed_size as f32 / encoded.len() as f32;
        assert!(compression_ratio > 2.0); // Should achieve at least 2:1 compression

        // Decode
        let decoded = decoder.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), samples.len());

        // Check signal similarity (SEA is lossy, so allow some error)
        let mut total_error = 0.0f32;
        let mut total_energy = 0.0f32;
        for i in 0..samples.len() {
            total_error += (samples[i] - decoded[i]).powi(2);
            total_energy += samples[i].powi(2);
        }

        let snr = if total_error > 0.0 {
            10.0 * (total_energy / total_error).log10()
        } else {
            100.0 // Perfect reconstruction
        };

        // For 4-bit quantization with LMS prediction, expect positive SNR
        // The prediction should help achieve better than raw quantization
        assert!(snr > 3.0, "SNR too low: {:.2} dB", snr);
    }

    #[test]
    fn test_sea_encode_decode_music() {
        let params = CodecParams::music();
        let mut encoder = SeaEncoder::new(params.clone()).unwrap();
        let mut decoder = SeaDecoder::new(params.clone()).unwrap();

        // Create stereo test signal
        let frame_size = 2048;
        let mut samples = Vec::with_capacity(frame_size * 2);

        // Generate stereo signal with different content in each channel
        for i in 0..frame_size {
            let t = i as f32 / 48000.0;
            // Left channel - lower frequencies
            samples.push((2.0 * std::f32::consts::PI * 300.0 * t).sin() * 0.5);
            // Right channel - higher frequencies
            samples.push((2.0 * std::f32::consts::PI * 600.0 * t).sin() * 0.5);
        }

        // Encode
        let encoded = encoder.encode(&samples).unwrap();
        assert!(!encoded.is_empty());

        // Decode
        let decoded = decoder.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), samples.len());

        // Verify stereo separation is maintained
        let mut left_error = 0.0f32;
        let mut right_error = 0.0f32;
        for i in (0..decoded.len()).step_by(2) {
            left_error += (samples[i] - decoded[i]).abs();
            right_error += (samples[i + 1] - decoded[i + 1]).abs();
        }

        let avg_left_error = left_error / (frame_size as f32);
        let avg_right_error = right_error / (frame_size as f32);

        // Allow more error for lossy compression with 6-bit quantization
        assert!(
            avg_left_error < 0.5,
            "Left channel error too high: {}",
            avg_left_error
        );
        assert!(
            avg_right_error < 0.5,
            "Right channel error too high: {}",
            avg_right_error
        );
    }

    #[test]
    fn test_sea_packet_loss_concealment() {
        let params = CodecParams::voice();
        let mut encoder = SeaEncoder::new(params.clone()).unwrap();
        let mut decoder = SeaDecoder::new(params.clone()).unwrap();

        // Generate a continuous sine wave
        let frame_size = 960;
        let frequency = 440.0;
        let mut original_samples = Vec::new();

        // Generate multiple frames
        for frame in 0..3 {
            let mut samples = vec![0.0f32; frame_size];
            for i in 0..frame_size {
                let t = ((frame * frame_size) + i) as f32 / 48000.0;
                samples[i] = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.5;
            }
            original_samples.extend_from_slice(&samples);
        }

        // Encode and decode first frame
        let frame1 = &original_samples[0..frame_size];
        let encoded1 = encoder.encode(frame1).unwrap();
        let decoded1 = decoder.decode(&encoded1).unwrap();

        // Simulate packet loss for second frame - use PLC
        let plc_frame = decoder.conceal_packet_loss().unwrap();
        assert_eq!(plc_frame.len(), frame_size);

        // Encode and decode third frame
        let frame3 = &original_samples[frame_size * 2..frame_size * 3];
        let encoded3 = encoder.encode(frame3).unwrap();
        let decoded3 = decoder.decode(&encoded3).unwrap();

        // Check that PLC frame maintains continuity
        // The PLC frame should have similar characteristics to surrounding frames
        let mut plc_energy = 0.0f32;
        let mut orig_energy = 0.0f32;
        for i in 0..frame_size {
            plc_energy += plc_frame[i].powi(2);
            orig_energy += original_samples[frame_size + i].powi(2);
        }

        // PLC should maintain reasonable energy (with fade)
        // Allow wider range as LMS prediction can vary
        let energy_ratio = plc_energy / orig_energy.max(0.001);
        assert!(
            energy_ratio > 0.01 && energy_ratio < 10.0,
            "PLC energy ratio out of bounds: {:.2}",
            energy_ratio
        );

        // Check decoder stats
        let stats = decoder.stats();
        assert_eq!(stats.fec_recoveries, 1);
        assert_eq!(stats.frames_decoded, 2); // First and third frame
    }

    #[test]
    fn test_sea_consecutive_packet_loss() {
        let params = CodecParams::voice();
        let mut decoder = SeaDecoder::new(params.clone()).unwrap();
        let mut encoder = SeaEncoder::new(params.clone()).unwrap();

        // Generate and decode one good frame first
        let frame_size = 960;
        let mut samples = vec![0.0f32; frame_size];
        for i in 0..frame_size {
            samples[i] = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin() * 0.3;
        }

        let encoded = encoder.encode(&samples).unwrap();
        decoder.decode(&encoded).unwrap();

        // Simulate multiple consecutive packet losses
        let mut plc_energies = Vec::new();
        for _ in 0..5 {
            let plc_frame = decoder.conceal_packet_loss().unwrap();
            let energy: f32 = plc_frame.iter().map(|s| s.powi(2)).sum();
            plc_energies.push(energy);
        }

        // Energy should decrease with each consecutive PLC (fade effect)
        for i in 1..plc_energies.len() {
            assert!(
                plc_energies[i] <= plc_energies[i - 1] * 1.1, // Allow small tolerance
                "PLC energy should decrease: {} > {}",
                plc_energies[i],
                plc_energies[i - 1]
            );
        }
    }

    #[test]
    fn test_sea_low_bandwidth_mode() {
        let mut params = CodecParams::voice();
        params.bitrate = 16000; // Low bandwidth

        let mut encoder = SeaEncoder::new(params.clone()).unwrap();
        let mut decoder = SeaDecoder::new(params.clone()).unwrap();

        // Generate test signal
        let frame_size = 480; // Smaller frame for low bandwidth
        let mut samples = vec![0.0f32; frame_size];
        for i in 0..frame_size {
            samples[i] = ((i as f32) / 100.0).sin() * 0.5;
        }

        // Encode
        let encoded = encoder.encode(&samples).unwrap();

        // Should achieve good compression for low bandwidth
        let compression_ratio = (samples.len() * 4) as f32 / encoded.len() as f32;
        assert!(compression_ratio > 2.0);

        // Decode
        let decoded = decoder.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), samples.len());
    }

    #[test]
    fn test_sea_bitrate_adaptation() {
        let params = CodecParams::voice();
        let mut encoder = SeaEncoder::new(params).unwrap();

        // Create test samples
        let samples = vec![0.1f32; 960];

        // Test different bitrate settings
        let bitrates = [16000u32, 32000, 64000];
        let mut encoded_sizes = Vec::new();

        for &bitrate in &bitrates {
            encoder.set_bitrate(bitrate).unwrap();
            let encoded = encoder.encode(&samples).unwrap();
            encoded_sizes.push(encoded.len());
        }

        // Higher bitrates should produce larger encoded data
        for i in 1..encoded_sizes.len() {
            assert!(
                encoded_sizes[i] >= encoded_sizes[i - 1],
                "Higher bitrate should produce larger output: {} < {}",
                encoded_sizes[i],
                encoded_sizes[i - 1]
            );
        }
    }

    #[test]
    fn test_sea_silence_encoding() {
        let params = CodecParams::voice();
        let mut encoder = SeaEncoder::new(params.clone()).unwrap();
        let mut decoder = SeaDecoder::new(params.clone()).unwrap();

        // Encode silence
        let silence = vec![0.0f32; 960];
        let encoded = encoder.encode(&silence).unwrap();

        // Silence should compress reasonably well
        let compression_ratio = (silence.len() * 4) as f32 / encoded.len() as f32;
        assert!(compression_ratio > 2.0);

        // Decode
        let decoded = decoder.decode(&encoded).unwrap();

        // Decoded silence should be near zero
        let max_amplitude = decoded.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        assert!(max_amplitude < 0.01);
    }

    #[test]
    fn test_sea_factory() {
        let factory = SeaCodecFactory;

        assert_eq!(factory.codec_type(), CodecType::G722);
        assert!(factory.is_available());

        // Test encoder creation
        let encoder = factory.create_encoder(CodecParams::voice());
        assert!(encoder.is_ok());

        // Test decoder creation
        let decoder = factory.create_decoder(CodecParams::voice());
        assert!(decoder.is_ok());
    }

    #[test]
    fn test_sea_reset() {
        let params = CodecParams::voice();
        let mut encoder = SeaEncoder::new(params.clone()).unwrap();
        let mut decoder = SeaDecoder::new(params.clone()).unwrap();

        // Process some data
        let samples = vec![0.5f32; 960];
        let encoded = encoder.encode(&samples).unwrap();
        decoder.decode(&encoded).unwrap();

        // Reset
        encoder.reset().unwrap();
        decoder.reset().unwrap();

        // Should work normally after reset
        let encoded2 = encoder.encode(&samples).unwrap();
        let decoded2 = decoder.decode(&encoded2).unwrap();
        assert_eq!(decoded2.len(), samples.len());
    }
}
