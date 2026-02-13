use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};

use crate::capture::WebcamCapture;
use crate::control::{CaptureAction, CaptureCommand, RenderAction, RenderCommand};
use crate::output::V4l2Output;
use crate::renderer::AsciiRenderer;

/// Frame data passed between pipeline stages
pub struct Frame {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Frame data sent to GUI for preview display
#[cfg(feature = "gui")]
pub struct PreviewFrame {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub struct Pipeline {
    threads: Vec<thread::JoinHandle<()>>,
    out_w: u32,
    out_h: u32,
    /// Channel for sending rendered frames to the output thread.
    /// Stored here so we can spawn the output thread later via start_output().
    output_rx: Option<Receiver<Vec<u8>>>,
    shutdown: Arc<AtomicBool>,
}

impl Pipeline {
    pub fn start(
        camera_index: u32,
        resolution: Option<(u32, u32)>,
        target_fps: u32,
        renderer: AsciiRenderer,
        v4l2_output: Option<V4l2Output>,
        shutdown: Arc<AtomicBool>,
        capture_cmd_rx: Receiver<CaptureCommand>,
        render_cmd_rx: Receiver<RenderCommand>,
        #[cfg(feature = "gui")] gui_raw_tx: Option<Sender<PreviewFrame>>,
        #[cfg(feature = "gui")] gui_rendered_tx: Option<Sender<PreviewFrame>>,
    ) -> anyhow::Result<Self> {
        let (capture_tx, capture_rx): (Sender<Frame>, Receiver<Frame>) = bounded(2);
        let (render_tx, render_rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = bounded(2);

        let out_w = renderer.output_width;
        let out_h = renderer.output_height;

        let mut frame_interval = Duration::from_secs_f64(1.0 / target_fps as f64);
        let shutdown_capture = shutdown.clone();
        let shutdown_render = shutdown.clone();

        // Capture thread — creates Camera internally to avoid Send issues
        let capture_handle = thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                let mut cur_fps = target_fps;
                let mut camera = match WebcamCapture::new(camera_index, resolution, target_fps) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Capture thread error: {}", e);
                        shutdown_capture.store(true, Ordering::SeqCst);
                        return;
                    }
                };

                let (mut w, mut h) = camera.resolution();
                let mut cur_index = camera_index;
                let mut cur_resolution = resolution;
                eprintln!("  Capturing: {}x{}", w, h);

                let mut fps_counter = FpsCounter::new("Capture");
                let mut consecutive_errors: u32 = 0;

                while !shutdown_capture.load(Ordering::Relaxed) {
                    let start = Instant::now();

                    // Drain command queue
                    while let Ok(cmd) = capture_cmd_rx.try_recv() {
                        match cmd.action {
                            CaptureAction::ChangeCamera {
                                index,
                                resolution: new_res,
                            } => {
                                let old_index = cur_index;
                                let old_res = cur_resolution;

                                // Stop and drop old camera — sleep gives the UVC
                                // driver time to fully release the device
                                camera.stop_stream();
                                drop(camera);
                                thread::sleep(Duration::from_millis(200));

                                match WebcamCapture::new(index, new_res, cur_fps) {
                                    Ok(new_cam) => {
                                        let (nw, nh) = new_cam.resolution();
                                        camera = new_cam;
                                        w = nw;
                                        h = nh;
                                        cur_index = index;
                                        cur_resolution = new_res;
                                        consecutive_errors = 0;
                                        let res_str = format!("{}x{}", nw, nh);
                                        eprintln!(
                                            "  Camera changed: /dev/video{} ({})",
                                            index, res_str
                                        );
                                        let _ = cmd.response_tx.send(Ok(format!(
                                            "camera_index={} ({})",
                                            index, res_str
                                        )));
                                    }
                                    Err(e) => {
                                        let err_msg = format!("{}", e);
                                        eprintln!("  Camera change failed: {}", err_msg);
                                        // Rollback to old camera
                                        thread::sleep(Duration::from_millis(200));
                                        match WebcamCapture::new(old_index, old_res, cur_fps) {
                                            Ok(old_cam) => {
                                                let (ow, oh) = old_cam.resolution();
                                                camera = old_cam;
                                                w = ow;
                                                h = oh;
                                                eprintln!(
                                                    "  Rolled back to /dev/video{}",
                                                    old_index
                                                );
                                                let _ = cmd.response_tx.send(Err(err_msg));
                                            }
                                            Err(rollback_err) => {
                                                eprintln!(
                                                    "  FATAL: Rollback failed: {}. Shutting down.",
                                                    rollback_err
                                                );
                                                let _ = cmd.response_tx.send(Err(format!(
                                                    "camera change failed and rollback failed: {}",
                                                    rollback_err
                                                )));
                                                shutdown_capture.store(true, Ordering::SeqCst);
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                            CaptureAction::ChangeFps { fps } => {
                                frame_interval = Duration::from_secs_f64(1.0 / fps as f64);
                                // Reopen camera at new FPS
                                camera.stop_stream();
                                drop(camera);
                                thread::sleep(Duration::from_millis(200));
                                match WebcamCapture::new(cur_index, cur_resolution, fps) {
                                    Ok(new_cam) => {
                                        let (nw, nh) = new_cam.resolution();
                                        camera = new_cam;
                                        w = nw;
                                        h = nh;
                                        cur_fps = fps;
                                        eprintln!("  FPS changed: {} (camera reopened)", fps);
                                        let _ = cmd.response_tx.send(Ok(format!("fps={}", fps)));
                                    }
                                    Err(e) => {
                                        eprintln!("  FPS change failed: {}, reopening at old fps", e);
                                        match WebcamCapture::new(cur_index, cur_resolution, cur_fps) {
                                            Ok(old_cam) => {
                                                camera = old_cam;
                                                let _ = cmd.response_tx.send(Err(format!("{}", e)));
                                            }
                                            Err(e2) => {
                                                eprintln!("  FATAL: rollback failed: {}", e2);
                                                let _ = cmd.response_tx.send(Err(format!("{}", e2)));
                                                shutdown_capture.store(true, Ordering::SeqCst);
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    match camera.capture_frame() {
                        Ok(rgb) => {
                            consecutive_errors = 0;

                            // Send to GUI raw preview if available
                            #[cfg(feature = "gui")]
                            if let Some(ref gui_tx) = gui_raw_tx {
                                let _ = gui_tx.try_send(PreviewFrame {
                                    rgb: rgb.clone(),
                                    width: w,
                                    height: h,
                                });
                            }

                            let frame = Frame {
                                rgb,
                                width: w,
                                height: h,
                            };
                            match capture_tx.try_send(frame) {
                                Ok(()) => {}
                                Err(crossbeam_channel::TrySendError::Full(_)) => {}
                                Err(crossbeam_channel::TrySendError::Disconnected(_)) => break,
                            }
                            fps_counter.tick();
                        }
                        Err(e) => {
                            consecutive_errors += 1;
                            if !shutdown_capture.load(Ordering::Relaxed) {
                                eprintln!("Capture error: {}", e);
                            }
                            if consecutive_errors >= 30 {
                                eprintln!("Too many consecutive capture errors, shutting down");
                                shutdown_capture.store(true, Ordering::SeqCst);
                                break;
                            }
                        }
                    }

                    // Rate limit to target FPS
                    let elapsed = start.elapsed();
                    if elapsed < frame_interval {
                        thread::sleep(frame_interval - elapsed);
                    }
                }
            })?;

        // Render thread
        let render_handle = thread::Builder::new()
            .name("render".into())
            .spawn(move || {
                let mut renderer = renderer;
                let mut fps_counter = FpsCounter::new("Render");
                let timeout = Duration::from_millis(100);

                loop {
                    if shutdown_render.load(Ordering::Relaxed) {
                        break;
                    }

                    // Drain command queue
                    while let Ok(cmd) = render_cmd_rx.try_recv() {
                        match cmd.action {
                            RenderAction::Rebuild {
                                charset,
                                ascii_columns,
                                fg,
                                bg,
                                brightness_curve,
                                invert,
                                theme_name,
                            } => {
                                let out_w = renderer.output_width;
                                let out_h = renderer.output_height;

                                match AsciiRenderer::new(
                                    &charset,
                                    fg,
                                    bg,
                                    brightness_curve,
                                    invert,
                                    out_w,
                                    out_h,
                                    ascii_columns,
                                    &theme_name,
                                ) {
                                    Ok(new_renderer) => {
                                        renderer = new_renderer;
                                        eprintln!("  Renderer rebuilt ({} cols)", ascii_columns);
                                        let _ = cmd.response_tx.send(Ok(format!(
                                            "renderer rebuilt ({} cols)",
                                            ascii_columns
                                        )));
                                    }
                                    Err(e) => {
                                        eprintln!("  Renderer rebuild failed: {}", e);
                                        let _ = cmd.response_tx.send(Err(e));
                                    }
                                }
                            }
                        }
                    }

                    match capture_rx.recv_timeout(timeout) {
                        Ok(frame) => {
                            let rendered = renderer.render(&frame.rgb, frame.width, frame.height);

                            // Send to GUI rendered preview if available
                            #[cfg(feature = "gui")]
                            if let Some(ref gui_tx) = gui_rendered_tx {
                                let _ = gui_tx.try_send(PreviewFrame {
                                    rgb: rendered.clone(),
                                    width: renderer.output_width,
                                    height: renderer.output_height,
                                });
                            }

                            match render_tx.try_send(rendered) {
                                Ok(()) => {}
                                Err(crossbeam_channel::TrySendError::Full(_)) => {}
                                Err(crossbeam_channel::TrySendError::Disconnected(_)) => break,
                            }
                            fps_counter.tick();
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;

        let mut threads = vec![capture_handle, render_handle];

        // Output thread — only spawn if v4l2_output is provided
        let output_rx = if let Some(mut v4l2_output) = v4l2_output {
            let shutdown_output = shutdown.clone();
            let output_handle = thread::Builder::new()
                .name("output".into())
                .spawn(move || {
                    let mut fps_counter = FpsCounter::new("Output");
                    let timeout = Duration::from_millis(100);

                    loop {
                        if shutdown_output.load(Ordering::Relaxed) {
                            break;
                        }

                        match render_rx.recv_timeout(timeout) {
                            Ok(rendered_frame) => {
                                if let Err(e) = v4l2_output.write_frame(&rendered_frame) {
                                    if !shutdown_output.load(Ordering::Relaxed) {
                                        eprintln!("Output error: {}", e);
                                    }
                                    shutdown_output.store(true, Ordering::SeqCst);
                                    break;
                                }
                                fps_counter.tick();
                            }
                            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })?;
            threads.push(output_handle);
            None // render_rx consumed by output thread
        } else {
            Some(render_rx) // Keep for later start_output()
        };

        Ok(Pipeline {
            threads,
            out_w,
            out_h,
            output_rx,
            shutdown: shutdown.clone(),
        })
    }

    pub fn output_width(&self) -> u32 {
        self.out_w
    }

    pub fn output_height(&self) -> u32 {
        self.out_h
    }

    /// Start the v4l2 output thread on an already-running pipeline.
    pub fn start_output(&mut self, mut v4l2_output: V4l2Output) -> anyhow::Result<()> {
        let render_rx = self
            .output_rx
            .take()
            .ok_or_else(|| anyhow::anyhow!("Output already running or no render channel"))?;

        let shutdown_output = self.shutdown.clone();
        let output_handle = thread::Builder::new()
            .name("output".into())
            .spawn(move || {
                let mut fps_counter = FpsCounter::new("Output");
                let timeout = Duration::from_millis(100);

                loop {
                    if shutdown_output.load(Ordering::Relaxed) {
                        break;
                    }

                    match render_rx.recv_timeout(timeout) {
                        Ok(rendered_frame) => {
                            if let Err(e) = v4l2_output.write_frame(&rendered_frame) {
                                if !shutdown_output.load(Ordering::Relaxed) {
                                    eprintln!("Output error: {}", e);
                                }
                                shutdown_output.store(true, Ordering::SeqCst);
                                break;
                            }
                            fps_counter.tick();
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        self.threads.push(output_handle);
        Ok(())
    }

    /// Stop the v4l2 output thread (the capture+render pipeline continues).
    /// Note: This signals shutdown to all threads. For a clean per-thread stop,
    /// the output thread would need its own shutdown flag (future enhancement).
    pub fn stop_output(&mut self) {
        // Currently a no-op for the output thread — it will exit when the pipeline shuts down.
        // A full implementation would need a separate shutdown flag per thread.
    }

    pub fn wait(self) {
        for handle in self.threads {
            let _ = handle.join();
        }
    }
}

/// Simple FPS counter that prints to stderr every 5 seconds
struct FpsCounter {
    name: &'static str,
    count: u32,
    last_report: Instant,
}

impl FpsCounter {
    fn new(name: &'static str) -> Self {
        FpsCounter {
            name,
            count: 0,
            last_report: Instant::now(),
        }
    }

    fn tick(&mut self) {
        self.count += 1;
        let elapsed = self.last_report.elapsed();
        if elapsed >= Duration::from_secs(5) {
            let fps = self.count as f64 / elapsed.as_secs_f64();
            eprintln!("  {} FPS: {:.1}", self.name, fps);
            self.count = 0;
            self.last_report = Instant::now();
        }
    }
}
