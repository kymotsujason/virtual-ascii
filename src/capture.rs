use std::thread;
use std::time::Duration;

use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution};
use nokhwa::Camera;

pub fn requested_format(resolution: Option<(u32, u32)>, fps: u32) -> RequestedFormat<'static> {
    // Default to 1920x1080 (16:9). AbsoluteHighestFrameRate picks by pixel count
    // on tie, which selects 4:3 (e.g. 1920x1440) over 16:9 on many cameras.
    let (w, h) = resolution.unwrap_or((1920, 1080));
    let fmt_type = RequestedFormatType::Closest(CameraFormat::new(
        Resolution::new(w, h),
        FrameFormat::MJPEG,
        fps,
    ));
    RequestedFormat::new::<RgbFormat>(fmt_type)
}

pub struct WebcamCapture {
    camera: Camera,
    width: u32,
    height: u32,
}

impl WebcamCapture {
    pub fn new(device_index: u32, resolution: Option<(u32, u32)>, fps: u32) -> anyhow::Result<Self> {
        Self::open_with_retries(device_index, resolution, fps, 3)
    }

    fn open_with_retries(
        device_index: u32,
        resolution: Option<(u32, u32)>,
        fps: u32,
        max_attempts: u32,
    ) -> anyhow::Result<Self> {
        let index = CameraIndex::Index(device_index);
        let mut last_err = None;

        for attempt in 0..max_attempts {
            if attempt > 0 {
                // Exponential backoff: 200ms, 500ms
                let delay = if attempt == 1 { 200 } else { 500 };
                thread::sleep(Duration::from_millis(delay));
            }

            let format = requested_format(resolution, fps);
            match Camera::new(index.clone(), format) {
                Ok(mut camera) => {
                    match camera.open_stream() {
                        Ok(()) => {
                            let cam_format = camera.camera_format();
                            let width = cam_format.resolution().width_x;
                            let height = cam_format.resolution().height_y;
                            return Ok(WebcamCapture {
                                camera,
                                width,
                                height,
                            });
                        }
                        Err(e) => {
                            last_err = Some(format!("Failed to start camera stream: {}", e));
                        }
                    }
                }
                Err(e) => {
                    last_err = Some(match e {
                        nokhwa::NokhwaError::OpenDeviceError(ref s, _) => {
                            format!(
                                "Cannot open camera index {}. {}.\n\
                                 Hint: Check that a webcam is connected and you have permission to access it.\n\
                                 Try: ls /dev/video*",
                                device_index, s
                            )
                        }
                        _ => {
                            let base =
                                format!("Failed to open camera index {}: {}", device_index, e);
                            if let Some((w, h)) = resolution {
                                format!(
                                    "{}\n\
                                     Hint: Camera may not support {}x{}.\n\
                                     Try: v4l2-ctl --list-formats-ext -d /dev/video{}",
                                    base, w, h, device_index
                                )
                            } else {
                                base
                            }
                        }
                    });
                }
            }
        }

        Err(anyhow::anyhow!(last_err.unwrap_or_else(|| {
            "Failed to open camera".to_string()
        })))
    }

    /// Capture a single frame, decoded to RGB24
    pub fn capture_frame(&mut self) -> anyhow::Result<Vec<u8>> {
        let buffer = self
            .camera
            .frame()
            .map_err(|e| anyhow::anyhow!("Frame capture failed: {}", e))?;

        let image = buffer
            .decode_image::<RgbFormat>()
            .map_err(|e| anyhow::anyhow!("Frame decode failed: {}", e))?;

        Ok(image.into_raw())
    }

    pub fn stop_stream(&mut self) {
        let _ = self.camera.stop_stream();
    }

    pub fn resolution(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}
