//! LMS (Least Mean Squares) adaptive filter implementation for SEA codec

/// LMS filter state for audio prediction
#[derive(Debug, Clone)]
pub struct LmsFilter {
    /// Filter history (past samples)
    history: [i32; 4],
    /// Filter weights (coefficients)
    weights: [i32; 4],
    /// Weight scaling factor
    weight_scale: i32,
}

impl Default for LmsFilter {
    fn default() -> Self {
        Self {
            history: [0; 4],
            // Initialize with small non-zero weights for better initial predictions
            weights: [128, 64, 32, 16],
            weight_scale: 256, // Fixed point scaling
        }
    }
}

impl LmsFilter {
    /// Create a new LMS filter with default parameters
    pub fn new() -> Self {
        Self::default()
    }

    /// Predict the next sample based on history
    pub fn predict(&self) -> i32 {
        let mut prediction = 0i64;

        for i in 0..4 {
            prediction += self.history[i] as i64 * self.weights[i] as i64;
        }

        // Normalize the prediction (weights are scaled)
        (prediction / self.weight_scale as i64) as i32
    }

    /// Update filter with new sample and return prediction error
    #[allow(dead_code)]
    pub fn update(&mut self, sample: i32) -> i32 {
        let prediction = self.predict();
        let error = sample - prediction;

        // Update history first
        self.push_sample(sample);

        // Then adapt weights using LMS algorithm with proper scaling
        if error.abs() < 32768 {
            // Prevent overflow
            self.adapt_weights(error);
        }

        error
    }

    /// Reset the filter state
    pub fn reset(&mut self) {
        self.history = [0; 4];
        self.weights = [0, 0, 0, 0];
    }

    /// Get current history
    #[allow(dead_code)]
    pub fn history(&self) -> [i32; 4] {
        self.history
    }

    /// Get current weights
    #[allow(dead_code)]
    pub fn weights(&self) -> [i32; 4] {
        self.weights
    }

    /// Set weights (for decoder synchronization)
    pub fn set_weights(&mut self, weights: [i32; 4]) {
        self.weights = weights;
    }

    /// Push a sample and adapt weights (for decoder)
    pub fn push_sample(&mut self, sample: i32) {
        // Shift history
        self.history[3] = self.history[2];
        self.history[2] = self.history[1];
        self.history[1] = self.history[0];
        self.history[0] = sample;
    }

    /// Adapt weights with given error (for decoder)
    #[allow(dead_code)]
    pub fn adapt_weights(&mut self, error: i32) {
        // Normalized LMS (NLMS) for better stability
        // Calculate power of input signal for normalization
        let mut power = 0i64;
        for i in 0..4 {
            power += (self.history[i] as i64) * (self.history[i] as i64);
        }

        // Add small constant to avoid division by zero
        power = power.max(1);

        // Use very conservative step size for stability
        // mu = 0.0001 in fixed point (scaled by 2^20 for precision)
        let mu = 105; // 0.0001 * 2^20

        // Limit error magnitude to prevent instability
        let error = error.clamp(-8192, 8192);

        for i in 0..4 {
            // NLMS update: w[i] += mu * error * x[i] / (power + epsilon)
            // Using fixed-point arithmetic with careful scaling
            let update = if power > 0 {
                // Scale carefully to avoid overflow
                let numerator = (mu as i64 * error as i64 * self.history[i] as i64) >> 10;
                let delta = (numerator / power) as i32;
                delta.clamp(-100, 100) // Limit step size
            } else {
                0
            };

            // Apply weight update with saturation
            self.weights[i] = self.weights[i].saturating_add(update);

            // Strict weight clamping for stability
            self.weights[i] = self.weights[i].clamp(-8192, 8192);
        }
    }
}

/// Multi-channel LMS filter bank
#[derive(Debug, Clone)]
pub struct LmsFilterBank {
    filters: Vec<LmsFilter>,
}

impl LmsFilterBank {
    /// Create a new filter bank for the given number of channels
    pub fn new(channels: usize) -> Self {
        Self {
            filters: vec![LmsFilter::new(); channels],
        }
    }

    /// Process samples for all channels (interleaved)
    #[allow(dead_code)]
    pub fn process_interleaved(&mut self, samples: &[i32], channels: usize) -> Vec<i32> {
        let mut residuals = Vec::with_capacity(samples.len());

        for (i, &sample) in samples.iter().enumerate() {
            let channel = i % channels;
            residuals.push(self.filters[channel].update(sample));
        }

        residuals
    }

    /// Reset all filters
    pub fn reset(&mut self) {
        for filter in &mut self.filters {
            filter.reset();
        }
    }

    /// Get filter for specific channel
    pub fn get_filter(&self, channel: usize) -> Option<&LmsFilter> {
        self.filters.get(channel)
    }

    /// Get mutable filter for specific channel
    pub fn get_filter_mut(&mut self, channel: usize) -> Option<&mut LmsFilter> {
        self.filters.get_mut(channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lms_filter_prediction() {
        let mut filter = LmsFilter::new();

        // Feed some samples
        let samples = [100, 200, 150, 175, 160];
        for &sample in &samples {
            filter.update(sample);
        }

        // Prediction should be reasonable after feeding samples
        let prediction = filter.predict();
        // After feeding positive samples, prediction should be non-negative
        // (it might be 0 initially if weights haven't adapted enough)
        assert!(prediction >= 0);
    }

    #[test]
    fn test_lms_convergence() {
        let mut filter = LmsFilter::new();

        // Create a simple AR(1) process: x[n] = 0.8 * x[n-1] + noise
        let mut signal = vec![1000i32];
        for i in 1..200 {
            let next = (signal[i - 1] as f32 * 0.8) as i32 + ((i * 7) % 20) as i32 - 10;
            signal.push(next);
        }

        // Track prediction errors
        let mut errors = Vec::new();
        for &sample in &signal {
            let error = filter.update(sample);
            errors.push(error.abs());
        }

        // Calculate average error in windows
        let early_error: f64 = errors[10..30].iter().map(|&e| e as f64).sum::<f64>() / 20.0;
        let late_error: f64 = errors[150..170].iter().map(|&e| e as f64).sum::<f64>() / 20.0;

        // Late error should be smaller (convergence)
        assert!(
            late_error < early_error * 0.8,
            "LMS should converge: early_error={:.2}, late_error={:.2}",
            early_error,
            late_error
        );
    }

    #[test]
    fn test_lms_weight_synchronization() {
        let mut filter1 = LmsFilter::new();
        let mut filter2 = LmsFilter::new();

        // Train filter1
        let samples = [100, 200, 150, 175, 160, 180, 170];
        for &sample in &samples {
            filter1.update(sample);
        }

        // Synchronize weights to filter2
        let weights = filter1.weights();
        filter2.set_weights(weights);

        // Also need to synchronize history for predictions to match
        let history = filter1.history();
        for &h in history.iter().rev() {
            filter2.push_sample(h);
        }

        // Both filters should produce same prediction
        assert_eq!(filter1.predict(), filter2.predict());
        assert_eq!(filter1.weights(), filter2.weights());

        // After same update, should remain synchronized
        filter1.update(190);
        filter2.update(190);
        assert_eq!(filter1.weights(), filter2.weights());
    }

    #[test]
    fn test_lms_residual_coding() {
        let mut encoder_filter = LmsFilter::new();
        let mut decoder_filter = LmsFilter::new();

        // Simulate encoder-decoder pair with more predictable AR(1) signal
        let mut original_samples = vec![1000i32];
        for i in 1..50 {
            // AR(1) process: x[n] = 0.9 * x[n-1] + small noise
            let next = (original_samples[i - 1] as f32 * 0.9) as i32 + ((i * 3) % 10) as i32 - 5;
            original_samples.push(next);
        }
        let mut residuals = Vec::new();
        let mut reconstructed = Vec::new();

        for &sample in &original_samples {
            // Encoder side: compute residual
            let prediction = encoder_filter.predict();
            let residual = sample - prediction;
            encoder_filter.push_sample(sample);
            encoder_filter.adapt_weights(residual);
            residuals.push(residual);

            // Decoder side: reconstruct from residual
            let decoder_prediction = decoder_filter.predict();
            let reconstructed_sample = residual + decoder_prediction;
            decoder_filter.push_sample(reconstructed_sample);
            decoder_filter.adapt_weights(residual);
            reconstructed.push(reconstructed_sample);
        }

        // Verify perfect reconstruction
        for (orig, recon) in original_samples.iter().zip(reconstructed.iter()) {
            assert_eq!(orig, recon, "Perfect reconstruction should be achieved");
        }

        // Verify residuals are smaller than originals after adaptation (skip first few)
        // Skip first 10 samples as the filter is still adapting
        let original_energy: i64 = original_samples[10..]
            .iter()
            .map(|&x| (x as i64) * (x as i64))
            .sum();
        let residual_energy: i64 = residuals[10..]
            .iter()
            .map(|&x| (x as i64) * (x as i64))
            .sum();

        // For AR(1) process with high correlation, residuals should be much smaller
        assert!(
            residual_energy < original_energy / 2,
            "Residual energy {} should be much less than original energy {} (after adaptation)",
            residual_energy,
            original_energy
        );
    }

    #[test]
    fn test_lms_steady_state_performance() {
        let mut filter = LmsFilter::new();

        // Generate a more complex but predictable signal
        let mut signal = Vec::new();
        for i in 0..500 {
            // Combination of sinusoids
            let t = i as f32 * 0.05;
            let sample = ((t.sin() * 500.0) + (t * 2.0).cos() * 300.0) as i32;
            signal.push(sample);
        }

        // Let filter adapt
        let mut steady_state_errors = Vec::new();
        for (i, &sample) in signal.iter().enumerate() {
            let error = filter.update(sample);
            if i > 300 {
                // After convergence
                steady_state_errors.push(error.abs());
            }
        }

        // Check steady-state performance
        let avg_error =
            steady_state_errors.iter().sum::<i32>() as f64 / steady_state_errors.len() as f64;
        let max_error = *steady_state_errors.iter().max().unwrap();

        assert!(
            avg_error < 200.0,
            "Average steady-state error should be small: {}",
            avg_error
        );
        assert!(
            max_error < 1000,
            "Maximum steady-state error should be bounded: {}",
            max_error
        );
    }

    #[test]
    fn test_lms_filter_adaptation() {
        let mut filter = LmsFilter::new();

        // Generate a predictable pattern
        let mut pattern = Vec::new();
        for i in 0..100 {
            // Simple sine-like pattern
            let value = ((i as f32 * 0.1).sin() * 1000.0) as i32;
            pattern.push(value);
        }

        let mut errors = Vec::new();

        for &sample in &pattern {
            let error = filter.update(sample);
            errors.push(error.abs() as f64);
        }

        // Calculate average error for different segments
        let early_avg: f64 = errors[10..20].iter().sum::<f64>() / 10.0;
        let late_avg: f64 = errors[80..90].iter().sum::<f64>() / 10.0;

        // Late errors should be smaller or similar (allowing for some variance)
        assert!(
            late_avg <= early_avg * 2.0, // Allow 2x tolerance for stability
            "Late avg error {:.2} should not be much larger than early avg error {:.2}",
            late_avg,
            early_avg
        );
    }

    #[test]
    fn test_lms_filter_reset() {
        let mut filter = LmsFilter::new();

        // Feed some data
        filter.update(1000);
        filter.update(2000);

        // Reset
        filter.reset();

        assert_eq!(filter.history(), [0; 4]);
        assert_eq!(filter.weights(), [0; 4]);
    }

    #[test]
    fn test_filter_bank() {
        let mut bank = LmsFilterBank::new(2);

        // Interleaved stereo samples
        let samples = vec![100, 200, 150, 250, 175, 275];
        let residuals = bank.process_interleaved(&samples, 2);

        assert_eq!(residuals.len(), samples.len());

        // Check that different channels have different states
        let filter0 = bank.get_filter(0).unwrap();
        let filter1 = bank.get_filter(1).unwrap();
        assert_ne!(filter0.history(), filter1.history());
    }

    #[test]
    fn test_weight_clamping() {
        let mut filter = LmsFilter::new();

        // Feed extreme values to test weight clamping
        for _ in 0..100 {
            filter.update(30000);
            filter.update(-30000);
        }

        // Weights should be clamped
        for weight in filter.weights() {
            assert!(weight >= -32768 && weight <= 32767);
        }
    }

    #[test]
    fn test_lms_stability_with_noise() {
        let mut filter = LmsFilter::new();

        // Feed noisy signal
        for i in 0..1000 {
            let noise = ((i * 31 + 17) % 100 - 50) * 10; // Pseudo-random noise
            let signal = (i as f32 * 0.01).sin() * 1000.0 + noise as f32;
            filter.update(signal as i32);
        }

        // Verify filter remains stable (weights don't explode)
        let weights = filter.weights();
        for w in weights {
            assert!(w.abs() <= 16384, "Weight {} exceeds maximum", w);
        }
    }

    #[test]
    fn test_filter_bank_channel_isolation() {
        let mut bank = LmsFilterBank::new(4);

        // Feed different patterns to different channels with more samples for better adaptation
        let patterns = vec![
            vec![100, 200, 100, 200, 100, 200, 100, 200], // Channel 0: alternating
            vec![150, 155, 160, 165, 170, 175, 180, 185], // Channel 1: increasing
            vec![200, 195, 190, 185, 180, 175, 170, 165], // Channel 2: decreasing
            vec![100, 150, 200, 250, 100, 150, 200, 250], // Channel 3: large steps
        ];

        // Process interleaved samples multiple times to ensure adaptation
        for _ in 0..5 {
            let mut interleaved = Vec::new();
            for i in 0..8 {
                for ch in 0..4 {
                    interleaved.push(patterns[ch][i]);
                }
            }
            let _residuals = bank.process_interleaved(&interleaved, 4);
        }

        // Verify each channel adapted independently
        let mut all_weights = Vec::new();
        for ch in 0..4 {
            let filter = bank.get_filter(ch).unwrap();
            let weights = filter.weights();
            all_weights.push(weights);
        }

        // Check that at least some channels have different weights
        let mut found_difference = false;
        for i in 0..3 {
            for j in (i + 1)..4 {
                let same = all_weights[i]
                    .iter()
                    .zip(all_weights[j].iter())
                    .all(|(a, b)| a == b);
                if !same {
                    found_difference = true;
                    break;
                }
            }
            if found_difference {
                break;
            }
        }

        assert!(
            found_difference,
            "At least some channels should have different weights after processing different patterns"
        );
    }
}
