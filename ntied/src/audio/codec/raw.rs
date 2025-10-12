use std::time::Instant;

use anyhow::Result;

use super::traits::{AudioDecoder, AudioEncoder, CodecFactory, CodecParams, CodecStats, CodecType};

/// Raw PCM encoder (no compression)
pub struct RawEncoder {
    params: CodecParams,
    stats: CodecStats,
    encode_times: Vec<f64>,
}

impl RawEncoder {
    pub fn new(params: CodecParams) -> Result<Self> {
        Ok(Self {
            params,
            stats: CodecStats::default(),
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
        // For raw PCM: sample_rate * channels * 32 bits per f32
        self.stats.current_bitrate = self.params.sample_rate * self.params.channels as u32 * 32;
    }
}

impl AudioEncoder for RawEncoder {
    fn encode(&mut self, samples: &[f32]) -> Result<Vec<u8>> {
        let start = Instant::now();

        // Convert f32 samples to bytes (little-endian)
        let mut output = Vec::with_capacity(samples.len() * 4);
        for sample in samples {
            output.extend_from_slice(&sample.to_le_bytes());
        }

        let encode_time = start.elapsed().as_micros() as f64;
        self.update_stats(encode_time, output.len());

        Ok(output)
    }

    fn reset(&mut self) -> Result<()> {
        // No state to reset
        Ok(())
    }

    fn params(&self) -> &CodecParams {
        &self.params
    }

    fn set_params(&mut self, params: CodecParams) -> Result<()> {
        self.params = params;
        Ok(())
    }

    fn set_bitrate(&mut self, _bitrate: u32) -> Result<()> {
        // Raw codec doesn't support variable bitrate
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

/// Raw PCM decoder (no compression)
pub struct RawDecoder {
    params: CodecParams,
    stats: CodecStats,
    decode_times: Vec<f64>,
    last_frame: Option<Vec<f32>>,
}

impl RawDecoder {
    pub fn new(params: CodecParams) -> Result<Self> {
        Ok(Self {
            params,
            stats: CodecStats::default(),
            decode_times: Vec::with_capacity(100),
            last_frame: None,
        })
    }

    fn update_stats(&mut self, decode_time: f64, input_size: usize) {
        self.stats.frames_decoded += 1;
        self.stats.bytes_decoded += input_size as u64;

        // Update average decode time
        self.decode_times.push(decode_time);
        if self.decode_times.len() > 100 {
            self.decode_times.remove(0);
        }
        self.stats.avg_decode_time_us =
            self.decode_times.iter().sum::<f64>() / self.decode_times.len() as f64;
    }
}

impl AudioDecoder for RawDecoder {
    fn decode(&mut self, data: &[u8]) -> Result<Vec<f32>> {
        let start = Instant::now();

        // Convert bytes back to f32 samples
        let mut samples = Vec::with_capacity(data.len() / 4);
        for chunk in data.chunks_exact(4) {
            let bytes: [u8; 4] = chunk.try_into().unwrap();
            samples.push(f32::from_le_bytes(bytes));
        }

        // Store for potential PLC
        self.last_frame = Some(samples.clone());

        let decode_time = start.elapsed().as_micros() as f64;
        self.update_stats(decode_time, data.len());

        Ok(samples)
    }

    fn conceal_packet_loss(&mut self) -> Result<Vec<f32>> {
        // Simple PLC: repeat last frame with fade
        if let Some(ref last) = self.last_frame {
            let mut concealed = last.clone();
            // Apply fade to reduce artifacts
            let len = concealed.len();
            for (i, sample) in concealed.iter_mut().enumerate() {
                let fade = 1.0 - (i as f32 / len as f32) * 0.3;
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
        self.last_frame = None;
        Ok(())
    }

    fn params(&self) -> &CodecParams {
        &self.params
    }

    fn set_params(&mut self, params: CodecParams) -> Result<()> {
        self.params = params;
        Ok(())
    }

    fn stats(&self) -> &CodecStats {
        &self.stats
    }
}

/// Factory for creating Raw codec instances
pub struct RawCodecFactory;

impl CodecFactory for RawCodecFactory {
    fn codec_type(&self) -> CodecType {
        CodecType::Raw
    }

    fn is_available(&self) -> bool {
        // Raw codec is always available
        true
    }

    fn create_encoder(&self, params: CodecParams) -> Result<Box<dyn AudioEncoder>> {
        Ok(Box::new(RawEncoder::new(params)?))
    }

    fn create_decoder(&self, params: CodecParams) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(RawDecoder::new(params)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raw_encode_decode() {
        let params = CodecParams::voice();
        let factory = RawCodecFactory;

        let mut encoder = factory.create_encoder(params.clone()).unwrap();
        let mut decoder = factory.create_decoder(params.clone()).unwrap();

        // Create test samples
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
    fn test_raw_plc() {
        let params = CodecParams::voice();
        let factory = RawCodecFactory;

        let mut decoder = factory.create_decoder(params).unwrap();

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
