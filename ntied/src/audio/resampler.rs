use anyhow::{Result, anyhow};

/// Simple audio resampler using linear interpolation
/// This is suitable for real-time voice communication where low latency is important
pub struct Resampler {
    input_rate: u32,
    output_rate: u32,
    channels: u16,
    /// Accumulator for fractional sample position
    position: f64,
    /// Previous sample for each channel (for interpolation)
    prev_samples: Vec<f32>,
}

impl Resampler {
    /// Create a new resampler
    pub fn new(input_rate: u32, output_rate: u32, channels: u16) -> Result<Self> {
        if input_rate == 0 || output_rate == 0 {
            return Err(anyhow!("Sample rates must be non-zero"));
        }
        if channels == 0 {
            return Err(anyhow!("Channel count must be non-zero"));
        }

        Ok(Self {
            input_rate,
            output_rate,
            channels,
            position: 0.0,
            prev_samples: vec![0.0; channels as usize],
        })
    }

    /// Get the input sample rate
    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }

    /// Get the output sample rate
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// Resample audio samples from input rate to output rate
    pub fn resample(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        if input.len() % self.channels as usize != 0 {
            return Err(anyhow!(
                "Input sample count must be a multiple of channel count"
            ));
        }

        // If rates are the same, just return a copy
        if self.input_rate == self.output_rate {
            return Ok(input.to_vec());
        }

        let ratio = self.input_rate as f64 / self.output_rate as f64;
        let input_frames = input.len() / self.channels as usize;

        // Calculate output frames more accurately, considering the fractional position
        let total_input_frames = self.position + input_frames as f64;
        let total_output_frames = (total_input_frames / ratio).floor() as usize;
        let prev_output_frames = (self.position / ratio).floor() as usize;
        let output_frames = total_output_frames.saturating_sub(prev_output_frames);

        let mut output = Vec::with_capacity(output_frames * self.channels as usize);

        for _ in 0..output_frames {
            // Calculate the position in the input buffer
            let input_pos = self.position;
            let input_index = input_pos.floor() as usize;
            let fraction = (input_pos - input_index as f64) as f32;

            // Check if we're still within the input buffer
            if input_index >= input_frames {
                break;
            }

            // Process each channel
            for ch in 0..self.channels as usize {
                let current_sample = if input_index * self.channels as usize + ch < input.len() {
                    input[input_index * self.channels as usize + ch]
                } else {
                    // Use previous sample or silence
                    self.prev_samples.get(ch).copied().unwrap_or(0.0)
                };

                let next_sample = if (input_index + 1) < input_frames
                    && (input_index + 1) * self.channels as usize + ch < input.len()
                {
                    input[(input_index + 1) * self.channels as usize + ch]
                } else {
                    // For the last sample, use the current sample or previous
                    current_sample
                };

                // Linear interpolation for smooth transitions
                let sample = current_sample * (1.0 - fraction) + next_sample * fraction;
                output.push(sample);
            }

            // Advance position
            self.position += ratio;
        }

        // Store the last samples for continuity
        if input_frames > 0 {
            let last_frame_index = (input_frames - 1) * self.channels as usize;
            for ch in 0..self.channels as usize {
                if last_frame_index + ch < input.len() {
                    self.prev_samples[ch] = input[last_frame_index + ch];
                }
            }
        }

        // Adjust position for next call - keep only the fractional part that wasn't consumed
        self.position -= input_frames as f64;

        // For upsampling, we might have a negative position, which is correct
        // It represents how far we've advanced into the "next" input frame
        if self.position < 0.0 && ratio < 1.0 {
            // For upsampling, this is expected behavior
            self.position = self.position.max(-1.0);
        } else {
            // For downsampling, clamp to 0
            self.position = self.position.max(0.0);
        }

        Ok(output)
    }

    /// Reset the resampler state
    pub fn reset(&mut self) {
        self.position = 0.0;
        self.prev_samples.fill(0.0);
    }

    /// Update sample rates (resets internal state)
    pub fn set_rates(&mut self, input_rate: u32, output_rate: u32) -> Result<()> {
        if input_rate == 0 || output_rate == 0 {
            return Err(anyhow!("Sample rates must be non-zero"));
        }
        self.input_rate = input_rate;
        self.output_rate = output_rate;
        self.reset();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resampler_creation() {
        let resampler = Resampler::new(44100, 48000, 2).unwrap();
        assert_eq!(resampler.input_rate(), 44100);
        assert_eq!(resampler.output_rate(), 48000);
    }

    #[test]
    fn test_same_rate() {
        let mut resampler = Resampler::new(48000, 48000, 1).unwrap();
        let input = vec![0.5, -0.5, 0.3, -0.3];
        let output = resampler.resample(&input).unwrap();
        assert_eq!(output, input);
    }

    #[test]
    fn test_upsample() {
        // Upsample from 8000 to 16000 (2x)
        let mut resampler = Resampler::new(8000, 16000, 1).unwrap();
        let input = vec![0.0, 1.0, 0.0, -1.0];
        let output = resampler.resample(&input).unwrap();

        // Should have approximately 2x the samples
        assert!(output.len() >= input.len() * 2 - 1);
        assert!(output.len() <= input.len() * 2 + 1);
    }

    #[test]
    fn test_downsample() {
        // Downsample from 16000 to 8000 (0.5x)
        let mut resampler = Resampler::new(16000, 8000, 1).unwrap();
        let input = vec![0.0, 0.5, 1.0, 0.5, 0.0, -0.5, -1.0, -0.5];
        let output = resampler.resample(&input).unwrap();

        // Should have approximately half the samples
        assert!(output.len() >= input.len() / 2 - 1);
        assert!(output.len() <= input.len() / 2 + 1);
    }

    #[test]
    fn test_multichannel() {
        let mut resampler = Resampler::new(44100, 48000, 2).unwrap();
        let input = vec![
            0.5, -0.5, // Frame 1: L=0.5, R=-0.5
            0.3, -0.3, // Frame 2: L=0.3, R=-0.3
        ];
        let output = resampler.resample(&input).unwrap();

        // Check that we have an even number of samples (stereo)
        assert_eq!(output.len() % 2, 0);
    }

    #[test]
    fn test_common_rates() {
        // Test common sample rate conversions
        let test_cases = vec![
            (44100, 48000), // CD to DAT
            (48000, 44100), // DAT to CD
            (48000, 16000), // Wideband to narrowband
            (16000, 48000), // Narrowband to wideband
        ];

        for (input_rate, output_rate) in test_cases {
            let mut resampler = Resampler::new(input_rate, output_rate, 1).unwrap();
            let input_frames = 100;
            let input = vec![0.5; input_frames];
            let output = resampler.resample(&input).unwrap();

            // Check that output length is approximately correct
            let expected_frames =
                (input_frames as f64 * output_rate as f64 / input_rate as f64).round() as usize;
            assert!(
                (output.len() as i32 - expected_frames as i32).abs() <= 2,
                "Rate conversion {}->{}Hz: expected {} frames, got {}",
                input_rate,
                output_rate,
                expected_frames,
                output.len()
            );
        }
    }

    #[test]
    fn test_continuous_processing() {
        // Test that processing multiple consecutive chunks maintains continuity
        let mut resampler = Resampler::new(16000, 48000, 1).unwrap();

        // Generate a sine wave split across multiple chunks
        let frequency = 440.0; // A4 note
        let input_rate = 16000.0;
        let samples_per_chunk = 320; // 20ms at 16kHz
        let num_chunks = 10;

        let mut all_output = Vec::new();

        for chunk_idx in 0..num_chunks {
            let mut chunk = Vec::with_capacity(samples_per_chunk);
            for i in 0..samples_per_chunk {
                let sample_idx = chunk_idx * samples_per_chunk + i;
                let t = sample_idx as f32 / input_rate;
                let sample = (2.0 * std::f32::consts::PI * frequency * t).sin();
                chunk.push(sample);
            }

            let resampled = resampler.resample(&chunk).unwrap();
            all_output.extend(resampled);
        }

        // Verify we got approximately the right number of output samples
        let expected_output = (samples_per_chunk * num_chunks * 3) as usize; // 3x upsampling
        // Allow for some variance due to fractional sample positions
        let tolerance = (num_chunks * 3) as i32; // ~1% tolerance
        assert!(
            (all_output.len() as i32 - expected_output as i32).abs() <= tolerance,
            "Expected ~{} samples, got {} (tolerance: {})",
            expected_output,
            all_output.len(),
            tolerance
        );
    }

    #[test]
    fn test_position_tracking() {
        // Test that fractional position is correctly maintained
        let mut resampler = Resampler::new(11025, 48000, 1).unwrap();

        // Process several small chunks
        for _ in 0..20 {
            let chunk = vec![0.5; 100];
            let output = resampler.resample(&chunk).unwrap();

            // Output should be consistent for each chunk
            assert!(output.len() > 0);
        }
    }

    #[test]
    fn test_extreme_downsampling() {
        // Test extreme downsampling (48kHz to 8kHz)
        let mut resampler = Resampler::new(48000, 8000, 1).unwrap();

        let input = vec![0.5; 960]; // 20ms at 48kHz
        let output = resampler.resample(&input).unwrap();

        // Should get approximately 1/6 of the input samples
        let expected = 160;
        assert!(
            (output.len() as i32 - expected).abs() <= 2,
            "Expected ~{} samples, got {}",
            expected,
            output.len()
        );
    }

    #[test]
    fn test_stereo_continuity() {
        // Test that stereo channels maintain proper alignment
        let mut resampler = Resampler::new(24000, 48000, 2).unwrap();

        // Create interleaved stereo samples
        let mut input = Vec::new();
        for i in 0..240 {
            // Left channel: rising
            input.push(i as f32 / 240.0);
            // Right channel: falling
            input.push(1.0 - (i as f32 / 240.0));
        }

        let output = resampler.resample(&input).unwrap();

        // Check stereo alignment
        assert_eq!(
            output.len() % 2,
            0,
            "Output must have even number of samples for stereo"
        );

        // Verify channel separation is maintained
        for i in (0..output.len()).step_by(2) {
            let left = output[i];
            let right = output[i + 1];

            // In our test pattern, left + right should be close to 1.0
            assert!(
                (left + right - 1.0).abs() < 0.1,
                "Channel separation lost at sample {}: L={}, R={}",
                i,
                left,
                right
            );
        }
    }

    #[test]
    fn test_reset_state() {
        let mut resampler = Resampler::new(16000, 48000, 1).unwrap();

        // Process some samples
        let input1 = vec![0.5; 160];
        let output1 = resampler.resample(&input1).unwrap();

        // Reset and process again
        resampler.reset();
        let output2 = resampler.resample(&input1).unwrap();

        // After reset, output should be identical
        assert_eq!(
            output1.len(),
            output2.len(),
            "Output lengths differ after reset"
        );
    }

    #[test]
    fn test_empty_input() {
        let mut resampler = Resampler::new(44100, 48000, 1).unwrap();
        let result = resampler.resample(&[]).unwrap();
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_single_sample() {
        let mut resampler = Resampler::new(44100, 48000, 1).unwrap();
        let result = resampler.resample(&[0.5]).unwrap();
        assert!(
            result.len() <= 2,
            "Single sample should produce at most 2 samples"
        );
    }
}
