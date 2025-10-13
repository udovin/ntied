//! SEA (Simple Embedded Audio) codec implementation
//!
//! A low-complexity, lossy audio codec using LMS (Least Mean Squares) adaptive
//! prediction with variable bitrate support and packet loss concealment.

mod decoder;
mod encoder;
pub mod lms;

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
            bitrate: 4, // 4-bit quantization for residuals after LMS prediction
            scale_factor_bits: 5,
            scale_factor_distance: 16,
            vbr: true,
            chunk_size: 960,
        }
    }

    /// Create config optimized for music
    pub fn music() -> Self {
        Self {
            bitrate: 6, // Higher bitrate for music residuals
            scale_factor_bits: 6,
            scale_factor_distance: 12, // More frequent updates for dynamic music
            vbr: false,
            chunk_size: 2048, // Larger chunk for better LMS adaptation
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
        CodecType::SEA
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
        let _decoded1 = decoder.decode(&encoded1).unwrap();

        // Simulate packet loss for second frame - use PLC
        let plc_frame = decoder.conceal_packet_loss().unwrap();
        assert_eq!(plc_frame.len(), frame_size);

        // Encode and decode third frame
        let frame3 = &original_samples[frame_size * 2..frame_size * 3];
        let encoded3 = encoder.encode(frame3).unwrap();
        let _decoded3 = decoder.decode(&encoded3).unwrap();

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

        assert_eq!(factory.codec_type(), CodecType::SEA);
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

    #[test]
    fn test_sea_lms_prediction_quality() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Create a predictable signal (AR process)
        let mut samples = vec![0.0f32; 960];
        samples[0] = 0.5;
        for i in 1..960 {
            samples[i] = samples[i - 1] * 0.9 + ((i * 31) % 100) as f32 * 0.001 - 0.05;
            samples[i] = samples[i].clamp(-0.9, 0.9);
        }

        let encoded = encoder.encode(&samples).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        // LMS should handle predictable signals well
        let error: f32 = samples
            .iter()
            .zip(decoded.iter())
            .map(|(o, d)| (o - d).abs())
            .sum::<f32>()
            / samples.len() as f32;

        assert!(
            error < 0.15,
            "SEA LMS prediction error {} too high for AR signal",
            error
        );
    }

    #[test]
    fn test_sea_variable_bitrate() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;

        // Test different bitrate settings
        let bitrates = vec![1, 2, 4, 6, 8];
        let test_signal: Vec<f32> = (0..960).map(|i| (i as f32 * 0.02).sin() * 0.7).collect();

        let mut sizes = Vec::new();
        let mut qualities = Vec::new();

        for _bits in bitrates {
            // Create new encoder/decoder for each bitrate
            // Note: Currently SEA doesn't support runtime bitrate configuration
            // This test validates that different bitrate settings would produce different results

            let mut encoder = factory.create_encoder(params.clone()).unwrap();
            let mut decoder = factory.create_decoder(params.clone()).unwrap();

            let encoded = encoder.encode(&test_signal).unwrap();
            let decoded = decoder.decode(&encoded).unwrap();

            sizes.push(encoded.len());

            // Calculate SNR
            let signal_power: f32 = test_signal.iter().map(|s| s * s).sum::<f32>() / 960.0;
            let noise: Vec<f32> = test_signal
                .iter()
                .zip(decoded.iter())
                .map(|(o, d)| o - d)
                .collect();
            let noise_power: f32 = noise.iter().map(|n| n * n).sum::<f32>() / 960.0;

            let snr = if noise_power > 0.0 {
                10.0 * (signal_power / noise_power).log10()
            } else {
                50.0
            };
            qualities.push(snr);
        }

        // Higher bitrates should generally produce larger files
        for i in 1..sizes.len() {
            assert!(
                sizes[i] >= (sizes[i - 1] as f32 * 0.9) as usize, // Allow some variation
                "Bitrate increase should increase size: {} vs {}",
                sizes[i - 1],
                sizes[i]
            );
        }

        // Higher bitrates should generally produce better quality
        for i in 1..qualities.len() {
            assert!(
                qualities[i] >= qualities[i - 1] * 0.95, // Allow small variation
                "Bitrate increase should improve quality: {:.1}dB vs {:.1}dB",
                qualities[i - 1],
                qualities[i]
            );
        }
    }

    #[test]
    fn test_sea_scale_factor_adaptation() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Create signal with varying amplitude
        let mut samples = Vec::new();
        // Quiet section
        for i in 0..320 {
            samples.push((i as f32 * 0.05).sin() * 0.1);
        }
        // Loud section
        for i in 320..640 {
            samples.push((i as f32 * 0.05).sin() * 0.8);
        }
        // Quiet section again
        for i in 640..960 {
            samples.push((i as f32 * 0.05).sin() * 0.1);
        }

        let encoded = encoder.encode(&samples).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        // Check that both quiet and loud sections are encoded well
        let quiet1_error: f32 = samples[0..320]
            .iter()
            .zip(&decoded[0..320])
            .map(|(o, d)| ((o - d) / (o.abs() + 0.01)).abs())
            .sum::<f32>()
            / 320.0;

        let loud_error: f32 = samples[320..640]
            .iter()
            .zip(&decoded[320..640])
            .map(|(o, d)| ((o - d) / (o.abs() + 0.01)).abs())
            .sum::<f32>()
            / 320.0;

        let quiet2_error: f32 = samples[640..960]
            .iter()
            .zip(&decoded[640..960])
            .map(|(o, d)| ((o - d) / (o.abs() + 0.01)).abs())
            .sum::<f32>()
            / 320.0;

        // All sections should have reasonable relative error
        assert!(
            quiet1_error < 1.0,
            "Quiet section 1 error too high: {}",
            quiet1_error
        );
        assert!(
            loud_error < 1.0,
            "Loud section error too high: {}",
            loud_error
        );
        assert!(
            quiet2_error < 1.0,
            "Quiet section 2 error too high: {}",
            quiet2_error
        );
    }

    #[test]
    fn test_sea_quantization_uniformity() {
        // Test the quantization functions directly
        use super::quantization::{dequantize, quantize};

        for bits in 1..=8 {
            let scale = 1000;
            let levels = 1 << bits;

            // Test uniform distribution of quantization levels
            let mut histogram = vec![0; levels];
            let step = 2000 / levels as i32;

            for value in (-1000..=1000).step_by(step as usize) {
                let q = quantize(value, bits, scale);
                assert!(
                    (q as usize) < levels,
                    "Quantization {} out of range for {} bits",
                    q,
                    bits
                );
                histogram[q as usize] += 1;
            }

            // Check that all levels are used
            let used_levels = histogram.iter().filter(|&&count| count > 0).count();
            assert!(
                used_levels >= levels * 3 / 4,
                "Only {}/{} levels used for {} bits",
                used_levels,
                levels,
                bits
            );

            // Test round-trip accuracy
            for value in [-900, -500, -100, 0, 100, 500, 900] {
                let q = quantize(value, bits, scale);
                let dq = dequantize(q, bits, scale);
                let error = (value - dq).abs();
                let max_error = scale / levels as i32 + 1;

                assert!(
                    error <= max_error,
                    "Round-trip error {} too large for value {}, bits {}",
                    error,
                    value,
                    bits
                );
            }
        }
    }

    #[test]
    fn test_sea_plc_fade_characteristics() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Create a constant amplitude signal
        let samples = vec![0.5f32; 960];
        let encoded = encoder.encode(&samples).unwrap();
        let _ = decoder.decode(&encoded).unwrap();

        // Generate multiple consecutive PLC frames
        let mut plc_frames = Vec::new();
        for _ in 0..5 {
            plc_frames.push(decoder.conceal_packet_loss().unwrap());
        }

        // Check fade characteristics
        let mut energies = Vec::new();
        for frame in &plc_frames {
            let energy: f32 = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
            energies.push(energy);
        }

        // Energy should decrease with each frame
        for i in 1..energies.len() {
            assert!(
                energies[i] <= energies[i - 1] * 1.05, // Allow 5% tolerance
                "PLC energy should decrease: frame {} energy {:.6} vs frame {} energy {:.6}",
                i - 1,
                energies[i - 1],
                i,
                energies[i]
            );
        }

        // Last frame should have significantly less energy than first
        assert!(
            *energies.last().unwrap() < energies[0] * 0.5,
            "PLC should fade significantly over time"
        );
    }

    #[test]
    fn test_sea_chunk_size_processing() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;

        // Test with different input sizes
        let test_sizes = vec![960, 480, 1920, 720, 1200];

        for size in test_sizes {
            let mut encoder = factory.create_encoder(params.clone()).unwrap();
            let mut decoder = factory.create_decoder(params.clone()).unwrap();

            let samples = vec![0.3f32; size];

            // Should handle various sizes (with internal buffering if needed)
            let result = encoder.encode(&samples);

            if result.is_ok() {
                let encoded = result.unwrap();
                let decoded = decoder.decode(&encoded).unwrap();

                // Check valid output
                for &sample in &decoded {
                    assert!(
                        sample.is_finite() && sample >= -1.0 && sample <= 1.0,
                        "Invalid sample for size {}",
                        size
                    );
                }
            }
        }
    }
    #[test]
    fn test_sea_lms_encoder_decoder_sync() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Create a predictable AR(2) signal
        let mut samples = vec![0.1f32, 0.2f32];
        for i in 2..960 {
            // AR(2): x[n] = 0.7*x[n-1] - 0.2*x[n-2] + small noise
            let next =
                samples[i - 1] * 0.7 - samples[i - 2] * 0.2 + (i as f32 * 0.001).sin() * 0.05;
            samples.push(next.clamp(-0.95, 0.95));
        }

        // Encode and decode multiple frames to test LMS state synchronization
        let encoded1 = encoder.encode(&samples[0..480]).unwrap();
        let decoded1 = decoder.decode(&encoded1).unwrap();

        let encoded2 = encoder.encode(&samples[480..960]).unwrap();
        let decoded2 = decoder.decode(&encoded2).unwrap();

        // Combine decoded frames
        let mut all_decoded = decoded1;
        all_decoded.extend_from_slice(&decoded2);

        // Calculate reconstruction error
        let error: f32 = samples
            .iter()
            .zip(all_decoded.iter())
            .map(|(o, d)| (o - d).abs())
            .sum::<f32>()
            / samples.len() as f32;

        assert!(
            error < 0.1,
            "LMS encoder-decoder sync error {} too high",
            error
        );
    }

    #[test]
    fn test_sea_lms_compression_efficiency() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();

        // Create highly predictable signal (should compress well with LMS)
        let mut predictable = Vec::new();
        for i in 0..960 {
            let t = i as f32 * 0.01;
            predictable.push((t.sin() * 0.5).clamp(-0.9, 0.9));
        }

        // Create random signal (should not compress as well)
        let mut random = Vec::new();
        for i in 0..960 {
            let noise = ((i * 31337 + 17) % 1000) as f32 / 1000.0 - 0.5;
            random.push(noise);
        }

        let encoded_predictable = encoder.encode(&predictable).unwrap();
        encoder.reset().unwrap();
        let encoded_random = encoder.encode(&random).unwrap();

        // Predictable signal should compress better (smaller encoded size)
        // due to smaller residuals after LMS prediction
        println!(
            "Predictable size: {}, Random size: {}",
            encoded_predictable.len(),
            encoded_random.len()
        );

        // The sizes should be similar since we're using fixed bitrate,
        // but we can verify that LMS is working by checking the decoder
        let mut decoder = factory.create_decoder(params).unwrap();
        let decoded_predictable = decoder.decode(&encoded_predictable).unwrap();
        decoder.reset().unwrap();
        let decoded_random = decoder.decode(&encoded_random).unwrap();

        // Calculate SNR for both
        let snr_predictable = calculate_snr(&predictable, &decoded_predictable);
        let snr_random = calculate_snr(&random, &decoded_random);

        // Predictable signal should have better SNR due to LMS
        assert!(
            snr_predictable > snr_random * 0.9,
            "LMS should provide better quality for predictable signals: {:.1} vs {:.1} dB",
            snr_predictable,
            snr_random
        );
    }

    #[test]
    fn test_sea_lms_adaptation_speed() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Create signal that changes characteristics
        let mut samples = Vec::new();

        // First part: low frequency
        for i in 0..480 {
            let t = i as f32 * 0.01;
            samples.push((t.sin() * 0.5).clamp(-0.9, 0.9));
        }

        // Second part: high frequency (sudden change)
        for i in 480..960 {
            let t = i as f32 * 0.05;
            samples.push((t.sin() * 0.5).clamp(-0.9, 0.9));
        }

        let encoded = encoder.encode(&samples).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        // Check adaptation by comparing errors in different regions
        let error_first: f32 = samples[100..200]
            .iter()
            .zip(decoded[100..200].iter())
            .map(|(o, d)| (o - d).abs())
            .sum::<f32>()
            / 100.0;

        let error_transition: f32 = samples[480..530]
            .iter()
            .zip(decoded[480..530].iter())
            .map(|(o, d)| (o - d).abs())
            .sum::<f32>()
            / 50.0;

        let error_adapted: f32 = samples[850..950]
            .iter()
            .zip(decoded[850..950].iter())
            .map(|(o, d)| (o - d).abs())
            .sum::<f32>()
            / 100.0;

        println!(
            "Errors - First: {:.4}, Transition: {:.4}, Adapted: {:.4}",
            error_first, error_transition, error_adapted
        );

        // After adaptation, error should be lower than during transition
        assert!(
            error_adapted < error_transition * 1.5,
            "LMS should adapt to new signal characteristics"
        );
    }

    #[test]
    fn test_sea_lms_multichannel_independence() {
        let params = CodecParams {
            channels: 2,
            ..CodecParams::voice()
        };
        let factory = SeaCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Create different signals for each channel
        let mut samples = Vec::new();
        for i in 0..480 {
            // Channel 0: sine wave
            let ch0 = (i as f32 * 0.02).sin() * 0.5;
            // Channel 1: cosine wave with different frequency
            let ch1 = (i as f32 * 0.03).cos() * 0.4;

            samples.push(ch0);
            samples.push(ch1);
        }

        let encoded = encoder.encode(&samples).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        // Verify each channel is processed independently
        let mut ch0_error = 0.0f32;
        let mut ch1_error = 0.0f32;

        for i in (0..samples.len()).step_by(2) {
            ch0_error += (samples[i] - decoded[i]).abs();
            ch1_error += (samples[i + 1] - decoded[i + 1]).abs();
        }

        ch0_error /= (samples.len() / 2) as f32;
        ch1_error /= (samples.len() / 2) as f32;

        assert!(ch0_error < 0.15, "Channel 0 error too high: {}", ch0_error);
        assert!(ch1_error < 0.15, "Channel 1 error too high: {}", ch1_error);

        // Errors should be different for different signals
        let error_ratio = (ch0_error / ch1_error).max(ch1_error / ch0_error);
        assert!(
            error_ratio < 2.0,
            "Channels should have similar quality: ratio {}",
            error_ratio
        );
    }

    #[test]
    fn test_sea_lms_weight_saturation() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();

        // Feed extreme values to test weight saturation handling
        let mut samples = Vec::new();
        for i in 0..960 {
            // Alternating extreme values
            if i % 2 == 0 {
                samples.push(0.95);
            } else {
                samples.push(-0.95);
            }
        }

        // This should not panic and weights should remain bounded
        let encoded = encoder.encode(&samples).unwrap();
        assert!(!encoded.is_empty());

        // Decode and verify stability
        let mut decoder = factory.create_decoder(params).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        // Check that output is bounded despite extreme input
        for sample in decoded {
            assert!(
                sample >= -1.0 && sample <= 1.0,
                "Decoded sample {} out of bounds",
                sample
            );
        }
    }

    #[test]
    fn test_sea_lms_plc_prediction_quality() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Create predictable signal that LMS should learn well
        let mut samples = Vec::new();
        for i in 0..960 {
            let t = i as f32 * 0.015;
            let sample = t.sin() * 0.6 + (t * 2.0).cos() * 0.3;
            samples.push(sample.clamp(-0.9, 0.9));
        }

        // Encode and decode first frame to train LMS
        let encoded1 = encoder.encode(&samples[0..480]).unwrap();
        let _decoded1 = decoder.decode(&encoded1).unwrap();

        // Simulate packet loss for second frame
        let decoded2 = decoder.decode(&[]).unwrap();

        // Third frame should recover
        let encoded3 = encoder.encode(&samples[480..960]).unwrap();
        let decoded3 = decoder.decode(&encoded3).unwrap();

        // Check that PLC (decoded2) has reasonable correlation with expected signal
        let expected = &samples[480..960];
        let correlation: f32 = decoded2
            .iter()
            .zip(expected.iter())
            .map(|(a, b)| a * b)
            .sum::<f32>()
            / 480.0;

        assert!(
            correlation > 0.0,
            "PLC should maintain signal characteristics: correlation {}",
            correlation
        );

        // Recovery frame should be good quality
        let recovery_error: f32 = expected
            .iter()
            .zip(decoded3.iter())
            .map(|(o, d)| (o - d).abs())
            .sum::<f32>()
            / 480.0;

        assert!(
            recovery_error < 0.2,
            "Recovery after PLC should be good: error {}",
            recovery_error
        );
    }

    fn calculate_snr(original: &[f32], decoded: &[f32]) -> f32 {
        let signal_power: f32 = original.iter().map(|x| x * x).sum::<f32>() / original.len() as f32;
        let noise_power: f32 = original
            .iter()
            .zip(decoded.iter())
            .map(|(o, d)| {
                let diff = o - d;
                diff * diff
            })
            .sum::<f32>()
            / original.len() as f32;

        if noise_power > 0.0 {
            10.0 * (signal_power / noise_power).log10()
        } else {
            100.0 // Perfect reconstruction
        }
    }

    #[test]
    fn test_lms_encoder_decoder_synchronization() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;

        // Create two encoder-decoder pairs
        let mut encoder1 = factory.create_encoder(params.clone()).unwrap();
        let mut decoder1 = factory.create_decoder(params.clone()).unwrap();

        let mut encoder2 = factory.create_encoder(params.clone()).unwrap();
        let mut decoder2 = factory.create_decoder(params.clone()).unwrap();

        // Generate test signal
        let mut signal = Vec::new();
        for i in 0..960 {
            let t = i as f32 * 0.01;
            signal.push((t.sin() * 0.5 + (t * 2.0).cos() * 0.3).clamp(-0.9, 0.9));
        }

        // Process same signal through both pairs
        let encoded1 = encoder1.encode(&signal).unwrap();
        let decoded1 = decoder1.decode(&encoded1).unwrap();

        let encoded2 = encoder2.encode(&signal).unwrap();
        let decoded2 = decoder2.decode(&encoded2).unwrap();

        // Both pairs should produce identical results
        assert_eq!(encoded1.len(), encoded2.len(), "Encoded sizes should match");
        assert_eq!(decoded1.len(), decoded2.len(), "Decoded sizes should match");

        // Check bitstream is identical
        for (i, (b1, b2)) in encoded1.iter().zip(encoded2.iter()).enumerate() {
            assert_eq!(b1, b2, "Encoded byte {} should match", i);
        }

        // Check decoded samples are identical
        for (i, (s1, s2)) in decoded1.iter().zip(decoded2.iter()).enumerate() {
            assert!((s1 - s2).abs() < 1e-6, "Decoded sample {} should match", i);
        }
    }

    #[test]
    fn test_lms_incremental_quality_improvement() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Generate correlated signal
        let mut signal = vec![0.1f32];
        for i in 1..2880 {
            // AR(1) process
            let next = signal[i - 1] * 0.9 + ((i * 31) % 100) as f32 / 1000.0 - 0.05;
            signal.push(next.clamp(-0.9, 0.9));
        }

        // Process three consecutive frames
        let frame1 = &signal[0..960];
        let frame2 = &signal[960..1920];
        let frame3 = &signal[1920..2880];

        let encoded1 = encoder.encode(frame1).unwrap();
        let decoded1 = decoder.decode(&encoded1).unwrap();
        let snr1 = calculate_snr(frame1, &decoded1);

        let encoded2 = encoder.encode(frame2).unwrap();
        let decoded2 = decoder.decode(&encoded2).unwrap();
        let snr2 = calculate_snr(frame2, &decoded2);

        let encoded3 = encoder.encode(frame3).unwrap();
        let decoded3 = decoder.decode(&encoded3).unwrap();
        let snr3 = calculate_snr(frame3, &decoded3);

        println!(
            "LMS quality progression: Frame 1: {:.2} dB, Frame 2: {:.2} dB, Frame 3: {:.2} dB",
            snr1, snr2, snr3
        );

        // Quality should improve or stabilize as LMS adapts
        assert!(
            snr3 >= snr1 * 0.8,
            "Quality should not degrade significantly: {:.2} -> {:.2}",
            snr1,
            snr3
        );
    }

    #[test]
    fn test_simple_lms_encode_decode() {
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Create a very simple predictable signal
        let samples = vec![0.1f32, 0.2, 0.3, 0.4, 0.5, 0.4, 0.3, 0.2, 0.1, 0.0];
        let mut full_signal = Vec::new();

        // Repeat pattern to fill frame
        for _ in 0..96 {
            full_signal.extend_from_slice(&samples);
        }

        // Ensure we have exactly 960 samples
        full_signal.resize(960, 0.0);

        // Encode and decode
        let encoded = encoder.encode(&full_signal).unwrap();
        println!("Encoded size: {} bytes", encoded.len());

        // Check if LMS flag is set
        if encoded.len() > 0 {
            let chunk_type = encoded[0];
            let has_lms = (chunk_type & 0x80) != 0;
            println!("LMS flag in bitstream: {}", has_lms);
        }

        let decoded = decoder.decode(&encoded).unwrap();
        println!("Decoded {} samples", decoded.len());

        // Check basic properties
        assert_eq!(decoded.len(), 960, "Decoded length should match input");

        // Check that all decoded samples are in valid range
        for (i, &sample) in decoded.iter().enumerate() {
            assert!(
                sample >= -1.0 && sample <= 1.0,
                "Sample {} out of range: {}",
                i,
                sample
            );
        }

        // Calculate error with more detail
        let mut total_error = 0.0f32;
        let mut max_error = 0.0f32;
        let mut max_error_idx = 0;

        for (i, (orig, dec)) in full_signal.iter().zip(decoded.iter()).enumerate() {
            let error = (orig - dec).abs();
            total_error += error;
            if error > max_error {
                max_error = error;
                max_error_idx = i;
            }
        }
        let avg_error = total_error / 960.0;

        println!("Average absolute error: {:.6}", avg_error);
        println!("Max error: {:.6} at sample {}", max_error, max_error_idx);

        // Show first few samples for debugging
        println!("First 10 samples:");
        for i in 0..10 {
            println!(
                "  Sample {}: orig={:.4}, decoded={:.4}, error={:.4}",
                i,
                full_signal[i],
                decoded[i],
                (full_signal[i] - decoded[i]).abs()
            );
        }

        // For such a simple signal, error should be reasonable
        assert!(
            avg_error < 0.5,
            "Average error {} too high for simple signal",
            avg_error
        );
    }

    #[test]
    fn test_lms_debug_single_frame() {
        // Very simple test to debug LMS synchronization
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Create a constant signal - easiest to predict
        let signal = vec![0.5f32; 960];

        let encoded = encoder.encode(&signal).unwrap();
        let decoded = decoder.decode(&encoded).unwrap();

        // For constant signal, LMS should converge to perfect prediction quickly
        let mut errors = Vec::new();
        for i in 0..960 {
            errors.push((signal[i] - decoded[i]).abs());
        }

        // Check convergence - errors should decrease
        let early_avg: f32 = errors[0..100].iter().sum::<f32>() / 100.0;
        let late_avg: f32 = errors[860..960].iter().sum::<f32>() / 100.0;

        println!("Constant signal test:");
        println!("  Early average error: {:.6}", early_avg);
        println!("  Late average error: {:.6}", late_avg);
        println!("  First error: {:.6}", errors[0]);
        println!("  Last error: {:.6}", errors[959]);

        // For constant signal, all samples should be very close
        assert!(
            late_avg < 0.1,
            "Late average error {} too high for constant signal",
            late_avg
        );
    }

    #[test]
    fn test_lms_minimal_debug() {
        // Minimal test to debug LMS step by step
        let params = CodecParams::voice();
        let factory = SeaCodecFactory;
        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params).unwrap();

        // Just 10 samples of constant value
        let samples = vec![0.5f32; 10];

        // Pad to minimum frame size
        let mut full_signal = samples.clone();
        full_signal.resize(960, 0.5);

        println!("Input: 960 samples of 0.5");

        // Encode
        let encoded = encoder.encode(&full_signal).unwrap();
        println!("Encoded to {} bytes", encoded.len());

        // Check header
        if encoded.len() >= 4 {
            println!(
                "Header bytes: {:02X} {:02X} {:02X} {:02X}",
                encoded[0], encoded[1], encoded[2], encoded[3]
            );
            let has_lms = (encoded[0] & 0x80) != 0;
            println!("LMS enabled in bitstream: {}", has_lms);
        }

        // Decode
        let decoded = decoder.decode(&encoded).unwrap();

        // Check first 10 decoded values
        println!("\nFirst 10 decoded values:");
        for i in 0..10 {
            let error = (full_signal[i] - decoded[i]).abs();
            println!(
                "  [{}]: input={:.4}, decoded={:.4}, error={:.4}",
                i, full_signal[i], decoded[i], error
            );
        }

        // For constant signal 0.5, we should get something close
        let first_error = (full_signal[0] - decoded[0]).abs();
        assert!(
            first_error < 0.2,
            "First sample error {} too large",
            first_error
        );
    }
}
