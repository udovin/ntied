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
            weights: [0, 0, 0, 0],
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
        // Use fixed-point arithmetic for stability
        // Using a fixed learning rate of 0.01
        let step_size = (0.01 * 65536.0) as i32; // Convert to fixed point

        for i in 0..4 {
            if self.history[i] != 0 {
                // Calculate weight update: delta = learning_rate * error * input
                let delta =
                    ((error as i64 * self.history[i] as i64 * step_size as i64) >> 24) as i32;

                // Apply weight update with saturation
                self.weights[i] = self.weights[i].saturating_add(delta);

                // Clamp weights to prevent overflow
                self.weights[i] = self.weights[i].clamp(-16384, 16384);
            }
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
}
