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
    StreamMaxData { stream_id: u64, max_data: u64 },
    MaxStreams { count: u64 },
    MaxChannels { count: u64 },
    ChannelOpen { channel_id: u64 },
    ChannelFin { channel_id: u64, last_message_id: u64 },
    ChannelMaxData {
        channel_id: u64,
        max_data: u64,
        max_messages: u64,
    },
    ChannelEvict { channel_id: u64, message_id: u64, size: u64 },
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
    /// Auth fragment ranges: `(offset, len)`.
    auth: Vec<(u64, usize)>,
    /// Rekey fragment ranges: `(offset, len)`.
    rekey: Vec<(u64, usize)>,
    sent_at: Instant,
}

/// Result of processing an ACK: what was lost.
#[derive(Default)]
pub struct LossReport {
    /// Stream ranges: `(stream_id, offset, len)`.
    pub streams: Vec<(u64, u64, usize)>,
    /// Channel ranges: `(channel_id, message_id, offset, len)`.
    pub channels: Vec<(u64, u64, u64, usize)>,
    /// Control frames to re-queue for sending.
    pub frames: Vec<ControlFrame>,
    /// Lost auth fragment ranges: `(offset, len)`.
    pub auth: Vec<(u64, usize)>,
    /// Lost rekey fragment ranges: `(offset, len)`.
    pub rekey: Vec<(u64, usize)>,
}

/// Successfully ACKed stream/channel ranges.
#[derive(Default)]
pub struct AckReport {
    pub streams: Vec<(u64, u64, usize)>,
    pub channels: Vec<(u64, u64, u64, usize)>,
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

    /// Detect timeout-based losses without receiving an ACK.
    ///
    /// Called when the loss timeout fires and no ACK has arrived.
    /// Returns lost stream/channel ranges and control frames.
    pub fn detect_timeout_losses(&mut self, now: Instant) -> LossReport {
        self.detect_losses(now)
    }

    pub fn detect_timeout_losses_into(&mut self, now: Instant, lost: &mut LossReport) {
        lost.streams.clear();
        lost.channels.clear();
        lost.frames.clear();
        lost.auth.clear();
        lost.rekey.clear();
        self.detect_losses_into(now, lost);
    }

    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    pub fn is_in_flight(&self, counter: u64) -> bool {
        self.in_flight.contains_key(&counter)
    }

    /// Record a sent packet.
    pub fn on_packet_sent(
        &mut self,
        counter: u64,
        streams: Vec<(u64, u64, usize)>,
        channels: Vec<(u64, u64, u64, usize)>,
        frames: Vec<ControlFrame>,
        auth: Vec<(u64, usize)>,
        rekey: Vec<(u64, usize)>,
        now: Instant,
    ) {
        if streams.is_empty()
            && channels.is_empty()
            && frames.is_empty()
            && auth.is_empty()
            && rekey.is_empty()
        {
            return;
        }
        self.in_flight.insert(
            counter,
            SentPacket {
                streams,
                channels,
                frames,
                auth,
                rekey,
                sent_at: now,
            },
        );
    }

    /// Process an incoming ACK.  Returns (acked, lost).  Allocates an
    /// `AckReport` and `LossReport` per call; for hot paths use
    /// `on_ack_received_into` with caller-owned scratch buffers.
    pub fn on_ack_received(&mut self, ack: &Ack, now: Instant) -> (AckReport, LossReport) {
        let mut acked = AckReport {
            streams: Vec::new(),
            channels: Vec::new(),
            frames: Vec::new(),
        };
        let mut lost = LossReport {
            streams: Vec::new(),
            channels: Vec::new(),
            frames: Vec::new(),
            auth: Vec::new(),
            rekey: Vec::new(),
        };
        self.on_ack_received_into(ack, now, &mut acked, &mut lost);
        (acked, lost)
    }

    /// Like `on_ack_received` but writes into caller-owned report buffers
    /// (which are cleared first).  Used by the hot path to reuse allocations.
    pub fn on_ack_received_into(
        &mut self,
        ack: &Ack,
        now: Instant,
        acked: &mut AckReport,
        lost: &mut LossReport,
    ) {
        acked.streams.clear();
        acked.channels.clear();
        acked.frames.clear();
        lost.streams.clear();
        lost.channels.clear();
        lost.frames.clear();
        lost.auth.clear();
        lost.rekey.clear();

        if self.largest_acked.map_or(true, |l| ack.largest_ack > l) {
            self.update_rtt(ack.largest_ack, ack.ack_delay, now);
            self.largest_acked = Some(ack.largest_ack);
        }

        // Walk decoded ranges and remove from in_flight directly into the
        // caller's `acked` report — avoids the intermediate Vec<(u64,u64)>
        // that `decode_ack_ranges` would build.
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
            self.remove_acked_range(start, cursor, acked);
            cursor = start.saturating_sub(1);
        }

        self.detect_losses_into(now, lost);
    }

    fn remove_acked_range(&mut self, start: u64, end: u64, acked: &mut AckReport) {
        // Collect keys to avoid mutating during iteration.  The temporary
        // capacity is bounded by the range size, which is typically small.
        let keys: Vec<u64> = self.in_flight.range(start..=end).map(|(&k, _)| k).collect();
        for key in keys {
            if let Some(pkt) = self.in_flight.remove(&key) {
                acked.streams.extend(pkt.streams);
                acked.channels.extend(pkt.channels);
                acked.frames.extend(pkt.frames);
            }
        }
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

    /// Remove packets covered by ACK ranges. Returns acked stream/channel data.
    fn remove_acked(&mut self, acked_ranges: &[(u64, u64)]) -> AckReport {
        let mut report = AckReport {
            streams: Vec::new(),
            channels: Vec::new(),
            frames: Vec::new(),
        };
        for &(start, end) in acked_ranges {
            let keys: Vec<u64> = self.in_flight.range(start..=end).map(|(&k, _)| k).collect();
            for key in keys {
                if let Some(pkt) = self.in_flight.remove(&key) {
                    report.streams.extend(pkt.streams);
                    report.channels.extend(pkt.channels);
                    report.frames.extend(pkt.frames);
                }
            }
        }
        report
    }

    /// Detect lost packets by gap and timeout.  Returns what was lost.
    ///
    /// Timeout-based detection runs even if no ACK has ever been received:
    /// otherwise a connection that loses every packet (and so never receives
    /// an ACK) would never retransmit.
    fn detect_losses(&mut self, now: Instant) -> LossReport {
        let mut report = LossReport {
            streams: Vec::new(),
            channels: Vec::new(),
            frames: Vec::new(),
            auth: Vec::new(),
            rekey: Vec::new(),
        };
        self.detect_losses_into(now, &mut report);
        report
    }

    fn detect_losses_into(&mut self, now: Instant, report: &mut LossReport) {
        let mut lost_counters: Vec<u64> = Vec::new();

        let timeout_start = match self.largest_acked {
            Some(largest_acked) if largest_acked >= PACKET_LOSS_THRESHOLD => {
                let gap_threshold = largest_acked - PACKET_LOSS_THRESHOLD;
                lost_counters.extend(self.in_flight.range(..=gap_threshold).map(|(&k, _)| k));
                gap_threshold + 1
            }
            _ => 0,
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
                report.auth.extend(packet.auth);
                report.rekey.extend(packet.rekey);
            }
        }
    }
}

/// Tracks received packet counters for generating ACK frames.
///
/// Maintains sorted, non-overlapping counter ranges.  When a new packet
/// arrives, its counter is inserted and adjacent ranges are merged.
///
/// A `floor` marks the lower bound: counters below the floor are rejected
/// as stale.  The floor is advanced via `advance_floor()` when the peer
/// confirms receipt of our ACK (ACK-of-ACK), meaning all ranges below
/// that point can be discarded.
pub struct RecvAckState {
    ranges: Vec<(u64, u64)>,
    floor: u64,
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
            floor: 0,
            pending: false,
            largest: None,
            largest_recv_time: None,
        }
    }

    /// Check if a packet counter is new (not duplicate).
    pub fn should_accept(&self, counter: u64) -> RecvResult {
        if counter < self.floor || self.contains(counter) {
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
    ///
    /// Also returns the current floor — the caller should record this along
    /// with the packet counter so that when the peer ACKs our packet, we
    /// can call `advance_floor()` with this value.
    pub fn generate_ack(&mut self, now: Instant) -> Option<(Ack, u64)> {
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

        let ack = Ack {
            largest_ack: largest,
            ack_delay,
            ranges: ack_ranges,
        };

        // The largest counter we've seen — when peer ACKs the packet
        // containing this ACK, all counters up to this are safe to discard.
        let ack_floor = largest + 1;

        Some((ack, ack_floor))
    }

    /// Advance the floor.  All ranges fully below `new_floor` are removed.
    /// Counters below the floor will be rejected by `should_accept()`.
    ///
    /// Called when the peer ACKs a packet that contained our ACK frame,
    /// confirming they've seen our receive state up to that point.
    pub fn advance_floor(&mut self, new_floor: u64) {
        if new_floor <= self.floor {
            return;
        }
        self.floor = new_floor;

        // Remove ranges entirely below the floor.
        while let Some(&(start, end)) = self.ranges.first() {
            if end < self.floor {
                self.ranges.remove(0);
            } else if start < self.floor {
                // Trim the range: floor cuts into it.
                self.ranges[0].0 = self.floor;
                break;
            } else {
                break;
            }
        }
    }

    /// Current floor value.
    pub fn floor(&self) -> u64 {
        self.floor
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

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    // =========================================================================
    // Bug reproduction tests — these should FAIL on current code.
    // =========================================================================

    #[test]
    fn advance_floor_trims_ranges() {
        let mut state = RecvAckState::new();
        let t = now();

        // Receive every other counter: 0, 2, 4, ..., 198 → 100 ranges.
        for i in 0..100u64 {
            state.commit(i * 2, t);
        }
        assert_eq!(state.ranges.len(), 100);

        // Advance floor past the first 50 ranges.
        // Counter 98 is the end of range (98, 98), so floor=99 discards 0..98.
        state.advance_floor(99);
        assert!(state.ranges.len() <= 51);
        assert_eq!(state.floor(), 99);
    }

    #[test]
    fn floor_rejects_old_counters() {
        let mut state = RecvAckState::new();
        let t = now();

        state.commit(0, t);
        state.commit(1, t);
        state.commit(2, t);

        // Advance floor past these.
        state.advance_floor(3);

        // Ranges trimmed.
        assert!(state.ranges.is_empty());

        // Counters below floor are rejected without being in ranges.
        assert_eq!(state.should_accept(0), RecvResult::Duplicate);
        assert_eq!(state.should_accept(2), RecvResult::Duplicate);

        // Counter at/above floor is accepted.
        assert_eq!(state.should_accept(3), RecvResult::Accepted);
    }

    #[test]
    fn advance_floor_trims_partial_range() {
        let mut state = RecvAckState::new();
        let t = now();

        // Contiguous range (0, 10).
        for i in 0..=10u64 {
            state.commit(i, t);
        }
        assert_eq!(state.ranges.len(), 1);
        assert_eq!(state.ranges[0], (0, 10));

        // Floor cuts into the range.
        state.advance_floor(5);
        assert_eq!(state.ranges.len(), 1);
        assert_eq!(state.ranges[0], (5, 10));
        assert_eq!(state.should_accept(4), RecvResult::Duplicate);
        assert_eq!(state.should_accept(5), RecvResult::Duplicate); // in range
    }

    #[test]
    fn generate_ack_returns_floor() {
        let mut state = RecvAckState::new();
        let t = now();

        state.commit(5, t);
        state.commit(6, t);

        let (ack, ack_floor) = state.generate_ack(t).unwrap();
        assert_eq!(ack.largest_ack, 6);
        // ack_floor = largest + 1 = 7.
        assert_eq!(ack_floor, 7);
    }

    // =========================================================================
    // Existing behavior tests (should always pass).
    // =========================================================================

    #[test]
    fn basic_accept_and_duplicate() {
        let mut state = RecvAckState::new();
        let t = now();

        assert_eq!(state.should_accept(0), RecvResult::Accepted);
        state.commit(0, t);
        assert_eq!(state.should_accept(0), RecvResult::Duplicate);
        assert_eq!(state.should_accept(1), RecvResult::Accepted);
    }

    #[test]
    fn out_of_order_merges() {
        let mut state = RecvAckState::new();
        let t = now();

        state.commit(0, t);
        state.commit(2, t);
        assert_eq!(state.ranges.len(), 2);

        // Fill the gap → merges into one range.
        state.commit(1, t);
        assert_eq!(state.ranges.len(), 1);
        assert_eq!(state.ranges[0], (0, 2));
    }

    #[test]
    fn generate_ack_resets_pending() {
        let mut state = RecvAckState::new();
        let t = now();

        state.commit(0, t);
        assert!(state.generate_ack(t).is_some());
        assert!(state.generate_ack(t).is_none()); // pending cleared
    }
}
