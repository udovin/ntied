use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Result, anyhow};
use audiopus::coder::{Decoder, Encoder};
use audiopus::{Application, Bitrate, Channels, SampleRate};

use super::traits::{AudioDecoder, AudioEncoder, CodecFactory, CodecParams, CodecStats, CodecType};

/// Opus encoder implementation
pub struct OpusEncoder {
    encoder: Encoder,
    params: CodecParams,
    stats: CodecStats,
    frame_size: usize,
    encode_times: Vec<f64>,
}

impl OpusEncoder {
    /// Create a new Opus encoder
    pub fn new(params: CodecParams) -> Result<Self> {
        let channels = match params.channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            _ => return Err(anyhow!("Opus only supports 1 or 2 channels")),
        };

        let sample_rate = SampleRate::try_from(params.sample_rate as i32)
            .map_err(|_| anyhow!("Unsupported sample rate: {}", params.sample_rate))?;

        let mut encoder = Encoder::new(
            sample_rate,
            channels,
            Application::Voip, // Optimized for voice
        )?;

        // Configure encoder
        if params.bitrate > 0 {
            encoder.set_bitrate(Bitrate::BitsPerSecond(params.bitrate as i32))?;
        } else {
            encoder.set_bitrate(Bitrate::Auto)?;
        }

        // Set complexity
        encoder.set_complexity(params.complexity as i32)?;

        // Enable in-band FEC for packet loss resilience
        if params.fec {
            encoder.set_inband_fec(true)?;
        }

        // Enable DTX for bandwidth savings during silence
        if params.dtx {
            encoder.set_dtx(true)?;
        }

        // Set expected packet loss for FEC optimization
        if params.expected_packet_loss > 0 {
            encoder.set_packet_loss_perc(params.expected_packet_loss as i32)?;
        }

        // Calculate frame size (20ms for voice)
        let frame_size = (params.sample_rate * 20 / 1000) as usize;

        Ok(Self {
            encoder,
            params,
            stats: CodecStats::default(),
            frame_size,
            encode_times: Vec::with_capacity(100),
        })
    }

    fn update_stats(&mut self, encode_time: f64, output_size: usize) {
        self.stats.frames_encoded += 1;
        self.stats.bytes_encoded += output_size as u64;

        // Update average encode time
        self.encode_times.push(encode_time);
        if self.encode_times.len() > 100 {
            self.encode_times.remove(0);
        }
        self.stats.avg_encode_time_us =
            self.encode_times.iter().sum::<f64>() / self.encode_times.len() as f64;

        // Calculate current bitrate (bits per second)
        let frame_duration_ms = (self.frame_size * 1000) / self.params.sample_rate as usize;
        self.stats.current_bitrate = (output_size * 8 * 1000) as u32 / frame_duration_ms as u32;
    }
}

impl AudioEncoder for OpusEncoder {
    fn encode(&mut self, samples: &[f32]) -> Result<Vec<u8>> {
        let start = Instant::now();

        // Ensure we have the correct frame size
        if samples.len() != self.frame_size * self.params.channels as usize {
            return Err(anyhow!(
                "Invalid sample count: expected {}, got {}",
                self.frame_size * self.params.channels as usize,
                samples.len()
            ));
        }

        // Opus can encode up to 1275 bytes per frame
        let mut output = vec![0u8; 1275];

        // audiopus accepts f32 samples directly
        // Encode the frame
        let encoded_len = self.encoder.encode_float(samples, &mut output)?;

        output.truncate(encoded_len);

        // Check if this was a DTX frame (very small size indicates silence)
        if encoded_len <= 3 {
            self.stats.dtx_packets += 1;
        }

        let encode_time = start.elapsed().as_micros() as f64;
        self.update_stats(encode_time, encoded_len);

        Ok(output)
    }

    fn reset(&mut self) -> Result<()> {
        self.encoder.reset_state()?;
        Ok(())
    }

    fn params(&self) -> &CodecParams {
        &self.params
    }

    fn set_params(&mut self, params: CodecParams) -> Result<()> {
        // Recreate encoder with new params if sample rate or channels changed
        if params.sample_rate != self.params.sample_rate || params.channels != self.params.channels
        {
            *self = OpusEncoder::new(params)?;
        } else {
            // Otherwise just update the existing encoder
            if params.bitrate > 0 {
                self.encoder
                    .set_bitrate(Bitrate::BitsPerSecond(params.bitrate as i32))?;
            }
            self.encoder.set_complexity(params.complexity as i32)?;
            self.encoder.set_inband_fec(params.fec)?;
            self.encoder.set_dtx(params.dtx)?;
            self.encoder
                .set_packet_loss_perc(params.expected_packet_loss as i32)?;
            self.params = params;
        }
        Ok(())
    }

    fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        self.encoder
            .set_bitrate(Bitrate::BitsPerSecond(bitrate as i32))?;
        self.params.bitrate = bitrate;
        Ok(())
    }

    fn set_packet_loss(&mut self, percentage: u8) -> Result<()> {
        self.encoder.set_packet_loss_perc(percentage as i32)?;
        self.params.expected_packet_loss = percentage;
        Ok(())
    }

    fn stats(&self) -> &CodecStats {
        &self.stats
    }
}

/// Opus decoder implementation
pub struct OpusDecoder {
    decoder: Decoder,
    params: CodecParams,
    stats: CodecStats,
    frame_size: usize,
    decode_times: Vec<f64>,
}

impl OpusDecoder {
    /// Create a new Opus decoder
    pub fn new(params: CodecParams) -> Result<Self> {
        let channels = match params.channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            _ => return Err(anyhow!("Opus only supports 1 or 2 channels")),
        };

        let sample_rate = SampleRate::try_from(params.sample_rate as i32)
            .map_err(|_| anyhow!("Unsupported sample rate: {}", params.sample_rate))?;

        let decoder = Decoder::new(sample_rate, channels)?;

        // Calculate frame size (20ms for voice)
        let frame_size = (params.sample_rate * 20 / 1000) as usize;

        Ok(Self {
            decoder,
            params,
            stats: CodecStats::default(),
            frame_size,
            decode_times: Vec::with_capacity(100),
        })
    }

    fn update_stats(&mut self, decode_time: f64, input_size: usize, success: bool) {
        if success {
            self.stats.frames_decoded += 1;
            self.stats.bytes_decoded += input_size as u64;

            // Update average decode time
            self.decode_times.push(decode_time);
            if self.decode_times.len() > 100 {
                self.decode_times.remove(0);
            }
            self.stats.avg_decode_time_us =
                self.decode_times.iter().sum::<f64>() / self.decode_times.len() as f64;
        } else {
            self.stats.decode_errors += 1;
        }
    }
}

impl AudioDecoder for OpusDecoder {
    fn decode(&mut self, data: &[u8]) -> Result<Vec<f32>> {
        let start = Instant::now();

        // Prepare output buffer for f32 samples
        let mut output_f32 = vec![0.0f32; self.frame_size * self.params.channels as usize];

        // Decode the frame
        let decoded_samples = match self
            .decoder
            .decode_float(Some(data), &mut output_f32, false)
        {
            Ok(samples) => samples,
            Err(e) => {
                self.update_stats(0.0, data.len(), false);
                return Err(anyhow!("Opus decode error: {}", e));
            }
        };

        // Truncate to actual decoded samples
        output_f32.truncate(decoded_samples * self.params.channels as usize);

        let decode_time = start.elapsed().as_micros() as f64;
        self.update_stats(decode_time, data.len(), true);

        Ok(output_f32)
    }

    fn conceal_packet_loss(&mut self) -> Result<Vec<f32>> {
        let start = Instant::now();

        // Prepare output buffer for f32 samples
        let mut output_f32 = vec![0.0f32; self.frame_size * self.params.channels as usize];

        // Use Opus PLC (Packet Loss Concealment) by passing None
        let decoded_samples = match self.decoder.decode_float(None, &mut output_f32, false) {
            Ok(samples) => samples,
            Err(e) => {
                self.stats.decode_errors += 1;
                return Err(anyhow!("Opus PLC error: {}", e));
            }
        };

        // Truncate to actual decoded samples
        output_f32.truncate(decoded_samples * self.params.channels as usize);

        self.stats.fec_recoveries += 1;
        let decode_time = start.elapsed().as_micros() as f64;
        self.decode_times.push(decode_time);

        Ok(output_f32)
    }

    fn reset(&mut self) -> Result<()> {
        // Opus decoder doesn't have a reset method, so recreate it
        *self = OpusDecoder::new(self.params.clone())?;
        Ok(())
    }

    fn params(&self) -> &CodecParams {
        &self.params
    }

    fn set_params(&mut self, params: CodecParams) -> Result<()> {
        // Recreate decoder with new params
        *self = OpusDecoder::new(params)?;
        Ok(())
    }

    fn stats(&self) -> &CodecStats {
        &self.stats
    }
}

/// Factory for creating Opus codec instances
pub struct OpusCodecFactory;

impl CodecFactory for OpusCodecFactory {
    fn codec_type(&self) -> CodecType {
        CodecType::Opus
    }

    fn is_available(&self) -> bool {
        // Opus is always available when the library is linked
        true
    }

    fn create_encoder(&self, params: CodecParams) -> Result<Box<dyn AudioEncoder>> {
        Ok(Box::new(OpusEncoder::new(params)?))
    }

    fn create_decoder(&self, params: CodecParams) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(OpusDecoder::new(params)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opus_encode_decode() {
        let params = CodecParams::voice();
        let factory = OpusCodecFactory;

        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params.clone()).unwrap();

        // Create a test frame (20ms at 48kHz mono = 960 samples)
        let frame_size = 960;
        let mut samples = vec![0.0f32; frame_size];

        // Generate a simple sine wave
        for i in 0..frame_size {
            samples[i] = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin() * 0.5;
        }

        // Encode
        let encoded = encoder.encode(&samples).unwrap();
        assert!(!encoded.is_empty());
        assert!(encoded.len() < samples.len() * 4); // Should be compressed

        // Decode
        let decoded = decoder.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), samples.len());

        // Check that the decoded signal is similar to the original
        // (won't be exact due to lossy compression)
        let mut error = 0.0f32;
        for i in 0..samples.len() {
            error += (samples[i] - decoded[i]).abs();
        }
        let avg_error = error / samples.len() as f32;
        assert!(avg_error < 0.1); // Average error should be small
    }

    #[test]
    fn test_packet_loss_concealment() {
        let params = CodecParams::voice();
        let factory = OpusCodecFactory;

        let mut decoder = factory.create_decoder(params).unwrap();

        // Generate PLC frame
        let plc_frame = decoder.conceal_packet_loss().unwrap();
        assert_eq!(plc_frame.len(), 960); // 20ms at 48kHz mono

        // Check stats
        assert_eq!(decoder.stats().fec_recoveries, 1);
    }

    #[test]
    fn test_dtx_detection() {
        let params = CodecParams {
            dtx: true,
            ..CodecParams::voice()
        };
        let factory = OpusCodecFactory;

        let mut encoder = factory.create_encoder(params).unwrap();

        // Encode silence
        let silence = vec![0.0f32; 960];
        let encoded = encoder.encode(&silence).unwrap();

        // DTX should produce very small packets for silence
        assert!(encoded.len() < 10);
        assert_eq!(encoder.stats().dtx_packets, 1);
    }
}
