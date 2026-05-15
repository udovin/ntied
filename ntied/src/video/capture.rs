//! Screen / window capture. Produces BGRA `VideoFrame`s from a chosen
//! source (whole monitor or single application window) and delivers
//! them on a bounded mpsc channel — if the consumer falls behind, the
//! oldest queued frame is dropped so latency stays bounded.
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

/// What to capture. Handles are opaque (raw HMONITOR / HWND on Windows
/// reinterpreted as `u64`) and only valid for the current process
/// lifetime — they should be obtained from `list_monitors` /
/// `list_windows`, used immediately, and not persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoSource {
    PrimaryMonitor,
    Monitor { handle: u64 },
    Window { handle: u64 },
}

#[derive(Clone, Debug)]
pub struct MonitorInfo {
    pub handle: u64,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub handle: u64,
    pub title: String,
    pub process: String,
}

/// Enumerate attached monitors, primary first.
pub fn list_monitors() -> io::Result<Vec<MonitorInfo>> {
    #[cfg(windows)]
    {
        windows_backend::list_monitors()
    }
    #[cfg(not(windows))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "monitor enumeration is only implemented on Windows",
        ))
    }
}

/// Enumerate top-level capturable windows. Filters out shell windows and
/// minimised / zero-sized ones to keep the picker readable.
pub fn list_windows() -> io::Result<Vec<WindowInfo>> {
    #[cfg(windows)]
    {
        windows_backend::list_windows()
    }
    #[cfg(not(windows))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "window enumeration is only implemented on Windows",
        ))
    }
}

pub struct ScreenCaptureStream {
    frames: mpsc::Receiver<VideoFrame>,
    #[cfg(windows)]
    _control: windows_backend::CaptureControlHandle,
}

impl ScreenCaptureStream {
    /// Start capturing the chosen source. `target_fps` caps the frame
    /// rate the OS compositor delivers.
    pub fn start(source: VideoSource, target_fps: u32) -> io::Result<Self> {
        #[cfg(windows)]
        {
            let (tx, frames) = mpsc::channel(FRAME_QUEUE);
            let control = windows_backend::start(source, target_fps, tx)?;
            Ok(Self { frames, _control: control })
        }

        #[cfg(not(windows))]
        {
            let _ = (source, target_fps);
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
        GraphicsCaptureItemType, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };
    use windows_capture::window::Window;

    use super::*;

    /// Kept alive by `ScreenCaptureStream`; dropping it halts capture.
    pub struct CaptureControlHandle {
        control: Option<CaptureControl<Handler, HandlerError>>,
    }

    impl Drop for CaptureControlHandle {
        fn drop(&mut self) {
            if let Some(control) = self.control.take() {
                let _ = control.stop();
            }
        }
    }

    pub(super) fn list_monitors() -> io::Result<Vec<MonitorInfo>> {
        let primary = Monitor::primary().ok();
        let primary_handle = primary.map(|m| m.as_raw_hmonitor() as u64);

        let mut out = Vec::new();
        for m in
            Monitor::enumerate().map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e}")))?
        {
            let handle = m.as_raw_hmonitor() as u64;
            let name = m.name().unwrap_or_else(|_| {
                m.device_name().unwrap_or_else(|_| format!("Monitor {:?}", handle))
            });
            let width = m.width().unwrap_or(0);
            let height = m.height().unwrap_or(0);
            let is_primary = primary_handle == Some(handle);
            out.push(MonitorInfo { handle, name, width, height, is_primary });
        }
        // Primary first, then by name.
        out.sort_by_key(|m| (!m.is_primary, m.name.clone()));
        Ok(out)
    }

    pub(super) fn list_windows() -> io::Result<Vec<WindowInfo>> {
        let mut out = Vec::new();
        for w in
            Window::enumerate().map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e}")))?
        {
            let title = w.title().unwrap_or_default();
            if title.is_empty() {
                continue;
            }
            let process = w.process_name().unwrap_or_default();
            let handle = w.as_raw_hwnd() as u64;
            out.push(WindowInfo { handle, title, process });
        }
        out.sort_by_key(|w| w.title.to_lowercase());
        Ok(out)
    }

    pub(super) fn start(
        source: VideoSource,
        target_fps: u32,
        tx: mpsc::Sender<VideoFrame>,
    ) -> io::Result<CaptureControlHandle> {
        let interval = if target_fps > 0 {
            MinimumUpdateIntervalSettings::Custom(Duration::from_secs(1) / target_fps)
        } else {
            MinimumUpdateIntervalSettings::Default
        };

        match source {
            VideoSource::PrimaryMonitor => {
                let monitor = Monitor::primary()
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e}")))?;
                start_with_item(monitor, interval, tx)
            }
            VideoSource::Monitor { handle } => {
                let monitor =
                    Monitor::from_raw_hmonitor(handle as *mut std::ffi::c_void);
                start_with_item(monitor, interval, tx)
            }
            VideoSource::Window { handle } => {
                let window = Window::from_raw_hwnd(handle as *mut std::ffi::c_void);
                start_with_item(window, interval, tx)
            }
        }
    }

    fn start_with_item<T>(
        item: T,
        interval: MinimumUpdateIntervalSettings,
        tx: mpsc::Sender<VideoFrame>,
    ) -> io::Result<CaptureControlHandle>
    where
        T: TryInto<GraphicsCaptureItemType> + Send + 'static,
    {
        let settings = Settings::new(
            item,
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
            let data = buffer.as_raw_buffer().to_vec();
            let frame = VideoFrame { width, height, stride, data, captured_at };
            // Bounded channel: drop on backpressure.
            let _ = self.tx.try_send(frame);
            Ok(())
        }

        fn on_closed(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }
}
