use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender};
use eframe::egui;

use crate::control::{CaptureCommand, RenderCommand};
use crate::pipeline::{Pipeline, PreviewFrame};

use super::panels;
use super::state::GuiState;
use super::v4l2_manager;

pub struct VirtualAsciiApp {
    pub state: GuiState,
    pub raw_preview_texture: Option<egui::TextureHandle>,
    pub rendered_preview_texture: Option<egui::TextureHandle>,
    pub pipeline: Option<Pipeline>,
    pub gui_raw_rx: Option<Receiver<PreviewFrame>>,
    pub gui_rendered_rx: Option<Receiver<PreviewFrame>>,
    pub capture_cmd_tx: Option<Sender<CaptureCommand>>,
    pub render_cmd_tx: Option<Sender<RenderCommand>>,
    pub shutdown: Arc<AtomicBool>,
    pub v4l2_op_result: Arc<Mutex<Option<Result<String, String>>>>,
}

impl VirtualAsciiApp {
    pub fn new() -> Self {
        let mut state = GuiState::new();
        state.v4l2loopback_loaded = v4l2_manager::is_v4l2loopback_loaded();

        Self {
            state,
            raw_preview_texture: None,
            rendered_preview_texture: None,
            pipeline: None,
            gui_raw_rx: None,
            gui_rendered_rx: None,
            capture_cmd_tx: None,
            render_cmd_tx: None,
            shutdown: Arc::new(AtomicBool::new(false)),
            v4l2_op_result: Arc::new(Mutex::new(None)),
        }
    }

    /// Poll preview channels and upload latest frames as textures
    fn poll_preview_frames(&mut self, ctx: &egui::Context) {
        if let Some(ref rx) = self.gui_raw_rx {
            let mut latest = None;
            while let Ok(frame) = rx.try_recv() {
                latest = Some(frame);
            }
            if let Some(frame) = latest {
                let image = egui::ColorImage::from_rgb(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgb,
                );
                match &mut self.raw_preview_texture {
                    Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                    None => {
                        self.raw_preview_texture = Some(ctx.load_texture(
                            "raw-preview",
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }
        }

        if let Some(ref rx) = self.gui_rendered_rx {
            let mut latest = None;
            while let Ok(frame) = rx.try_recv() {
                latest = Some(frame);
            }
            if let Some(frame) = latest {
                let image = egui::ColorImage::from_rgb(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgb,
                );
                match &mut self.rendered_preview_texture {
                    Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                    None => {
                        self.rendered_preview_texture = Some(ctx.load_texture(
                            "rendered-preview",
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }
        }
    }

    /// Check for v4l2 operation results from background threads
    fn check_v4l2_results(&mut self) {
        let mut result = self.v4l2_op_result.lock().unwrap();
        if let Some(res) = result.take() {
            match res {
                Ok(msg) => {
                    self.state.status_message = msg;
                    self.state.v4l2loopback_loaded = v4l2_manager::is_v4l2loopback_loaded();
                }
                Err(msg) => {
                    self.state.status_message = format!("Error: {}", msg);
                }
            }
        }
    }

    /// Flush pending settings changes to pipeline threads
    fn flush_settings(&mut self) {
        if !self.state.pipeline_running {
            return;
        }

        // Debounce: wait 150ms after last change before flushing
        if let Some(last) = self.state.last_change_time {
            if last.elapsed() < std::time::Duration::from_millis(150) {
                return;
            }
        }

        if self.state.capture_dirty {
            self.state.capture_dirty = false;
            self.send_capture_commands();
        }

        if self.state.render_dirty {
            self.state.render_dirty = false;
            self.send_render_commands();
        }
    }

    fn send_capture_commands(&self) {
        use crate::control::{CaptureAction, CaptureCommand};

        if let Some(ref tx) = self.capture_cmd_tx {
            // Send FPS change
            let (resp_tx, _resp_rx) = crossbeam_channel::bounded(1);
            let _ = tx.try_send(CaptureCommand {
                action: CaptureAction::ChangeFps {
                    fps: self.state.fps,
                },
                response_tx: resp_tx,
            });
        }
    }

    fn send_render_commands(&self) {
        use crate::config::definition_to_params;
        use crate::control::{RenderAction, RenderCommand};

        if let Some(ref tx) = self.render_cmd_tx {
            let (ascii_columns, charset) =
                definition_to_params(self.state.definition, &self.state.theme_name);
            let (resp_tx, _resp_rx) = crossbeam_channel::bounded(1);
            let _ = tx.try_send(RenderCommand {
                action: RenderAction::Rebuild {
                    charset,
                    ascii_columns,
                    fg: self.state.fg_rgb(),
                    bg: self.state.bg_rgb(),
                    brightness_curve: self.state.brightness_curve(),
                    invert: self.state.invert,
                    theme_name: self.state.theme_name.clone(),
                },
                response_tx: resp_tx,
            });
        }
    }
}

impl eframe::App for VirtualAsciiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_preview_frames(ctx);
        self.check_v4l2_results();
        self.flush_settings();

        panels::settings_panel(ctx, self);
        panels::preview_panel(ctx, self);
        panels::status_bar(ctx, self);

        // Keep repainting while pipeline is running
        if self.state.pipeline_running {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Take ownership of pipeline and wait for threads
        if let Some(pipeline) = self.pipeline.take() {
            pipeline.wait();
        }
    }
}
