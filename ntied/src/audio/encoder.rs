use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{Mutex as TokioMutex, mpsc};
use uuid::Uuid;

use crate::packet::AudioDataPacket;

use super::codec::{CodecType, create_encoder};
use super::{AudioConfig, AudioFrame, Resampler};

pub struct Encoder {
    tx: mpsc::Sender<AudioFrame>,
    rx: TokioMutex<mpsc::Receiver<AudioDataPacket>>,
    sent_frames: Arc<AtomicU64>,
    received_packets: Arc<AtomicU64>,
    sent_bytes: Arc<AtomicU64>,
    received_bytes: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl Encoder {
    const BUFFER_SIZE: usize = 100;

    /// Create a new encoder
    ///
    /// # Arguments
    /// * `call_id` - UUID of the call this encoder is for
    /// * `source_config` - Audio configuration from the capture device (microphone)
    /// * `codec_type` - Codec to use for encoding
    pub fn new(call_id: Uuid, source_config: AudioConfig, codec_type: CodecType) -> Self {
        let (tx, frame_rx) = mpsc::channel(Self::BUFFER_SIZE);
        let (packet_tx, rx) = mpsc::channel(Self::BUFFER_SIZE);
        let rx = TokioMutex::new(rx);
        let sent_frames = Arc::new(AtomicU64::new(0));
        let received_packets = Arc::new(AtomicU64::new(0));
        let sent_bytes = Arc::new(AtomicU64::new(0));
        let received_bytes = Arc::new(AtomicU64::new(0));
        let task = tokio::spawn(Self::main_loop(
            call_id,
            source_config,
            codec_type,
            packet_tx,
            frame_rx,
            sent_frames.clone(),
            received_packets.clone(),
            sent_bytes.clone(),
            received_bytes.clone(),
        ));
        Self {
            tx,
            rx,
            sent_frames,
            received_packets,
            sent_bytes,
            received_bytes,
            task,
        }
    }

    /// Send an audio frame for encoding
    pub async fn send_frame(
        &self,
        frame: AudioFrame,
    ) -> Result<(), mpsc::error::SendError<AudioFrame>> {
        self.tx.send(frame).await
    }

    /// Receive an encoded packet
    pub async fn recv_packet(&self) -> Option<AudioDataPacket> {
        self.rx.lock().await.recv().await
    }

    async fn main_loop(
        call_id: Uuid,
        source_config: AudioConfig,
        codec_type: CodecType,
        tx: mpsc::Sender<AudioDataPacket>,
        mut rx: mpsc::Receiver<AudioFrame>,
        sent_frames: Arc<AtomicU64>,
        received_packets: Arc<AtomicU64>,
        sent_bytes: Arc<AtomicU64>,
        received_bytes: Arc<AtomicU64>,
    ) {
        // Create codec encoder
        // TEMPORARY: Force mono to debug audio issues
        let target_channels = 1; // Force mono
        tracing::warn!(
            "Encoder: source has {} channels, forcing {} channels for codec",
            source_config.channels,
            target_channels
        );
        let mut encoder = match create_encoder(codec_type, target_channels) {
            Ok(enc) => enc,
            Err(e) => {
                tracing::error!("Failed to create encoder: {}", e);
                return;
            }
        };

        let codec_config = encoder.codec_config();
        tracing::info!(
            "Encoder initialized: source={}Hz/{}ch, codec={}Hz/{}ch",
            source_config.sample_rate,
            source_config.channels,
            codec_config.sample_rate,
            codec_config.channels
        );

        // Create resampler if needed
        let mut resampler = if source_config.sample_rate != codec_config.sample_rate {
            match Resampler::new(
                source_config.sample_rate,
                codec_config.sample_rate,
                source_config.channels,
            ) {
                Ok(r) => Some(r),
                Err(e) => {
                    tracing::error!("Failed to create resampler: {}", e);
                    return;
                }
            }
        } else {
            None
        };

        // Buffer for accumulating samples until we have enough for codec
        // Codec expects 20ms frames
        let codec_frame_size =
            (codec_config.sample_rate as usize * 20 / 1000) * codec_config.channels as usize;
        let mut sample_buffer = Vec::with_capacity(codec_frame_size * 2);

        let mut sequence: u32 = 0;

        while let Some(frame) = rx.recv().await {
            sent_frames.fetch_add(1, Ordering::Relaxed);
            received_bytes.fetch_add((frame.samples.len() * 4) as u64, Ordering::Relaxed);

            // Convert channels if needed
            let mut samples = if source_config.channels > codec_config.channels {
                // Downmix (e.g., stereo to mono)
                tracing::trace!(
                    "Downmixing {} -> {} channels, {} samples",
                    source_config.channels,
                    codec_config.channels,
                    frame.samples.len()
                );
                downmix_to_mono(&frame.samples, source_config.channels)
            } else if source_config.channels < codec_config.channels {
                // Upmix (e.g., mono to stereo)
                tracing::trace!(
                    "Upmixing {} -> {} channels, {} samples",
                    source_config.channels,
                    codec_config.channels,
                    frame.samples.len()
                );
                upmix_to_stereo(&frame.samples)
            } else {
                // Channels match, no conversion needed
                frame.samples
            };

            // Resample if needed
            if let Some(ref mut resampler) = resampler {
                samples = match resampler.resample(&samples) {
                    Ok(resampled) => resampled,
                    Err(e) => {
                        tracing::error!("Resampling failed: {}", e);
                        continue;
                    }
                };
            }

            // Add to buffer
            sample_buffer.extend_from_slice(&samples);

            // Encode complete frames
            while sample_buffer.len() >= codec_frame_size {
                let frame_samples: Vec<f32> = sample_buffer.drain(..codec_frame_size).collect();

                // Encode
                let encoded = match encoder.encode(&frame_samples) {
                    Ok(data) => data,
                    Err(e) => {
                        tracing::error!("Encoding failed: {}", e);
                        // Send silence packet to maintain sequence
                        vec![0u8; 4]
                    }
                };

                // Get current timestamp in microseconds
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_micros() as u64;

                // Create packet
                let packet = AudioDataPacket {
                    call_id,
                    sequence,
                    timestamp,
                    codec: codec_type,
                    channels: codec_config.channels,
                    data: encoded.clone(),
                };

                sent_bytes.fetch_add(encoded.len() as u64, Ordering::Relaxed);
                received_packets.fetch_add(1, Ordering::Relaxed);

                // Send packet
                if tx.send(packet).await.is_err() {
                    tracing::debug!("Encoder packet receiver dropped");
                    return;
                }

                sequence = sequence.wrapping_add(1);
            }
        }

        tracing::debug!("Encoder main loop ended");
    }

    pub fn stats(&self) -> EncoderStats {
        EncoderStats {
            sent_frames: self.sent_frames.load(Ordering::Relaxed),
            received_packets: self.received_packets.load(Ordering::Relaxed),
            sent_bytes: self.sent_bytes.load(Ordering::Relaxed),
            received_bytes: self.received_bytes.load(Ordering::Relaxed),
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug)]
pub struct EncoderStats {
    pub sent_frames: u64,
    pub received_packets: u64,
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
