//! Screen video capture, encode, decode pipeline. Mirrors the `audio`
//! module: capture → encoder → datagram network → decoder → render.
//!
//! Only the primary monitor is supported for now; window / region
//! selection lands in later phases.

pub mod capture;
pub mod decoder;
pub mod encoder;
pub mod frame;

pub use capture::{
    MonitorInfo, ScreenCaptureStream, VideoSource, WindowInfo, list_monitors, list_windows,
};
pub use decoder::VideoDecoder;
pub use encoder::VideoEncoder;
pub use frame::VideoFrame;
