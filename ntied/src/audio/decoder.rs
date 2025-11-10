use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tokio::sync::{Mutex as TokioMutex, mpsc};

use crate::packet::AudioDataPacket;

use super::codec::{CodecType, create_decoder};
use super::{AudioConfig, AudioFrame, Resampler};

/// Wrapper for buffered packet data in jitter buffer
struct BufferedPacket {
    data: Vec<u8>,
    timestamp: Instant,
}

pub struct Decoder {
    tx: mpsc::Sender<AudioDataPacket>,
    rx: TokioMutex<mpsc::Receiver<AudioFrame>>,
    sent_packets: Arc<AtomicU64>,
    received_frames: Arc<AtomicU64>,
    sent_bytes: Arc<AtomicU64>,
    received_bytes: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl Decoder {
    const BUFFER_SIZE: usize = 100;

    /// Create a new decoder
    ///
    /// # Arguments
    /// * `target_config` - Audio configuration for the LOCAL playback device (speaker)
    /// * `codec_type` - Codec used for decoding
    ///
    /// # Behavior
    /// The decoder determines codec channels dynamically from incoming AudioDataPacket.channels
    /// (sent by the REMOTE peer). This allows the decoder to adapt to the remote microphone
    /// configuration without prior knowledge. The decoder converts from codec channels to
    /// target_config.channels for final playback on the LOCAL speaker.
    pub fn new(target_config: AudioConfig, codec_type: CodecType) -> Self {
        let (frame_tx, rx) = mpsc::channel(Self::BUFFER_SIZE);
        let (tx, packet_rx) = mpsc::channel(Self::BUFFER_SIZE);
        let rx = TokioMutex::new(rx);
        let sent_packets = Arc::new(AtomicU64::new(0));
        let received_frames = Arc::new(AtomicU64::new(0));
        let sent_bytes = Arc::new(AtomicU64::new(0));
        let received_bytes = Arc::new(AtomicU64::new(0));
        let task = tokio::spawn(Self::main_loop(
            target_config,
            codec_type,
            frame_tx,
            packet_rx,
            sent_packets.clone(),
            received_frames.clone(),
            sent_bytes.clone(),
            received_bytes.clone(),
        ));
        Self {
            tx,
            rx,
            sent_packets,
            received_frames,
            sent_bytes,
            received_bytes,
            task,
        }
    }

    /// Send a packet for decoding
    pub async fn send_packet(
        &self,
        packet: AudioDataPacket,
    ) -> Result<(), mpsc::error::SendError<AudioDataPacket>> {
        self.tx.send(packet).await
    }

    /// Receive a decoded audio frame
    pub async fn recv_frame(&self) -> Option<AudioFrame> {
        self.rx.lock().await.recv().await
    }

    async fn main_loop(
        target_config: AudioConfig,
        codec_type: CodecType,
        tx: mpsc::Sender<AudioFrame>,
        mut rx: mpsc::Receiver<AudioDataPacket>,
        sent_packets: Arc<AtomicU64>,
        received_frames: Arc<AtomicU64>,
        sent_bytes: Arc<AtomicU64>,
        received_bytes: Arc<AtomicU64>,
    ) {
        tracing::info!("Decoder main loop started");
        tracing::info!(
            "Decoder: target device has {} channels",
            target_config.channels
        );

        // Decoder and resampler will be created when we receive the first packet
        // and determine the codec channels from the packet data
        let mut decoder: Option<Box<dyn super::codec::AudioDecoder>> = None;
        let mut resampler: Option<Resampler> = None;
        let mut current_codec_channels: Option<u16> = None;

        // Create jitter buffer - using a simple HashMap instead of JitterBuffer
        // since JitterBuffer expects AudioFrame
        use std::collections::BTreeMap;
        let mut packet_buffer: BTreeMap<u32, BufferedPacket> = BTreeMap::new();
        let mut next_sequence: u32 = 0;

        // Frame generation loop
        let target_frame_size =
            (target_config.sample_rate as usize * 20 / 1000) * target_config.channels as usize;

        let mut loop_count = 0u64;
        tracing::info!(
            "Decoder entering main loop, target frame size: {}",
            target_frame_size
        );

        // Create a timer for frame generation (50 frames per second = 20ms per frame)
        let mut frame_interval = tokio::time::interval(tokio::time::Duration::from_millis(20));
        frame_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                // Receive incoming packets (non-blocking)
                Some(packet) = rx.recv() => {
                    let packet_count = sent_packets.fetch_add(1, Ordering::Relaxed) + 1;
                    sent_bytes.fetch_add(packet.data.len() as u64, Ordering::Relaxed);

                    if packet_count % 50 == 0 {
                        tracing::debug!("Decoder received packet #{}, seq: {}, size: {}, channels: {}", packet_count, packet.sequence, packet.data.len(), packet.channels);
                    }

                    // Check if we need to recreate decoder due to channel change
                    if current_codec_channels != Some(packet.channels) {
                        tracing::info!(
                            "Decoder: Creating/updating codec decoder for {} channels (was: {:?})",
                            packet.channels,
                            current_codec_channels
                        );

                        // Create new decoder with channels from packet
                        decoder = match create_decoder(codec_type, packet.channels) {
                            Ok(dec) => Some(dec),
                            Err(e) => {
                                tracing::error!("Failed to create decoder: {}", e);
                                continue;
                            }
                        };

                        let actual_codec_config = decoder.as_ref().unwrap().codec_config();
                        tracing::info!(
                            "Decoder initialized: codec={}Hz/{}ch, target={}Hz/{}ch",
                            actual_codec_config.sample_rate,
                            actual_codec_config.channels,
                            target_config.sample_rate,
                            target_config.channels
                        );

                        // Create resampler if needed
                        resampler = if actual_codec_config.sample_rate != target_config.sample_rate {
                            match Resampler::new(
                                actual_codec_config.sample_rate,
                                target_config.sample_rate,
                                actual_codec_config.channels,
                            ) {
                                Ok(r) => Some(r),
                                Err(e) => {
                                    tracing::error!("Failed to create resampler: {}", e);
                                    continue;
                                }
                            }
                        } else {
                            None
                        };

                        current_codec_channels = Some(packet.channels);
                    }

                    // Store packet in buffer
                    packet_buffer.insert(packet.sequence, BufferedPacket {
                        data: packet.data,
                        timestamp: Instant::now(),
                    });
                }

                // Generate output frames at regular intervals (this takes priority)
                _ = frame_interval.tick() => {
                    loop_count += 1;
                    if loop_count % 50 == 0 {
                        tracing::debug!("Decoder frame generation tick #{}, buffer has {} packets", loop_count, packet_buffer.len());
                    }

                    // Skip frame generation if decoder not initialized yet
                    let Some(ref mut dec) = decoder else {
                        continue;
                    };

                    let codec_config = dec.codec_config();

                    // Try to get packet from buffer
                    let decoded_samples = if let Some(buffered_packet) = packet_buffer.remove(&next_sequence) {
                        // Decode the packet data
                        match dec.decode(&buffered_packet.data) {
                            Ok(samples) => {
                                next_sequence = next_sequence.wrapping_add(1);
                                samples
                            }
                            Err(e) => {
                                tracing::error!("Decoding failed: {}", e);
                                next_sequence = next_sequence.wrapping_add(1);
                                // Use PLC
                                match dec.conceal_packet_loss() {
                                    Ok(plc_samples) => plc_samples,
                                    Err(e) => {
                                        tracing::error!("PLC failed: {}", e);
                                        // Generate silence
                                        let codec_frame_size = (codec_config.sample_rate as usize * 20 / 1000)
                                            * codec_config.channels as usize;
                                        vec![0.0; codec_frame_size]
                                    }
                                }
                            }
                        }
                    } else {
                        // No packet available - check if we should skip ahead
                        let now = Instant::now();
                        let should_skip = packet_buffer.iter().next().map(|(&seq, pkt)| {
                            seq > next_sequence && now.duration_since(pkt.timestamp).as_millis() > 100
                        }).unwrap_or(false);

                        if should_skip {
                            // Skip to next available packet
                            if let Some((&seq, _)) = packet_buffer.iter().next() {
                                tracing::debug!("Skipping from sequence {} to {}", next_sequence, seq);
                                next_sequence = seq;
                            }
                        }

                        // Use PLC for missing packet
                        match dec.conceal_packet_loss() {
                            Ok(plc_samples) => plc_samples,
                            Err(e) => {
                                tracing::error!("PLC failed: {}", e);
                                // Generate silence
                                let codec_frame_size = (codec_config.sample_rate as usize * 20 / 1000)
                                    * codec_config.channels as usize;
                                vec![0.0; codec_frame_size]
                            }
                        }
                    };

                    // Resample if needed
                    let mut samples = if let Some(ref mut resampler) = resampler {
                        match resampler.resample(&decoded_samples) {
                            Ok(resampled) => resampled,
                            Err(e) => {
                                tracing::error!("Resampling failed: {}", e);
                                continue;
                            }
                        }
                    } else {
                        decoded_samples
                    };

                    // Channel conversion
                    if codec_config.channels < target_config.channels {
                        // Upmix mono to stereo
                        tracing::trace!(
                            "Decoder upmixing {} -> {} channels, {} samples",
                            codec_config.channels,
                            target_config.channels,
                            samples.len()
                        );
                        samples = upmix_to_stereo(&samples);
                    } else if codec_config.channels > target_config.channels {
                        // Downmix stereo to mono
                        tracing::trace!(
                            "Decoder downmixing {} -> {} channels, {} samples",
                            codec_config.channels,
                            target_config.channels,
                            samples.len()
                        );
                        samples = downmix_to_mono(&samples, codec_config.channels);
                    }

                    // Ensure we have the right frame size
                    if samples.len() < target_frame_size {
                        // Pad with zeros if too short
                        samples.resize(target_frame_size, 0.0);
                    } else if samples.len() > target_frame_size {
                        // Truncate if too long
                        samples.truncate(target_frame_size);
                    }

                    // Create output frame
                    let frame = AudioFrame {
                        samples,
                        sample_rate: target_config.sample_rate,
                        channels: target_config.channels,
                        timestamp: std::time::Instant::now(),
                    };

                    let frame_count = received_frames.fetch_add(1, Ordering::Relaxed) + 1;
                    received_bytes.fetch_add((frame.samples.len() * 4) as u64, Ordering::Relaxed);

                    if frame_count % 50 == 0 {
                        tracing::debug!("Decoder generated frame #{}, samples: {}", frame_count, frame.samples.len());
                    }

                    // Send frame
                    if let Err(e) = tx.send(frame).await {
                        tracing::warn!("Decoder frame receiver dropped: {:?}", e);
                        return;
                    }

                    if frame_count % 50 == 0 {
                        tracing::debug!("Decoder sent frame #{} successfully", frame_count);
                    }

                    // Cleanup old packets from buffer (older than 500ms)
                    let now = Instant::now();
                    packet_buffer.retain(|_, pkt| now.duration_since(pkt.timestamp).as_millis() < 500);
                }
            }
        }
    }

    pub fn stats(&self) -> DecoderStats {
        DecoderStats {
            sent_packets: self.sent_packets.load(Ordering::Relaxed),
            received_frames: self.received_frames.load(Ordering::Relaxed),
            sent_bytes: self.sent_bytes.load(Ordering::Relaxed),
            received_bytes: self.received_bytes.load(Ordering::Relaxed),
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug)]
pub struct DecoderStats {
    pub sent_packets: u64,
    pub received_frames: u64,
    pub sent_bytes: u64,
    pub received_bytes: u64,
}

/// Downmix multi-channel audio to mono by averaging channels
fn downmix_to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels == 1 {
        return samples.to_vec();
    }

    let channels = channels as usize;
    let mono_samples = samples.len() / channels;
    let mut output = Vec::with_capacity(mono_samples);

    for i in 0..mono_samples {
        let mut sum = 0.0;
        for ch in 0..channels {
            sum += samples[i * channels + ch];
        }
        output.push(sum / channels as f32);
    }

    output
}

/// Upmix mono audio to stereo by duplicating to both channels
fn upmix_to_stereo(samples: &[f32]) -> Vec<f32> {
    let mut output = Vec::with_capacity(samples.len() * 2);
    for &sample in samples {
        output.push(sample); // Left
        output.push(sample); // Right
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_downmix_to_mono() {
        // Stereo to mono
        let stereo = vec![0.5, -0.5, 0.3, -0.3, 0.1, -0.1];
        let mono = downmix_to_mono(&stereo, 2);
        assert_eq!(mono.len(), 3);
        assert!((mono[0] - 0.0).abs() < 1e-6);
        assert!((mono[1] - 0.0).abs() < 1e-6);
        assert!((mono[2] - 0.0).abs() < 1e-6);

        // Already mono
        let mono_input = vec![0.5, 0.3, 0.1];
        let mono_output = downmix_to_mono(&mono_input, 1);
        assert_eq!(mono_output, mono_input);
    }

    #[test]
    fn test_upmix_to_stereo() {
        let mono = vec![0.5, 0.3, 0.1];
        let stereo = upmix_to_stereo(&mono);
        assert_eq!(stereo.len(), 6);
        assert_eq!(stereo[0], 0.5); // L
        assert_eq!(stereo[1], 0.5); // R
        assert_eq!(stereo[2], 0.3); // L
        assert_eq!(stereo[3], 0.3); // R
    }
}
