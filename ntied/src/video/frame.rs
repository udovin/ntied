use std::time::Instant;

/// A raw captured or decoded video frame in BGRA8 layout (the native
/// format on Windows Graphics Capture and the one iced's `image::Handle`
/// expects). `stride` is the row length in bytes as reported by the
/// capture/decoder — may exceed `width * 4` for alignment.
#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub data: Vec<u8>,
    pub captured_at: Instant,
}
