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
        let output_frames = ((input_frames as f64) / ratio).ceil() as usize;
        let mut output = Vec::with_capacity(output_frames * self.channels as usize);

        for _ in 0..output_frames {
            // Calculate the position in the input buffer
            let input_pos = self.position;
            let input_index = input_pos.floor() as usize;
            let fraction = input_pos - input_index as f64;

            // Process each channel
            for ch in 0..self.channels as usize {
                let sample = if input_index < input_frames {
                    // Current sample position
                    let current_sample = if input_index * self.channels as usize + ch < input.len()
                    {
                        input[input_index * self.channels as usize + ch]
                    } else {
                        0.0
                    };

                    // Next sample for interpolation
                    let next_sample = if (input_index + 1) < input_frames
                        && (input_index + 1) * self.channels as usize + ch < input.len()
                    {
                        input[(input_index + 1) * self.channels as usize + ch]
                    } else {
                        current_sample // Use current sample if next is not available
                    };

                    // Linear interpolation
                    current_sample * (1.0 - fraction as f32) + next_sample * (fraction as f32)
                } else {
                    // We've run out of input samples, use silence
                    0.0
                };

                output.push(sample);

                // Store as previous sample for next iteration
                if input_index < input_frames
                    && input_index * self.channels as usize + ch < input.len()
                {
                    self.prev_samples[ch] = input[input_index * self.channels as usize + ch];
                }
            }

            // Advance position
            self.position += ratio;
        }

        // Adjust position for next call
        self.position -= input_frames as f64;
        if self.position < 0.0 {
            self.position = 0.0;
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
}
