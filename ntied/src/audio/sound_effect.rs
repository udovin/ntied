//! Short, fire-and-forget UI sound effects (notifications).
//!
//! WAV files are bundled into the binary via `include_bytes!`. They live
//! in `assets/sounds/` and originate from
//! [akx/Notifications](https://github.com/akx/Notifications) under
//! CC0 / CC-BY 3.0 (see `assets/sounds/NOTICES.md`).
//!
//! Each `play(kind)` call spawns a blocking task that opens its own short
//! `cpal::Stream`, plays once, and exits. The same approach the ringtone
//! uses — verified to coexist with other concurrent streams in the
//! process.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, StreamConfig};
use tokio::task::spawn_blocking;

#[derive(Clone, Copy, Debug)]
pub enum SoundKind {
    /// New chat message arrived (and the chat isn't open).
    NewMessage,
    /// A peer was added (contact request accepted).
    PeerAdded,
    /// Outgoing call was accepted and audio is now flowing.
    CallConnected,
    /// The current call ended (either side hung up or connection died).
    CallEnded,
    /// An outgoing call was rejected by the peer.
    CallRejected,
}

const NEW_MESSAGE_WAV: &[u8] = include_bytes!("../../assets/sounds/new_message.wav");
const PEER_ADDED_WAV: &[u8] = include_bytes!("../../assets/sounds/peer_added.wav");
const CALL_CONNECTED_WAV: &[u8] = include_bytes!("../../assets/sounds/call_connected.wav");
const CALL_ENDED_WAV: &[u8] = include_bytes!("../../assets/sounds/call_ended.wav");
const CALL_REJECTED_WAV: &[u8] = include_bytes!("../../assets/sounds/call_rejected.wav");

fn wav_bytes(kind: SoundKind) -> &'static [u8] {
    match kind {
        SoundKind::NewMessage => NEW_MESSAGE_WAV,
        SoundKind::PeerAdded => PEER_ADDED_WAV,
        SoundKind::CallConnected => CALL_CONNECTED_WAV,
        SoundKind::CallEnded => CALL_ENDED_WAV,
        SoundKind::CallRejected => CALL_REJECTED_WAV,
    }
}

/// Fire-and-forget: spawn a blocking task that opens the default output
/// device, plays the short tone for `kind`, and exits. Errors are logged.
pub fn play(kind: SoundKind) {
    spawn_blocking(move || {
        if let Err(err) = play_blocking(kind) {
            tracing::warn!(?kind, ?err, "sound effect failed");
        }
    });
}

fn play_blocking(kind: SoundKind) -> Result<()> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("no output device"))?;
    let config = device
        .default_output_config()
        .map_err(|e| anyhow!("default output config: {e}"))?;
    let sample_format = config.sample_format();
    let stream_config: StreamConfig = config.into();
    let sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;

    let samples = samples_for(kind, sample_rate, channels);
    let total = samples.len();
    let cursor = Arc::new(std::sync::Mutex::new(0usize));

    let stream = match sample_format {
        SampleFormat::I8 => build::<i8>(&device, &stream_config, samples, cursor),
        SampleFormat::I16 => build::<i16>(&device, &stream_config, samples, cursor),
        SampleFormat::I32 => build::<i32>(&device, &stream_config, samples, cursor),
        SampleFormat::I64 => build::<i64>(&device, &stream_config, samples, cursor),
        SampleFormat::U8 => build::<u8>(&device, &stream_config, samples, cursor),
        SampleFormat::U16 => build::<u16>(&device, &stream_config, samples, cursor),
        SampleFormat::U32 => build::<u32>(&device, &stream_config, samples, cursor),
        SampleFormat::U64 => build::<u64>(&device, &stream_config, samples, cursor),
        SampleFormat::F32 => build::<f32>(&device, &stream_config, samples, cursor),
        SampleFormat::F64 => build::<f64>(&device, &stream_config, samples, cursor),
        _ => return Err(anyhow!("unsupported sample format {:?}", sample_format)),
    }?;
    stream.play().map_err(|e| anyhow!("stream play: {e}"))?;

    // Sleep for the audio's real duration plus a tail to let cpal flush its
    // own output buffer before we drop the stream. Polling the cursor isn't
    // reliable: it advances when samples are written into cpal's buffer,
    // not when the device has actually played them.
    let frames = (total / channels.max(1)).max(1);
    let duration = std::time::Duration::from_secs_f64(frames as f64 / sample_rate as f64);
    std::thread::sleep(duration + std::time::Duration::from_millis(300));
    Ok(())
}

fn build<T>(
    device: &Device,
    config: &StreamConfig,
    samples: Vec<f32>,
    cursor: Arc<std::sync::Mutex<usize>>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let total = samples.len();
    let data_fn = move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
        let mut pos = cursor.lock().unwrap();
        for slot in data.iter_mut() {
            if *pos < total {
                *slot = T::from_sample(samples[*pos]);
                *pos += 1;
            } else {
                *slot = T::from_sample(0.0);
            }
        }
    };
    let err_fn = |err| tracing::warn!(?err, "sound effect stream error");
    device
        .build_output_stream(config, data_fn, err_fn, None)
        .map_err(|e| anyhow!("build_output_stream: {e}"))
}

/// Decode the bundled WAV, then resample / channel-duplicate to match the
/// playback device. Returns interleaved f32 samples ready for cpal.
fn samples_for(kind: SoundKind, target_rate: u32, target_channels: usize) -> Vec<f32> {
    let bytes = wav_bytes(kind);
    let (mono, src_rate) = match decode_pcm16_mono(bytes) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?kind, ?err, "WAV decode failed, falling back to silence");
            return vec![0.0; target_rate as usize / 10 * target_channels];
        }
    };
    let resampled = resample_linear(&mono, src_rate, target_rate);
    interleave_mono(&resampled, target_channels)
}

/// Minimal RIFF/WAVE parser for mono audio. Supports PCM16 (`fmt=1,
/// bits=16`) and IEEE float32 (`fmt=3, bits=32`) — the two formats akx's
/// notification WAVs actually ship in. Returns `(samples in [-1, 1],
/// sample_rate)`.
pub(super) fn decode_pcm16_mono(buf: &[u8]) -> Result<(Vec<f32>, u32)> {
    if buf.len() < 44 || &buf[0..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
        return Err(anyhow!("not a RIFF/WAVE file"));
    }
    let mut i = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<&[u8]> = None;
    while i + 8 <= buf.len() {
        let id = &buf[i..i + 4];
        let size = u32::from_le_bytes([buf[i + 4], buf[i + 5], buf[i + 6], buf[i + 7]]) as usize;
        let payload_start = i + 8;
        let payload_end = payload_start + size;
        if payload_end > buf.len() {
            break;
        }
        match id {
            b"fmt " => {
                if size < 16 {
                    return Err(anyhow!("fmt chunk too small"));
                }
                let format = u16::from_le_bytes([buf[payload_start], buf[payload_start + 1]]);
                let channels =
                    u16::from_le_bytes([buf[payload_start + 2], buf[payload_start + 3]]);
                let sample_rate = u32::from_le_bytes([
                    buf[payload_start + 4],
                    buf[payload_start + 5],
                    buf[payload_start + 6],
                    buf[payload_start + 7],
                ]);
                let bits =
                    u16::from_le_bytes([buf[payload_start + 14], buf[payload_start + 15]]);
                fmt = Some((format, channels, sample_rate, bits));
            }
            b"data" => {
                data = Some(&buf[payload_start..payload_end]);
            }
            _ => {}
        }
        i = payload_end + (size & 1);
    }
    let (format, channels, sample_rate, bits) = fmt.ok_or_else(|| anyhow!("missing fmt chunk"))?;
    let data = data.ok_or_else(|| anyhow!("missing data chunk"))?;
    if channels != 1 {
        return Err(anyhow!(
            "unsupported WAV: format={format} channels={channels} bits={bits} (need mono)"
        ));
    }
    let samples = match (format, bits) {
        (1, 16) => data
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect(),
        (3, 32) => data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        _ => {
            return Err(anyhow!(
                "unsupported WAV: format={format} bits={bits} (need PCM16 or IEEE float32)"
            ));
        }
    };
    Ok((samples, sample_rate))
}

pub(super) fn resample_linear(input: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if src_rate == dst_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = src_rate as f64 / dst_rate as f64;
    let out_len = ((input.len() as f64) / ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for n in 0..out_len {
        let src_pos = n as f64 * ratio;
        let i = src_pos as usize;
        let frac = (src_pos - i as f64) as f32;
        let a = input[i];
        let b = if i + 1 < input.len() { input[i + 1] } else { a };
        out.push(a + (b - a) * frac);
    }
    out
}

pub(super) fn interleave_mono(mono: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return mono.to_vec();
    }
    let mut out = Vec::with_capacity(mono.len() * channels);
    for &s in mono {
        for _ in 0..channels {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_wavs_decode() {
        for kind in [
            SoundKind::NewMessage,
            SoundKind::PeerAdded,
            SoundKind::CallConnected,
            SoundKind::CallEnded,
            SoundKind::CallRejected,
        ] {
            let (samples, rate) =
                decode_pcm16_mono(wav_bytes(kind)).expect("bundled WAV must decode");
            assert!(!samples.is_empty(), "{kind:?} decoded to empty");
            assert!(rate > 0, "{kind:?} reports zero rate");
        }
    }

    #[test]
    fn resample_identity() {
        let s = vec![0.1, 0.2, 0.3, 0.4];
        assert_eq!(resample_linear(&s, 48000, 48000), s);
    }

    #[test]
    fn resample_changes_length() {
        let s: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
        let up = resample_linear(&s, 22050, 44100);
        assert!(up.len() > s.len(), "upsample should grow");
        let down = resample_linear(&s, 44100, 22050);
        assert!(down.len() < s.len(), "downsample should shrink");
    }

    #[test]
    fn interleave_doubles_for_stereo() {
        let mono = vec![0.5, -0.5];
        assert_eq!(interleave_mono(&mono, 2), vec![0.5, 0.5, -0.5, -0.5]);
    }
}
