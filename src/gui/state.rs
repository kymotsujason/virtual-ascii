use crate::config::{self, BrightnessCurve, ColorTheme, Rgb};
use crate::detect;

#[derive(Clone, Copy, PartialEq)]
pub enum ViewMode {
    SideBySide,
    RawOnly,
    AsciiOnly,
}

pub struct GuiState {
    // Camera settings
    pub camera_index: u32,
    pub resolution_index: usize,
    pub available_resolutions: Vec<String>,
    pub fps: u32,
    pub max_fps: u32,

    // Appearance settings
    pub theme_name: String,
    pub fg_color: [u8; 3],
    pub bg_color: [u8; 3],
    pub definition: u8,
    pub brightness_curve_name: String,
    pub invert: bool,

    // Output settings
    pub output_device: String,

    // GUI-specific state
    pub pipeline_running: bool,
    pub v4l2_output_active: bool,
    pub v4l2loopback_loaded: bool,
    pub detected_cameras: Vec<detect::CameraInfo>,
    pub status_message: String,
    pub camera_conflict: Option<String>,

    // Preview
    pub view_mode: ViewMode,

    // Dirty tracking for settings changes
    pub capture_dirty: bool,
    pub render_dirty: bool,
    pub last_change_time: Option<std::time::Instant>,
}

impl GuiState {
    pub fn new() -> Self {
        let theme = ColorTheme::from_name("matrix").unwrap();
        let detected_cameras = detect::list_cameras("/dev/video20");
        let camera_index = detected_cameras.first().map(|c| c.index).unwrap_or(0);

        let available_resolutions = Self::build_resolution_list(camera_index);
        let max_fps = Self::detect_max_fps(camera_index, 0, &available_resolutions);

        Self {
            camera_index,
            resolution_index: 0,
            available_resolutions,
            fps: 30,
            max_fps,
            theme_name: "matrix".into(),
            fg_color: [theme.fg.r, theme.fg.g, theme.fg.b],
            bg_color: [theme.bg.r, theme.bg.g, theme.bg.b],
            definition: 5,
            brightness_curve_name: "linear".into(),
            invert: false,
            output_device: "/dev/video20".into(),
            pipeline_running: false,
            v4l2_output_active: false,
            v4l2loopback_loaded: false,
            detected_cameras,
            status_message: "Ready".into(),
            camera_conflict: None,
            view_mode: ViewMode::SideBySide,
            capture_dirty: false,
            render_dirty: false,
            last_change_time: None,
        }
    }

    pub fn fg_rgb(&self) -> Rgb {
        Rgb {
            r: self.fg_color[0],
            g: self.fg_color[1],
            b: self.fg_color[2],
        }
    }

    pub fn bg_rgb(&self) -> Rgb {
        Rgb {
            r: self.bg_color[0],
            g: self.bg_color[1],
            b: self.bg_color[2],
        }
    }

    pub fn brightness_curve(&self) -> BrightnessCurve {
        BrightnessCurve::from_name(&self.brightness_curve_name).unwrap_or(BrightnessCurve::Linear)
    }

    pub fn resolution(&self) -> Option<(u32, u32)> {
        let text = &self.available_resolutions[self.resolution_index];
        config::parse_resolution(text).ok()
    }

    pub fn refresh_cameras(&mut self) {
        self.detected_cameras = detect::list_cameras(&self.output_device);
    }

    /// Build the resolution dropdown list by querying V4L2 capabilities.
    fn build_resolution_list(camera_index: u32) -> Vec<String> {
        let mut list = vec!["Auto".to_string()];
        let resolutions = detect::list_resolutions(camera_index);
        for (w, h) in &resolutions {
            let label = match detect::max_fps_for_resolution(camera_index, *w, *h) {
                Some(fps) => format!("{}x{} ({}fps)", w, h, fps),
                None => format!("{}x{}", w, h),
            };
            list.push(label);
        }
        // Fallback: if V4L2 enumeration returned nothing, add common resolutions
        if resolutions.is_empty() {
            list.push("640x480".into());
            list.push("1280x720".into());
            list.push("1920x1080".into());
        }
        list
    }

    /// Re-query resolutions when the camera changes.
    pub fn refresh_resolutions(&mut self) {
        self.available_resolutions = Self::build_resolution_list(self.camera_index);
        self.resolution_index = 0; // Reset to "Auto"
        self.refresh_max_fps();
    }

    /// Update max_fps based on the currently selected resolution.
    pub fn refresh_max_fps(&mut self) {
        self.max_fps =
            Self::detect_max_fps(self.camera_index, self.resolution_index, &self.available_resolutions);
        if self.fps > self.max_fps {
            self.fps = self.max_fps;
        }
    }

    /// Detect max FPS for a given resolution selection.
    /// For "Auto" (index 0), returns the max across all resolutions.
    /// Falls back to 240 if detection returns nothing.
    fn detect_max_fps(camera_index: u32, resolution_index: usize, resolutions: &[String]) -> u32 {
        if resolution_index == 0 {
            // "Auto" mode: max FPS across all available resolutions
            let all_res = detect::list_resolutions(camera_index);
            let max = all_res
                .iter()
                .filter_map(|(w, h)| detect::max_fps_for_resolution(camera_index, *w, *h))
                .max();
            max.unwrap_or(240)
        } else if let Some(text) = resolutions.get(resolution_index) {
            if let Ok((w, h)) = config::parse_resolution(text) {
                detect::max_fps_for_resolution(camera_index, w, h).unwrap_or(240)
            } else {
                240
            }
        } else {
            240
        }
    }
}
