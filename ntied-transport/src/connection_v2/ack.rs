use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// A packet is considered lost if this many newer packets have been acked.
const PACKET_LOSS_THRESHOLD: u64 = 3;
const MIN_LOSS_TIMEOUT: Duration = Duration::from_millis(50);
const INITIAL_LOSS_TIMEOUT: Duration = Duration::from_millis(500);

pub const MAX_ACK_RANGES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckRange {
    pub gap: u64,
    pub length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ack {
    pub largest_ack: u64,
    pub ack_delay: u16,
    pub ranges: Vec<AckRange>,
}

/// Owned control frame stored for retransmission on loss.
///
/// Stream and channel data is tracked separately via offset tuples,
/// so this only covers small control frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlFrame {
    Ping { id: u32 },
    Pong { id: u32 },
    AuthComplete,
    ConnectionClose { error_code: u32, reason: Vec<u8> },
    WindowUpdate { stream_id: u64, max_offset: u64 },
    ChannelClose { channel_id: u64 },
}

/// Record of what a sent packet carried, for ack/loss handling.
///
/// Stream data is `(stream_id, offset, len)`.
/// Channel data is `(channel_id, message_id, offset, len)`.
/// Control frames are stored as owned values.
struct SentPacket {
    streams: Vec<(u64, u64, usize)>,
    channels: Vec<(u64, u64, u64, usize)>,
    frames: Vec<ControlFrame>,
    sent_at: Instant,
}

/// Result of processing an ACK: what was lost.
pub struct LossReport {
    /// Stream ranges: `(stream_id, offset, len)`.
    pub streams: Vec<(u64, u64, usize)>,
    /// Channel ranges: `(channel_id, message_id, offset, len)`.
    pub channels: Vec<(u64, u64, u64, usize)>,
    /// Control frames to re-queue for sending.
    pub frames: Vec<ControlFrame>,
}

/// Tracks sent packets, measures RTT, detects losses.
///
/// # Loss detection
///
/// Two mechanisms (same as QUIC, RFC 9002):
/// - **Gap-based**: a packet is lost if `PACKET_LOSS_THRESHOLD` (3) newer
///   packets have been acknowledged.
/// - **Timeout-based**: a packet is lost if `loss_timeout` has elapsed since
///   it was sent.  `loss_timeout = rtt_average + 4 × rtt_deviation`.
///
/// # RTT estimation
///
/// Exponentially weighted moving average (RFC 6298):
/// - `rtt_average = 7/8 × rtt_average + 1/8 × sample`
/// - `rtt_deviation = 3/4 × rtt_deviation + 1/4 × |rtt_average - sample|`
pub struct SendAckState {
    in_flight: BTreeMap<u64, SentPacket>,
    largest_acked: Option<u64>,
    /// Smoothed (average) round-trip time.
    rtt_average: Option<Duration>,
    /// Mean deviation of RTT samples.
    rtt_deviation: Duration,
    /// Time after which an unacked packet is considered lost.
    /// `max(rtt_average + 4 × rtt_deviation, MIN_LOSS_TIMEOUT)`
    loss_timeout: Duration,
}

impl SendAckState {
    pub fn new() -> Self {
        Self {
            in_flight: BTreeMap::new(),
            largest_acked: None,
            rtt_average: None,
            rtt_deviation: Duration::ZERO,
            loss_timeout: INITIAL_LOSS_TIMEOUT,
        }
    }

    pub fn loss_timeout(&self) -> Duration {
        self.loss_timeout
    }

    pub fn rtt_average(&self) -> Option<Duration> {
        self.rtt_average
    }

    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Record a sent packet.
    ///
    /// `streams`: `(stream_id, offset, len)` for each stream frame.
    /// `channels`: `(channel_id, message_id, offset, len)` for each channel frame.
    /// `frames`: control frames that need retransmission on loss.
    pub fn on_packet_sent(
        &mut self,
        counter: u64,
        streams: Vec<(u64, u64, usize)>,
        channels: Vec<(u64, u64, u64, usize)>,
        frames: Vec<ControlFrame>,
        now: Instant,
    ) {
        if streams.is_empty() && channels.is_empty() && frames.is_empty() {
            return;
        }
        self.in_flight.insert(
            counter,
            SentPacket {
                streams,
                channels,
                frames,
                sent_at: now,
            },
        );
    }

    /// Process an incoming ACK.  Returns lost stream/channel ranges and frames.
    pub fn on_ack_received(&mut self, ack: &Ack, now: Instant) -> LossReport {
        let acked_ranges = decode_ack_ranges(ack);

        if self.largest_acked.map_or(true, |l| ack.largest_ack > l) {
            self.update_rtt(ack.largest_ack, ack.ack_delay, now);
            self.largest_acked = Some(ack.largest_ack);
        }

        self.remove_acked(&acked_ranges);
        self.detect_losses(now)
    }

    /// Update RTT estimate from a newly acked packet.
    fn update_rtt(&mut self, acked_counter: u64, ack_delay_us: u16, now: Instant) {
        let Some(packet) = self.in_flight.get(&acked_counter) else {
            return;
        };

        let ack_delay = Duration::from_micros(ack_delay_us as u64);
        let sample = now.duration_since(packet.sent_at).saturating_sub(ack_delay);

        match self.rtt_average {
            None => {
                self.rtt_average = Some(sample);
                self.rtt_deviation = sample / 2;
            }
            Some(avg) => {
                let diff = if avg > sample {
                    avg - sample
                } else {
                    sample - avg
                };
                self.rtt_deviation = (self.rtt_deviation * 3 + diff) / 4;
                self.rtt_average = Some((avg * 7 + sample) / 8);
            }
        }

        let avg = self.rtt_average.unwrap();
        self.loss_timeout = (avg + self.rtt_deviation * 4).max(MIN_LOSS_TIMEOUT);
    }

    /// Remove packets covered by ACK ranges.
    fn remove_acked(&mut self, acked_ranges: &[(u64, u64)]) {
        for &(start, end) in acked_ranges {
            let keys: Vec<u64> = self.in_flight.range(start..=end).map(|(&k, _)| k).collect();
            for key in keys {
                self.in_flight.remove(&key);
            }
        }
    }

    /// Detect lost packets by gap and timeout.  Returns what was lost.
    fn detect_losses(&mut self, now: Instant) -> LossReport {
        let mut report = LossReport {
            streams: Vec::new(),
            channels: Vec::new(),
            frames: Vec::new(),
        };

        let Some(largest_acked) = self.largest_acked else {
            return report;
        };

        let mut lost_counters: Vec<u64> = Vec::new();

        // Gap-based: packets older than largest_acked - threshold.
        if largest_acked >= PACKET_LOSS_THRESHOLD {
            let gap_threshold = largest_acked - PACKET_LOSS_THRESHOLD;
            lost_counters.extend(self.in_flight.range(..=gap_threshold).map(|(&k, _)| k));
        }

        // Timeout-based: packets sent more than loss_timeout ago.
        let timeout_start = if largest_acked >= PACKET_LOSS_THRESHOLD {
            largest_acked - PACKET_LOSS_THRESHOLD + 1
        } else {
            0
        };
        for (&counter, packet) in self.in_flight.range(timeout_start..) {
            if now.duration_since(packet.sent_at) > self.loss_timeout {
                lost_counters.push(counter);
            }
        }

        for counter in lost_counters {
            if let Some(packet) = self.in_flight.remove(&counter) {
                report.streams.extend(packet.streams);
                report.channels.extend(packet.channels);
                report.frames.extend(packet.frames);
            }
        }

        report
    }
}

/// Tracks received packet counters for generating ACK frames.
///
/// Maintains sorted, non-overlapping counter ranges.  When a new packet
/// arrives, its counter is inserted and adjacent ranges are merged.
pub struct RecvAckState {
    ranges: Vec<(u64, u64)>,
    largest: Option<u64>,
    largest_recv_time: Option<Instant>,
    pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvResult {
    Accepted,
    Duplicate,
}

impl RecvAckState {
    pub fn new() -> Self {
        Self {
            ranges: Vec::new(),
            pending: false,
            largest: None,
            largest_recv_time: None,
        }
    }

    /// Check if a packet counter is new (not duplicate).
    pub fn should_accept(&self, counter: u64) -> RecvResult {
        if self.contains(counter) {
            RecvResult::Duplicate
        } else {
            RecvResult::Accepted
        }
    }

    /// Record receipt of a packet.  Call after `should_accept` returns `Accepted`.
    pub fn commit(&mut self, counter: u64, now: Instant) {
        self.insert(counter);
        if self.largest.map_or(true, |l| counter > l) {
            self.largest = Some(counter);
            self.largest_recv_time = Some(now);
        }
        self.pending = true;
    }

    /// Generate an ACK frame if there are unacknowledged received packets.
    pub fn generate_ack(&mut self, now: Instant) -> Option<Ack> {
        if !self.pending {
            return None;
        }
        self.pending = false;
        let largest = self.largest?;
        let largest_recv_time = self.largest_recv_time.unwrap();

        let ack_delay = now
            .duration_since(largest_recv_time)
            .as_micros()
            .min(u16::MAX as u128) as u16;

        let mut ack_ranges = Vec::new();
        let mut cursor = largest;

        for &(start, end) in self.ranges.iter().rev() {
            let gap = cursor - end;
            let length = end - start + 1;
            ack_ranges.push(AckRange { gap, length });
            cursor = start.saturating_sub(1);

            if ack_ranges.len() >= MAX_ACK_RANGES {
                break;
            }
        }

        Some(Ack {
            largest_ack: largest,
            ack_delay,
            ranges: ack_ranges,
        })
    }

    fn contains(&self, counter: u64) -> bool {
        self.ranges
            .binary_search_by(|&(start, end)| {
                if counter < start {
                    std::cmp::Ordering::Greater
                } else if counter > end {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }

    fn insert(&mut self, counter: u64) {
        let idx = self.ranges.partition_point(|&(start, _)| start <= counter);

        let merge_prev = idx > 0 && self.ranges[idx - 1].1 + 1 == counter;
        let merge_next = idx < self.ranges.len() && counter + 1 == self.ranges[idx].0;

        match (merge_prev, merge_next) {
            (true, true) => {
                let new_end = self.ranges[idx].1;
                self.ranges[idx - 1].1 = new_end;
                self.ranges.remove(idx);
            }
            (true, false) => {
                self.ranges[idx - 1].1 = counter;
            }
            (false, true) => {
                self.ranges[idx].0 = counter;
            }
            (false, false) => {
                self.ranges.insert(idx, (counter, counter));
            }
        }
    }
}

fn decode_ack_ranges(ack: &Ack) -> Vec<(u64, u64)> {
    let mut ranges = Vec::with_capacity(ack.ranges.len());
    let mut cursor = ack.largest_ack;

    for range in &ack.ranges {
        let Some(after_gap) = cursor.checked_sub(range.gap) else {
            break;
        };
        cursor = after_gap;

        if range.length == 0 {
            break;
        }

        let start = cursor.saturating_sub(range.length - 1);
        ranges.push((start, cursor));
        cursor = start.saturating_sub(1);
    }

    ranges
}
