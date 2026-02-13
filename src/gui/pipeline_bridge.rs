use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crossbeam_channel::bounded;

use crate::config::definition_to_params;
use crate::control::{CaptureAction, CaptureCommand};
use crate::pipeline::Pipeline;
use crate::renderer::AsciiRenderer;

use super::app::VirtualAsciiApp;
use super::camera_check;

impl VirtualAsciiApp {
    /// Start the capture+render pipeline for GUI preview
    pub fn start_pipeline(&mut self) -> Result<(), String> {
        if self.state.pipeline_running {
            return Err("Pipeline already running".into());
        }

        // Check for camera conflicts
        if let Some(conflict) = camera_check::check_camera_busy(self.state.camera_index) {
            self.state.camera_conflict = Some(conflict.clone());
            return Err(format!("Camera busy: {}", conflict));
        }

        if camera_check::is_cli_instance_running() {
            self.state.status_message =
                "Warning: CLI instance detected, camera may conflict".into();
        }

        // Probe camera resolution
        let resolution = self.state.resolution();
        let (out_w, out_h) = crate::probe_camera_resolution(self.state.camera_index, resolution, self.state.fps)
            .map_err(|e| format!("Camera probe failed: {}", e))?;

        // Create renderer
        let (ascii_columns, charset) =
            definition_to_params(self.state.definition, &self.state.theme_name);
        let renderer = AsciiRenderer::new(
            &charset,
            self.state.fg_rgb(),
            self.state.bg_rgb(),
            self.state.brightness_curve(),
            self.state.invert,
            out_w,
            out_h,
            ascii_columns,
            &self.state.theme_name,
        )
        .map_err(|e| format!("Renderer init failed: {}", e))?;

        // Create channels
        let (capture_cmd_tx, capture_cmd_rx) = bounded(4);
        let (render_cmd_tx, render_cmd_rx) = bounded(4);
        let (gui_raw_tx, gui_raw_rx) = bounded(1);
        let (gui_rendered_tx, gui_rendered_rx) = bounded(1);

        // Create a fresh shutdown flag for this pipeline instance.
        // This ensures old pipeline threads (still draining in a background wait()
        // thread) cannot interfere with the new pipeline's shutdown flag.
        let pipeline_shutdown = Arc::new(AtomicBool::new(false));
        self.shutdown = pipeline_shutdown.clone();

        // Start pipeline
        let pipeline = Pipeline::start(
            self.state.camera_index,
            resolution,
            self.state.fps,
            renderer,
            None, // No v4l2 output initially
            self.shutdown.clone(),
            capture_cmd_rx,
            render_cmd_rx,
            Some(gui_raw_tx),
            Some(gui_rendered_tx),
        )
        .map_err(|e| format!("Pipeline start failed: {}", e))?;

        self.pipeline = Some(pipeline);
        self.capture_cmd_tx = Some(capture_cmd_tx);
        self.render_cmd_tx = Some(render_cmd_tx);
        self.gui_raw_rx = Some(gui_raw_rx);
        self.gui_rendered_rx = Some(gui_rendered_rx);
        self.state.pipeline_running = true;
        self.state.camera_conflict = None;
        self.state.status_message = format!("Camera preview active ({}x{}). Virtual camera not started.", out_w, out_h);

        Ok(())
    }

    /// Stop the pipeline
    pub fn stop_pipeline(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);

        // Drop channels to unblock pipeline threads
        self.capture_cmd_tx = None;
        self.render_cmd_tx = None;
        self.gui_raw_rx = None;
        self.gui_rendered_rx = None;

        // Wait for pipeline threads in background to avoid blocking GUI.
        // Pipeline::wait() logs any thread panics with payload extraction,
        // so panic detection is handled automatically.
        if let Some(pipeline) = self.pipeline.take() {
            std::thread::spawn(move || {
                pipeline.wait();
            });
        }

        self.state.pipeline_running = false;
        self.state.v4l2_output_active = false;
        self.state.status_message = "Stopped".into();

        // Clear preview textures
        self.raw_preview_texture = None;
        self.rendered_preview_texture = None;
    }

    /// Start v4l2 output (virtual camera) on existing pipeline
    pub fn start_v4l2_output(&mut self) -> Result<(), String> {
        if !self.state.pipeline_running {
            return Err("Pipeline not running".into());
        }
        if !self.state.v4l2loopback_loaded {
            return Err("v4l2loopback module not loaded".into());
        }
        if self.state.v4l2_output_active {
            return Err("Virtual camera already active".into());
        }

        let pipeline = self.pipeline.as_mut().ok_or("No pipeline")?;

        let v4l2_output = crate::output::V4l2Output::new(
            &self.state.output_device,
            pipeline.output_width(),
            pipeline.output_height(),
        )
        .map_err(|e| format!("V4L2 output failed: {}", e))?;

        pipeline
            .start_output(v4l2_output)
            .map_err(|e| format!("Start output failed: {}", e))?;

        self.state.v4l2_output_active = true;
        self.state.status_message = "Virtual camera active".into();
        Ok(())
    }

    /// Stop v4l2 output without stopping capture+render
    pub fn stop_v4l2_output(&mut self) {
        if let Some(ref mut pipeline) = self.pipeline {
            pipeline.stop_output();
        }
        self.state.v4l2_output_active = false;
        self.state.status_message = "Virtual camera stopped".into();
    }

    /// Send camera change command to the pipeline
    pub fn change_camera(&mut self, new_index: u32) {
        if let Some(ref tx) = self.capture_cmd_tx {
            let resolution = self.state.resolution();
            let (resp_tx, _resp_rx) = crossbeam_channel::bounded(1);
            let _ = tx.try_send(CaptureCommand {
                action: CaptureAction::ChangeCamera {
                    index: new_index,
                    resolution,
                },
                response_tx: resp_tx,
            });
        }
        self.state.camera_index = new_index;
    }
}
