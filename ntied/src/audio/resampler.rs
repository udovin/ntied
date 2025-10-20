use anyhow::{Result, anyhow};

/// Simple audio resampler using linear interpolation
/// This is suitable for real-time voice communication where low latency is important
pub struct Resampler {
    input_rate: u32,
    output_rate: u32,
    channels: u16,
    /// Accumulator for fractional sample position (high precision)
    position: f64,
    /// Previous sample for each channel (for interpolation)
    prev_samples: Vec<f32>,
    /// Total samples processed (for drift detection)
    total_input_samples: u64,
    total_output_samples: u64,
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
            total_input_samples: 0,
            total_output_samples: 0,
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

        let input_frames = input.len() / self.channels as usize;

        if input_frames == 0 {
            return Ok(Vec::new());
        }

        // Step size in input samples per output sample (use high precision)
        // For upsampling (48kHz output, 16kHz input): step = 16000/48000 = 0.333333...
        // For downsampling (16kHz output, 48kHz input): step = 48000/16000 = 3.0
        let step = self.input_rate as f64 / self.output_rate as f64;

        // Periodically recalibrate position to prevent long-term drift
        if self.total_input_samples > 0 && self.total_input_samples % 10000 == 0 {
            let expected_ratio = self.total_input_samples as f64 / self.total_output_samples as f64;
            if (expected_ratio - step).abs() > 0.001 {
                tracing::trace!(
                    "Resampler drift correction: expected ratio {:.6}, actual {:.6}",
                    step,
                    expected_ratio
                );
                // Soft correction to avoid clicks
                self.position *= step / expected_ratio;
            }
        }

        // Calculate how many output frames we can generate
        let available_input = self.position + input_frames as f64;
        let output_frames = ((available_input / step).floor() as usize).min(
            // Limit output to reasonable size to prevent runaway
            ((input_frames as f64 / step) * 1.5) as usize,
        );

        let mut output = Vec::with_capacity(output_frames * self.channels as usize);

        // Track samples for drift detection
        self.total_input_samples += input_frames as u64;

        for _ in 0..output_frames {
            let input_index = self.position.floor() as usize;

            // Check bounds
            if input_index >= input_frames {
                // We've consumed all input
                break;
            }

            let fraction = (self.position - input_index as f64) as f32;

            // Process each channel
            for ch in 0..self.channels as usize {
                // Get current sample
                let current_sample = if input_index * self.channels as usize + ch < input.len() {
                    input[input_index * self.channels as usize + ch]
                } else {
                    0.0
                };

                // Get next sample for interpolation
                let next_sample = if input_index + 1 < input_frames {
                    let next_idx = (input_index + 1) * self.channels as usize + ch;
                    if next_idx < input.len() {
                        input[next_idx]
                    } else {
                        current_sample
                    }
                } else {
                    // Use last available or previous frame sample
                    if self.prev_samples.len() > ch {
                        // Blend with previous frame's last sample for continuity
                        self.prev_samples[ch]
                    } else {
                        current_sample
                    }
                };

                // Linear interpolation
                let interpolated = current_sample * (1.0 - fraction) + next_sample * fraction;
                output.push(interpolated);
            }

            // Advance position by step size (use exact value to prevent accumulation errors)
            self.position += step;
            self.total_output_samples += 1;
        }

        // Save last frame samples for continuity
        if input_frames > 0 {
            for ch in 0..self.channels as usize {
                let last_idx = (input_frames - 1) * self.channels as usize + ch;
                if last_idx < input.len() {
                    if ch < self.prev_samples.len() {
                        self.prev_samples[ch] = input[last_idx];
                    }
                }
            }
        }

        // Adjust position for next call
        self.position -= input_frames as f64;

        // Position should be small and positive for continuous processing
        // representing how far into the "next" input frame we are
        if self.position < -0.0001 {
            // Small negative values are rounding errors, reset to 0
            self.position = 0.0;
        } else if self.position < 0.0 {
            // Very small negative, likely floating point error
            self.position = 0.0;
        } else if self.position > step * 2.0 {
            // Position is too large, might indicate a problem
            tracing::debug!(
                "Large position after resampling: {} (step: {}, input_rate: {}, output_rate: {})",
                self.position,
                step,
                self.input_rate,
                self.output_rate
            );
            // Clamp position to prevent drift
            self.position = self.position.min(step);
        }

        Ok(output)
    }

    /// Reset the resampler state
    pub fn reset(&mut self) {
        self.position = 0.0;
        self.prev_samples.fill(0.0);
        self.total_input_samples = 0;
        self.total_output_samples = 0;
    }

    /// Get diagnostic information about the resampler state
    pub fn get_diagnostics(&self) -> ResamplerDiagnostics {
        ResamplerDiagnostics {
            input_rate: self.input_rate,
            output_rate: self.output_rate,
            channels: self.channels,
            position: self.position,
            step_size: self.input_rate as f64 / self.output_rate as f64,
            is_upsampling: self.input_rate < self.output_rate,
            total_input_samples: self.total_input_samples,
            total_output_samples: self.total_output_samples,
        }
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

/// Diagnostic information for debugging resampler issues
#[derive(Debug, Clone)]
pub struct ResamplerDiagnostics {
    pub input_rate: u32,
    pub output_rate: u32,
    pub channels: u16,
    pub position: f64,
    pub step_size: f64,
    pub is_upsampling: bool,
    pub total_input_samples: u64,
    pub total_output_samples: u64,
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

    #[test]
    fn test_pitch_preservation_upsampling() {
        // Test that long upsampling sessions don't cause pitch drift
        // This simulates a real call scenario with 16kHz input to 48kHz output
        let mut resampler = Resampler::new(16000, 48000, 1).unwrap();

        // Generate 10 seconds of 440Hz sine wave
        let frequency = 440.0;
        let input_rate = 16000.0;
        let output_rate = 48000.0;
        let chunk_duration_ms = 20;
        let samples_per_chunk = (input_rate * chunk_duration_ms as f32 / 1000.0) as usize;
        let total_chunks = 500; // 10 seconds worth

        let mut all_output = Vec::new();
        let mut total_input_samples = 0;

        for chunk_idx in 0..total_chunks {
            let mut chunk = Vec::with_capacity(samples_per_chunk);

            // Generate sine wave chunk
            for i in 0..samples_per_chunk {
                let sample_idx = chunk_idx * samples_per_chunk + i;
                let t = sample_idx as f32 / input_rate;
                let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.5;
                chunk.push(sample);
            }

            total_input_samples += chunk.len();
            let output = resampler.resample(&chunk).unwrap();
            all_output.extend(output);

            // Check resampler state periodically
            if chunk_idx % 100 == 0 {
                let diag = resampler.get_diagnostics();
                assert!(
                    diag.position.abs() < 10.0,
                    "Position drift at chunk {}: {}",
                    chunk_idx,
                    diag.position
                );
            }
        }

        // Verify output length is approximately correct
        let expected_output = (total_input_samples as f32 * output_rate / input_rate) as usize;
        let length_error =
            (all_output.len() as f32 - expected_output as f32).abs() / expected_output as f32;

        assert!(
            length_error < 0.01, // Less than 1% error
            "Output length error too large: {:.2}% (expected {}, got {})",
            length_error * 100.0,
            expected_output,
            all_output.len()
        );

        // Verify the frequency is preserved by checking zero crossings
        let mut zero_crossings = 0;
        for i in 1..all_output.len() {
            if all_output[i - 1] <= 0.0 && all_output[i] > 0.0 {
                zero_crossings += 1;
            }
        }

        // Expected zero crossings for 440Hz over 10 seconds
        let expected_crossings = (frequency * 10.0) as usize;
        let crossing_error =
            (zero_crossings as f32 - expected_crossings as f32).abs() / expected_crossings as f32;

        assert!(
            crossing_error < 0.05, // Less than 5% error in frequency
            "Frequency drift detected: expected {} zero crossings, got {} (error: {:.2}%)",
            expected_crossings,
            zero_crossings,
            crossing_error * 100.0
        );
    }

    #[test]
    fn test_pitch_preservation_downsampling() {
        // Test downsampling from 48kHz to 16kHz
        let mut resampler = Resampler::new(48000, 16000, 1).unwrap();

        let frequency = 440.0;
        let input_rate = 48000.0;
        let chunk_duration_ms = 20;
        let samples_per_chunk = (input_rate * chunk_duration_ms as f32 / 1000.0) as usize;
        let total_chunks = 100; // 2 seconds

        let mut total_output_samples = 0;

        for chunk_idx in 0..total_chunks {
            let mut chunk = Vec::with_capacity(samples_per_chunk);

            for i in 0..samples_per_chunk {
                let sample_idx = chunk_idx * samples_per_chunk + i;
                let t = sample_idx as f32 / input_rate;
                let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.5;
                chunk.push(sample);
            }

            let output = resampler.resample(&chunk).unwrap();
            total_output_samples += output.len();
        }

        // Check total output is approximately correct
        let expected_output = (total_chunks * samples_per_chunk) as f32 * 16000.0 / 48000.0;
        let error = (total_output_samples as f32 - expected_output).abs() / expected_output;

        assert!(
            error < 0.01,
            "Downsampling length error: {:.2}%",
            error * 100.0
        );
    }
}
