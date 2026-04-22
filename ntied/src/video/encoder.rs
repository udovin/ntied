//! Software H.264 encoder built on `openh264`. Converts BGRA frames to
//! a compressed NAL-unit bitstream with a periodic IDR (keyframe) so
//! that a lost P-frame never desynchronises the decoder for longer
//! than `idr_interval`.

use std::io;
use std::time::Duration;

use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig as OhEncoderConfig, FrameRate, IntraFramePeriod,
    RateControlMode, UsageType,
};
use openh264::formats::{RgbSliceU8, YUVBuffer};

use super::frame::VideoFrame;

#[derive(Clone, Debug)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    pub target_bitrate: u32,
    pub idr_interval: Duration,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            framerate: 30,
            target_bitrate: 2_000_000,
            idr_interval: Duration::from_secs(2),
        }
    }
}

/// Feeds raw BGRA frames in, emits encoded NAL-unit bytes out.
pub struct VideoEncoder {
    inner: Encoder,
    /// Soft caps from `EncoderConfig` — captured frames larger than
    /// this are downscaled by an integer ratio that brings both axes
    /// under the limits. Avoids hitting openh264's hard 3840x2160 cap.
    max_width: u32,
    max_height: u32,
    /// Reusable tight RGB scratch (`stride == width * 3`). Filled by
    /// the downscale-and-color-swap loop. Tight RGB lets us hit
    /// `from_rgb8_source` (integer scalar YUV conversion) instead of
    /// the slow `from_rgb_source` path that goes through f32 per pixel.
    tight_rgb: Vec<u8>,
}

impl VideoEncoder {
    pub fn new(config: EncoderConfig) -> io::Result<Self> {
        let frames_per_idr = (config.idr_interval.as_secs_f32()
            * config.framerate as f32)
            .round()
            .max(1.0) as u32;

        let oh = OhEncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .max_frame_rate(FrameRate::from_hz(config.framerate as f32))
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(config.target_bitrate))
            .intra_frame_period(IntraFramePeriod::from_num_frames(frames_per_idr))
            .skip_frames(true);

        let api = OpenH264API::from_source();
        let inner = Encoder::with_api_config(api, oh)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("openh264 init: {e}")))?;

        Ok(Self {
            inner,
            max_width: config.width,
            max_height: config.height,
            tight_rgb: Vec::new(),
        })
    }

    /// Encode one frame. Returns the encoded NAL-unit bytes, or `None`
    /// if the encoder chose to skip the frame (e.g. rate control).
    pub fn encode(&mut self, frame: &VideoFrame) -> io::Result<Option<Vec<u8>>> {
        // Pick the smallest integer downscale factor that brings both
        // axes under `max_width × max_height`. Integer ratio keeps the
        // sampler trivial (nearest-neighbour) and means even
        // dimensions stay even, satisfying openh264.
        let scale = downscale_factor(frame.width, frame.height, self.max_width, self.max_height);
        let target_w = ((frame.width / scale) & !1) as usize;
        let target_h = ((frame.height / scale) & !1) as usize;
        tracing::debug!(
            src_w = frame.width,
            src_h = frame.height,
            max_w = self.max_width,
            max_h = self.max_height,
            scale,
            target_w,
            target_h,
            "encode dims"
        );
        if target_w == 0 || target_h == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "frame {}x{} downscaled to zero size",
                    frame.width, frame.height
                ),
            ));
        }

        // Materialise a tight RGB buffer (3 bytes per pixel, no
        // alpha) at target dimensions in one pass: nearest-neighbour
        // downscale + BGRA→RGB byte-swap. RGB feeds the fast
        // `from_rgb8_source` path; the alternative (BGRA + scalar
        // `from_rgb_source`) goes through f32 arithmetic per pixel
        // and is the dominant cost on a full-screen capture.
        let row_bytes = target_w * 3;
        self.tight_rgb.clear();
        self.tight_rgb.resize(row_bytes * target_h, 0);
        let src_stride = frame.stride as usize;
        let src_w = frame.width as usize;
        let src_h = frame.height as usize;
        for dy in 0..target_h {
            let sy = dy * src_h / target_h;
            let src_row = sy * src_stride;
            let dst_row = dy * row_bytes;
            for dx in 0..target_w {
                let sx = dx * src_w / target_w;
                let s = src_row + sx * 4;
                let d = dst_row + dx * 3;
                // BGRA → RGB
                self.tight_rgb[d] = frame.data[s + 2];
                self.tight_rgb[d + 1] = frame.data[s + 1];
                self.tight_rgb[d + 2] = frame.data[s];
            }
        }

        let rgb = RgbSliceU8::new(&self.tight_rgb, (target_w, target_h));
        let yuv = YUVBuffer::from_rgb8_source(rgb);

        let bs = self
            .inner
            .encode(&yuv)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("encode: {e}")))?;

        let bytes = bs.to_vec();
        Ok(if bytes.is_empty() { None } else { Some(bytes) })
    }

    /// Force the next encoded frame to be an IDR (keyframe). Called when
    /// the remote side signals it lost sync, or on encoder start.
    pub fn request_keyframe(&mut self) {
        self.inner.force_intra_frame();
    }
}

/// Smallest integer N such that `src_w / N <= max_w` and `src_h / N <= max_h`.
/// Returns at least 1 (no upscaling).
fn downscale_factor(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> u32 {
    let by_w = src_w.div_ceil(max_w.max(1));
    let by_h = src_h.div_ceil(max_h.max(1));
    by_w.max(by_h).max(1)
}
