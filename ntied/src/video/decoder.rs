//! Software H.264 decoder built on `openh264`. Consumes encoded NAL-unit
//! packets and produces BGRA `VideoFrame`s for rendering.

use std::io;
use std::time::Instant;

use openh264::decoder::Decoder;

use super::frame::VideoFrame;

pub struct VideoDecoder {
    inner: Decoder,
    /// Reused output scratch. Kept as RGBA (not BGRA) because iced's
    /// `image::Handle::from_rgba` expects RGBA and this is the only
    /// consumer of decoded frames today.
    rgba_scratch: Vec<u8>,
}

impl VideoDecoder {
    pub fn new() -> io::Result<Self> {
        let inner = Decoder::new()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("openh264 init: {e}")))?;
        Ok(Self { inner, rgba_scratch: Vec::new() })
    }

    /// Feed an encoded packet. Returns a decoded BGRA frame if one is
    /// ready — the first few packets of a stream often produce nothing
    /// while the decoder waits for SPS/PPS + IDR.
    pub fn decode(&mut self, packet: &[u8]) -> io::Result<Option<VideoFrame>> {
        let yuv = self
            .inner
            .decode(packet)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("decode: {e}")))?;

        let Some(yuv) = yuv else {
            return Ok(None);
        };

        let (w, h) = openh264::formats::YUVSource::dimensions(&yuv);
        let rgba_len = w * h * 4;
        self.rgba_scratch.resize(rgba_len, 0);
        yuv.write_rgba8(&mut self.rgba_scratch);

        Ok(Some(VideoFrame {
            width: w as u32,
            height: h as u32,
            stride: (w as u32) * 4,
            data: self.rgba_scratch.clone(),
            captured_at: Instant::now(),
        }))
    }
}
