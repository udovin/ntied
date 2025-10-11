use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::AudioFrame;

/// Buffer for handling network jitter and packet reordering in audio streams.
///
/// The JitterBuffer maintains a sorted collection of audio frames and ensures
/// they are played back in the correct order, even if they arrive out of sequence.
/// It also handles missing packets by either waiting for them (up to a timeout)
/// or skipping them if they don't arrive in time.
pub struct JitterBuffer {
    /// Buffered frames sorted by sequence number
    buffer: BTreeMap<u32, BufferedFrame>,
    /// Next expected sequence number
    next_sequence: u32,
    /// Maximum time to wait for a missing packet before skipping it
    max_delay: Duration,
    /// Target buffer depth in milliseconds
    target_buffer_ms: u32,
    /// Statistics
    stats: JitterBufferStats,
}

struct BufferedFrame {
    frame: AudioFrame,
    received_at: Instant,
}

#[derive(Debug, Clone, Default)]
pub struct JitterBufferStats {
    pub packets_received: u64,
    pub packets_lost: u64,
    pub packets_late: u64,
    pub packets_duplicate: u64,
    pub packets_out_of_order: u64,
    pub current_delay_ms: f32,
    pub average_jitter_ms: f32,
}

impl JitterBuffer {
    /// Create a new JitterBuffer with default settings
    pub fn new() -> Self {
        Self::with_config(50, 200) // 50ms target, 200ms max delay
    }

    /// Create a new JitterBuffer with custom configuration
    ///
    /// # Arguments
    /// * `target_buffer_ms` - Target buffer depth in milliseconds (typically 20-100ms)
    /// * `max_delay_ms` - Maximum delay before dropping packets (typically 100-500ms)
    pub fn with_config(target_buffer_ms: u32, max_delay_ms: u32) -> Self {
        Self {
            buffer: BTreeMap::new(),
            next_sequence: 0,
            max_delay: Duration::from_millis(max_delay_ms as u64),
            target_buffer_ms,
            stats: JitterBufferStats::default(),
        }
    }

    /// Push a frame into the buffer
    ///
    /// Returns true if this is a new frame, false if it's a duplicate
    pub fn push(&mut self, sequence: u32, frame: AudioFrame) -> bool {
        self.stats.packets_received += 1;

        // Check for duplicate
        if self.buffer.contains_key(&sequence) {
            self.stats.packets_duplicate += 1;
            return false;
        }

        // Check if packet is too old (late)
        if sequence < self.next_sequence {
            self.stats.packets_late += 1;
            // Still insert it if it's not too old (might be useful for statistics)
            if self.next_sequence - sequence > 100 {
                return false; // Too old, discard
            }
        }

        // Check for out-of-order delivery
        if sequence > self.next_sequence {
            self.stats.packets_out_of_order += 1;
        }

        let buffered = BufferedFrame {
            frame,
            received_at: Instant::now(),
        };

        self.buffer.insert(sequence, buffered);
        self.update_stats();

        true
    }

    /// Pop the next frame in sequence from the buffer
    ///
    /// Returns None if the next frame is not available or if we should wait
    pub fn pop(&mut self) -> Option<AudioFrame> {
        let now = Instant::now();

        // First, check if we have the next expected packet
        if let Some(buffered) = self.buffer.remove(&self.next_sequence) {
            self.next_sequence = self.next_sequence.wrapping_add(1);
            return Some(buffered.frame);
        }

        // If not, check if we should wait or skip

        // Find the oldest packet in buffer
        if let Some((&seq, buffered)) = self.buffer.iter().next() {
            // Check if we've waited too long for the missing packets
            let wait_time = now.duration_since(buffered.received_at);

            if wait_time > self.max_delay || self.should_skip_to(seq) {
                // Skip missing packets and jump to this sequence
                let skipped = seq - self.next_sequence;
                self.stats.packets_lost += skipped as u64;

                self.next_sequence = seq;
                return self.pop(); // Recursive call to get the frame
            }
        }

        // Buffer is empty or we should keep waiting
        None
    }

    /// Check if buffer has enough frames to start playback
    pub fn is_ready(&self) -> bool {
        // Calculate buffer depth in terms of time
        let buffer_depth = self.estimate_buffer_depth_ms();
        buffer_depth >= self.target_buffer_ms as f32
    }

    /// Reset the buffer and sequence counter
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.next_sequence = 0;
        self.stats = JitterBufferStats::default();
    }

    /// Set the next expected sequence number (useful for call start)
    pub fn set_sequence(&mut self, sequence: u32) {
        self.next_sequence = sequence;
        self.buffer.clear();
    }

    /// Get current buffer statistics
    pub fn stats(&self) -> &JitterBufferStats {
        &self.stats
    }

    /// Get the number of frames currently buffered
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Estimate current buffer depth in milliseconds
    fn estimate_buffer_depth_ms(&self) -> f32 {
        if self.buffer.is_empty() {
            return 0.0;
        }

        // Assuming 20ms frames (typical for voice)
        self.buffer.len() as f32 * 20.0
    }

    /// Decide if we should skip to a given sequence number
    fn should_skip_to(&self, target_seq: u32) -> bool {
        // Skip if we're missing too many packets
        if target_seq <= self.next_sequence {
            return false; // Target is not ahead of us
        }

        let missing = target_seq - self.next_sequence;

        // Skip if missing more than 5 packets (100ms worth)
        missing > 5
    }

    /// Update internal statistics
    fn update_stats(&mut self) {
        self.stats.current_delay_ms = self.estimate_buffer_depth_ms();

        // Simple moving average for jitter (simplified calculation)
        let alpha = 0.1; // Smoothing factor
        let current_jitter = (self.stats.current_delay_ms - self.target_buffer_ms as f32).abs();
        self.stats.average_jitter_ms =
            alpha * current_jitter + (1.0 - alpha) * self.stats.average_jitter_ms;
    }

    /// Remove old packets that have been in buffer too long
    pub fn cleanup_old_packets(&mut self) {
        let now = Instant::now();
        let timeout = self.max_delay * 2; // Give up on packets older than 2x max delay

        let old_sequences: Vec<u32> = self
            .buffer
            .iter()
            .filter(|(_, buffered)| now.duration_since(buffered.received_at) > timeout)
            .map(|(&seq, _)| seq)
            .collect();

        for seq in old_sequences {
            self.buffer.remove(&seq);
            if seq >= self.next_sequence {
                self.stats.packets_lost += 1;
            }
        }
    }
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn create_test_frame(sample_count: usize) -> AudioFrame {
        AudioFrame {
            samples: vec![0.0; sample_count],
            sample_rate: 48000,
            channels: 1,
            timestamp: Instant::now(),
        }
    }

    #[test]
    fn test_in_order_delivery() {
        let mut buffer = JitterBuffer::new();

        // Push frames in order
        for i in 0..5 {
            assert!(buffer.push(i, create_test_frame(960)));
        }

        // Pop frames in order
        for _ in 0..5 {
            assert!(buffer.pop().is_some());
        }

        assert!(buffer.is_empty());
        assert_eq!(buffer.stats().packets_received, 5);
        assert_eq!(buffer.stats().packets_lost, 0);
    }

    #[test]
    fn test_out_of_order_delivery() {
        let mut buffer = JitterBuffer::new();

        // Push frames out of order: 0, 2, 1, 3
        assert!(buffer.push(0, create_test_frame(960)));
        assert!(buffer.push(2, create_test_frame(960)));
        assert!(buffer.push(1, create_test_frame(960)));
        assert!(buffer.push(3, create_test_frame(960)));

        // Should still pop in correct order
        for _ in 0..4 {
            assert!(buffer.pop().is_some());
        }

        assert!(buffer.is_empty());
        // Packet 2 arrives out of order (expected 1), and packet 3 also arrives out of order (still expecting 1)
        assert_eq!(buffer.stats().packets_out_of_order, 3);
    }

    #[test]
    fn test_duplicate_packets() {
        let mut buffer = JitterBuffer::new();

        assert!(buffer.push(0, create_test_frame(960)));
        assert!(!buffer.push(0, create_test_frame(960))); // Duplicate

        assert_eq!(buffer.stats().packets_duplicate, 1);
        assert_eq!(buffer.len(), 1);
    }

    #[test]
    fn test_packet_loss_recovery() {
        let mut buffer = JitterBuffer::with_config(20, 50);

        // Push packets 0, 2, 3, 4 (missing 1)
        assert!(buffer.push(0, create_test_frame(960)));
        assert!(buffer.pop().is_some()); // Get packet 0

        assert!(buffer.push(2, create_test_frame(960)));
        assert!(buffer.push(3, create_test_frame(960)));
        assert!(buffer.push(4, create_test_frame(960)));

        // After enough packets, should skip missing packet 1
        buffer.push(5, create_test_frame(960));
        buffer.push(6, create_test_frame(960));

        // Should eventually skip packet 1 and continue
        while !buffer.is_empty() {
            buffer.pop();
        }

        assert!(buffer.stats().packets_lost > 0);
    }

    #[test]
    fn test_reset() {
        let mut buffer = JitterBuffer::new();

        for i in 0..5 {
            buffer.push(i, create_test_frame(960));
        }

        buffer.reset();

        assert!(buffer.is_empty());
        assert_eq!(buffer.next_sequence, 0);
        assert_eq!(buffer.stats().packets_received, 0);
    }
}
