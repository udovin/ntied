use anyhow::{Context, Result};
use image::{ImageBuffer, Rgba};
use std::sync::Arc;
use tokio::sync::RwLock;
use xcap::{Monitor, Window};

/// Represents a source that can be captured
#[derive(Clone, Debug)]
pub enum CaptureSource {
    /// Capture the primary monitor
    PrimaryMonitor,
    /// Capture a specific monitor by ID
    Monitor(u32),
    /// Capture a specific window by ID
    Window(u32),
    /// Capture a specific area of the screen
    Area {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
}

/// Information about an available monitor
#[derive(Clone, Debug)]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

/// Information about an available window
#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app_name: String,
    pub width: u32,
    pub height: u32,
}

/// Screen capture manager
pub struct ScreenCapture {
    source: Arc<RwLock<Option<CaptureSource>>>,
    is_capturing: Arc<RwLock<bool>>,
}

impl ScreenCapture {
    /// Create a new screen capture manager
    pub fn new() -> Self {
        Self {
            source: Arc::new(RwLock::new(None)),
            is_capturing: Arc::new(RwLock::new(false)),
        }
    }

    /// Get list of available monitors
    pub async fn list_monitors() -> Result<Vec<MonitorInfo>> {
        let monitors = Monitor::all().context("Failed to enumerate monitors")?;

        let mut monitor_infos = Vec::new();
        for (idx, monitor) in monitors.iter().enumerate() {
            monitor_infos.push(MonitorInfo {
                id: idx as u32,
                name: monitor.name().to_string(),
                width: monitor.width(),
                height: monitor.height(),
                is_primary: monitor.is_primary(),
            });
        }

        Ok(monitor_infos)
    }

    /// Get list of available windows
    pub async fn list_windows() -> Result<Vec<WindowInfo>> {
        let windows = Window::all().context("Failed to enumerate windows")?;

        let mut window_infos = Vec::new();
        for (idx, window) in windows.iter().enumerate() {
            // Filter out windows with empty titles or very small dimensions
            if !window.title().is_empty() && window.width() > 100 && window.height() > 100 {
                window_infos.push(WindowInfo {
                    id: idx as u32,
                    title: window.title().to_string(),
                    app_name: window.app_name().to_string(),
                    width: window.width(),
                    height: window.height(),
                });
            }
        }

        Ok(window_infos)
    }

    /// Set the capture source
    pub async fn set_source(&self, source: CaptureSource) {
        let mut src = self.source.write().await;
        *src = Some(source);
    }

    /// Get the current capture source
    pub async fn get_source(&self) -> Option<CaptureSource> {
        self.source.read().await.clone()
    }

    /// Start capturing
    pub async fn start(&self) {
        let mut capturing = self.is_capturing.write().await;
        *capturing = true;
    }

    /// Stop capturing
    pub async fn stop(&self) {
        let mut capturing = self.is_capturing.write().await;
        *capturing = false;
    }

    /// Check if currently capturing
    pub async fn is_capturing(&self) -> bool {
        *self.is_capturing.read().await
    }

    /// Capture a single frame from the current source
    pub async fn capture_frame(&self) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
        let source = self.source.read().await;
        let source = source.as_ref().context("No capture source set")?;

        if !*self.is_capturing.read().await {
            anyhow::bail!("Capture is not active");
        }

        match source {
            CaptureSource::PrimaryMonitor => {
                let monitors = Monitor::all().context("Failed to get monitors")?;
                let primary = monitors
                    .into_iter()
                    .find(|m| m.is_primary())
                    .context("No primary monitor found")?;

                self.capture_monitor(&primary).await
            }
            CaptureSource::Monitor(id) => {
                let monitors = Monitor::all().context("Failed to get monitors")?;
                let monitor = monitors.get(*id as usize).context("Monitor not found")?;

                self.capture_monitor(monitor).await
            }
            CaptureSource::Window(id) => {
                let windows = Window::all().context("Failed to get windows")?;
                let window = windows.get(*id as usize).context("Window not found")?;

                self.capture_window(window).await
            }
            CaptureSource::Area {
                x,
                y,
                width,
                height,
            } => {
                // For area capture, we'll capture the primary monitor and crop
                let monitors = Monitor::all().context("Failed to get monitors")?;
                let primary = monitors
                    .into_iter()
                    .find(|m| m.is_primary())
                    .context("No primary monitor found")?;

                let full_image = self.capture_monitor(&primary).await?;
                self.crop_image(full_image, *x, *y, *width, *height)
            }
        }
    }

    /// Capture a monitor
    async fn capture_monitor(&self, monitor: &Monitor) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
        let image = monitor
            .capture_image()
            .context("Failed to capture monitor")?;
        Ok(image)
    }

    /// Capture a window
    async fn capture_window(&self, window: &Window) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
        let image = window.capture_image().context("Failed to capture window")?;
        Ok(image)
    }

    /// Crop an image to a specific area
    fn crop_image(
        &self,
        image: ImageBuffer<Rgba<u8>, Vec<u8>>,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
        let (img_width, img_height) = image.dimensions();

        // Validate coordinates
        if x < 0 || y < 0 || x as u32 + width > img_width || y as u32 + height > img_height {
            anyhow::bail!("Crop area out of bounds");
        }

        let cropped =
            image::imageops::crop_imm(&image, x as u32, y as u32, width, height).to_image();
        Ok(cropped)
    }

    /// Encode frame to JPEG bytes for transmission
    pub fn encode_frame_jpeg(
        frame: &ImageBuffer<Rgba<u8>, Vec<u8>>,
        quality: u8,
    ) -> Result<Vec<u8>> {
        use image::{ImageEncoder, codecs::jpeg::JpegEncoder};
        use std::io::Cursor;

        // Convert RGBA to RGB
        let rgb_image = image::DynamicImage::ImageRgba8(frame.clone()).to_rgb8();

        let mut buffer = Vec::new();
        let mut cursor = Cursor::new(&mut buffer);

        let encoder = JpegEncoder::new_with_quality(&mut cursor, quality);
        encoder
            .write_image(
                rgb_image.as_raw(),
                rgb_image.width(),
                rgb_image.height(),
                image::ExtendedColorType::Rgb8,
            )
            .context("Failed to encode image as JPEG")?;

        Ok(buffer)
    }

    /// Decode JPEG bytes to frame
    pub fn decode_frame_jpeg(data: &[u8]) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
        use image::ImageReader;
        use std::io::Cursor;

        let reader = ImageReader::new(Cursor::new(data))
            .with_guessed_format()
            .context("Failed to guess image format")?;

        let img = reader.decode().context("Failed to decode image")?;
        Ok(img.to_rgba8())
    }
}

impl Default for ScreenCapture {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_monitors() {
        let monitors = ScreenCapture::list_monitors().await;
        assert!(monitors.is_ok());
        let monitors = monitors.unwrap();
        assert!(
            !monitors.is_empty(),
            "At least one monitor should be available"
        );
    }

    #[tokio::test]
    async fn test_capture_lifecycle() {
        let capture = ScreenCapture::new();

        assert!(!capture.is_capturing().await);

        capture.start().await;
        assert!(capture.is_capturing().await);

        capture.stop().await;
        assert!(!capture.is_capturing().await);
    }

    #[tokio::test]
    async fn test_set_get_source() {
        let capture = ScreenCapture::new();

        assert!(capture.get_source().await.is_none());

        capture.set_source(CaptureSource::PrimaryMonitor).await;
        assert!(capture.get_source().await.is_some());
    }
}
