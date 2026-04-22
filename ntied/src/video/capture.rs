//! Screen capture. Produces BGRA `VideoFrame`s from a monitor and
//! delivers them on a bounded mpsc channel — if the consumer falls
//! behind, the oldest queued frame is dropped so latency stays bounded.
//!
//! Cross-platform surface with a Windows backend wired through
//! `windows-capture` (Graphics Capture API, `IDirect3D11CaptureFrame`).
//! Non-Windows targets compile but return `Unsupported`.

use std::io;
use std::time::Instant;

use tokio::sync::mpsc;

use super::frame::VideoFrame;

/// Buffered frames between the capture backend and the consumer. Larger
/// queues trade latency for burst tolerance; 4 is enough to absorb a few
/// missed wake-ups without the consumer ever rendering 100+ ms stale.
const FRAME_QUEUE: usize = 4;

pub struct ScreenCaptureStream {
    frames: mpsc::Receiver<VideoFrame>,
    #[cfg(windows)]
    _control: windows_backend::CaptureControlHandle,
}

/// Pixel size of the primary monitor, in even values (rounded down to
/// the nearest multiple of 2 — the encoder rejects odd dimensions).
#[derive(Clone, Copy, Debug)]
pub struct MonitorSize {
    pub width: u32,
    pub height: u32,
}

impl MonitorSize {
    pub fn primary() -> io::Result<Self> {
        #[cfg(windows)]
        {
            windows_backend::primary_size()
        }

        #[cfg(not(windows))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "monitor enumeration is only implemented on Windows",
            ))
        }
    }
}

impl ScreenCaptureStream {
    /// Start capturing the primary monitor. `target_fps` caps the frame
    /// rate the OS compositor delivers — set to 30 for screen share.
    pub fn start_primary_monitor(target_fps: u32) -> io::Result<Self> {
        #[cfg(windows)]
        {
            let (tx, frames) = mpsc::channel(FRAME_QUEUE);
            let control = windows_backend::start_primary(target_fps, tx)?;
            Ok(Self { frames, _control: control })
        }

        #[cfg(not(windows))]
        {
            let _ = target_fps;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "screen capture is only implemented on Windows",
            ))
        }
    }

    pub async fn recv(&mut self) -> Option<VideoFrame> {
        self.frames.recv().await
    }
}

#[cfg(windows)]
mod windows_backend {
    use std::time::Duration;

    use tokio::sync::mpsc;
    use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::monitor::Monitor;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };

    use super::*;

    /// Kept alive by `ScreenCaptureStream`; dropping it halts capture.
    pub struct CaptureControlHandle {
        control: Option<CaptureControl<Handler, HandlerError>>,
    }

    impl Drop for CaptureControlHandle {
        fn drop(&mut self) {
            if let Some(control) = self.control.take() {
                // `stop` posts a message to the capture thread; the
                // returned result is the handler's exit status which
                // we don't care about at teardown.
                let _ = control.stop();
            }
        }
    }

    pub(super) fn primary_size() -> io::Result<super::MonitorSize> {
        let monitor = Monitor::primary()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e}")))?;
        let width = monitor
            .width()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("width: {e}")))?;
        let height = monitor
            .height()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("height: {e}")))?;
        Ok(super::MonitorSize {
            width: width & !1,
            height: height & !1,
        })
    }

    pub(super) fn start_primary(
        target_fps: u32,
        tx: mpsc::Sender<VideoFrame>,
    ) -> io::Result<CaptureControlHandle> {
        let monitor = Monitor::primary()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e}")))?;

        let interval = if target_fps > 0 {
            MinimumUpdateIntervalSettings::Custom(Duration::from_secs(1) / target_fps)
        } else {
            MinimumUpdateIntervalSettings::Default
        };

        let settings = Settings::new(
            monitor,
            CursorCaptureSettings::WithCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            interval,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            HandlerFlags { tx },
        );

        let control = Handler::start_free_threaded(settings)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?;

        Ok(CaptureControlHandle { control: Some(control) })
    }

    pub(super) struct HandlerFlags {
        tx: mpsc::Sender<VideoFrame>,
    }

    pub(super) struct Handler {
        tx: mpsc::Sender<VideoFrame>,
    }

    #[derive(Debug)]
    pub(super) struct HandlerError(String);

    impl std::fmt::Display for HandlerError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl std::error::Error for HandlerError {}

    impl GraphicsCaptureApiHandler for Handler {
        type Flags = HandlerFlags;
        type Error = HandlerError;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            Ok(Self { tx: ctx.flags.tx })
        }

        fn on_frame_arrived(
            &mut self,
            frame: &mut Frame,
            _capture_control: InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            let captured_at = Instant::now();
            let mut buffer = frame
                .buffer()
                .map_err(|e| HandlerError(format!("frame buffer: {e}")))?;
            let width = buffer.width();
            let height = buffer.height();
            let stride = buffer.row_pitch();
            // Copy out of the D3D staging texture — the buffer is
            // unmapped when `FrameBuffer` drops at end of this call.
            let data = buffer.as_raw_buffer().to_vec();

            let frame = VideoFrame { width, height, stride, data, captured_at };

            // Bounded channel: drop on backpressure. try_send errors on
            // full or closed; both mean "no consumer can take this
            // frame right now", so the frame is simply dropped.
            let _ = self.tx.try_send(frame);
            Ok(())
        }

        fn on_closed(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

}
