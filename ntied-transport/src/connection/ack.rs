use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::wire::{Ack, AckRange, Frame, MAX_ACK_RANGES};

pub const PACKET_LOSS_THRESHOLD: u64 = 3;
pub const MIN_RTO: Duration = Duration::from_millis(50);
pub const INITIAL_RTO: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvResult {
    Accepted,
    Duplicate,
    BelowFloor,
}

pub struct RecvAckState {
    floor: u64,
    ranges: Vec<(u64, u64)>,
    largest: Option<u64>,
    largest_recv_time: Option<Instant>,
    ack_needed: bool,
}

impl RecvAckState {
    pub fn new() -> Self {
        Self {
            floor: 0,
            ranges: Vec::new(),
            ack_needed: false,
            largest: None,
            largest_recv_time: None,
        }
    }

    pub fn largest(&self) -> Option<u64> {
        self.largest
    }

    pub fn floor(&self) -> u64 {
        self.floor
    }

    pub fn should_accept(&self, counter: u64) -> RecvResult {
        if counter < self.floor {
            return RecvResult::BelowFloor;
        }
        if self.contains(counter) {
            return RecvResult::Duplicate;
        }
        RecvResult::Accepted
    }

    pub fn receive(&mut self, counter: u64, now: Instant) -> RecvResult {
        let result = self.should_accept(counter);
        if result == RecvResult::Accepted {
            self.commit(counter, now);
        }
        result
    }

    pub fn commit(&mut self, counter: u64, now: Instant) {
        self.insert(counter);
        if self.largest.map_or(true, |l| counter > l) {
            self.largest = Some(counter);
            self.largest_recv_time = Some(now);
        }
        self.ack_needed = true;
    }

    pub fn generate_ack(&mut self, now: Instant) -> Option<Ack> {
        if !self.ack_needed {
            return None;
        }
        self.ack_needed = false;
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

    pub fn advance_floor(&mut self, new_floor: u64) {
        if new_floor <= self.floor {
            return;
        }
        self.floor = new_floor;
        self.ranges.retain(|&(_, end)| end >= new_floor);
        if let Some(first) = self.ranges.first_mut() {
            if first.0 < new_floor {
                first.0 = new_floor;
            }
        }
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

struct InFlightEntry {
    frames: Vec<Frame>,
    sent_at: Instant,
}

pub struct SendAckState {
    in_flight: BTreeMap<u64, InFlightEntry>,
    largest_acked: Option<u64>,
    srtt: Option<Duration>,
    rttvar: Duration,
    rto: Duration,
}

impl SendAckState {
    pub fn new() -> Self {
        Self {
            in_flight: BTreeMap::new(),
            largest_acked: None,
            srtt: None,
            rttvar: Duration::ZERO,
            rto: INITIAL_RTO,
        }
    }

    pub fn rto(&self) -> Duration {
        self.rto
    }

    pub fn srtt(&self) -> Option<Duration> {
        self.srtt
    }

    pub fn largest_acked(&self) -> Option<u64> {
        self.largest_acked
    }

    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }

    pub fn on_packet_sent(&mut self, counter: u64, frames: Vec<Frame>, now: Instant) {
        let retransmittable: Vec<Frame> = frames
            .into_iter()
            .filter(|f| f.is_ack_eliciting())
            .collect();
        if !retransmittable.is_empty() {
            self.in_flight.insert(
                counter,
                InFlightEntry {
                    frames: retransmittable,
                    sent_at: now,
                },
            );
        }
    }

    pub fn on_ack_received(&mut self, ack: &Ack, now: Instant) -> Vec<Frame> {
        let acked_ranges = decode_ack_ranges(ack);

        if self.largest_acked.map_or(true, |l| ack.largest_ack > l) {
            self.update_rtt(ack.largest_ack, ack.ack_delay, now);
            self.largest_acked = Some(ack.largest_ack);
        }

        self.remove_acked(&acked_ranges);
        self.detect_losses(now)
    }

    fn update_rtt(&mut self, acked_counter: u64, ack_delay_us: u16, now: Instant) {
        let Some(entry) = self.in_flight.get(&acked_counter) else {
            return;
        };

        let ack_delay = Duration::from_micros(ack_delay_us as u64);
        let rtt_sample = now.duration_since(entry.sent_at).saturating_sub(ack_delay);

        match self.srtt {
            None => {
                self.srtt = Some(rtt_sample);
                self.rttvar = rtt_sample / 2;
            }
            Some(srtt) => {
                let diff = if srtt > rtt_sample {
                    srtt - rtt_sample
                } else {
                    rtt_sample - srtt
                };
                self.rttvar = (self.rttvar * 3 + diff) / 4;
                self.srtt = Some((srtt * 7 + rtt_sample) / 8);
            }
        }

        let srtt = self.srtt.unwrap();
        self.rto = (srtt + self.rttvar * 4).max(MIN_RTO);
    }

    fn remove_acked(&mut self, acked_ranges: &[(u64, u64)]) {
        for &(start, end) in acked_ranges {
            let keys: Vec<u64> = self.in_flight.range(start..=end).map(|(&k, _)| k).collect();
            for key in keys {
                self.in_flight.remove(&key);
            }
        }
    }

    fn detect_losses(&mut self, now: Instant) -> Vec<Frame> {
        let Some(largest_acked) = self.largest_acked else {
            return Vec::new();
        };

        let mut lost_counters: Vec<u64> = Vec::new();

        if largest_acked >= PACKET_LOSS_THRESHOLD {
            let gap_threshold = largest_acked - PACKET_LOSS_THRESHOLD;
            lost_counters.extend(self.in_flight.range(..=gap_threshold).map(|(&k, _)| k));
        }

        let timeout_start = if largest_acked >= PACKET_LOSS_THRESHOLD {
            largest_acked - PACKET_LOSS_THRESHOLD + 1
        } else {
            0
        };
        for (&counter, entry) in self.in_flight.range(timeout_start..) {
            if now.duration_since(entry.sent_at) > self.rto {
                lost_counters.push(counter);
            }
        }

        let mut lost = Vec::new();
        for counter in lost_counters {
            if let Some(entry) = self.in_flight.remove(&counter) {
                lost.extend(entry.frames);
            }
        }

        lost
    }
}

pub fn decode_ack_ranges(ack: &Ack) -> Vec<(u64, u64)> {
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
