mod capture;
mod config;
mod control;
mod detect;
mod glyph_cache;
#[cfg(feature = "gui")]
mod gui;
mod output;
mod pipeline;
mod rain;
mod renderer;

use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use clap::Parser;
use config::{AppConfig, Cli, SetArgs, SubCommand};
use control::RuntimeState;
use output::V4l2Output;
use pipeline::Pipeline;
use renderer::AsciiRenderer;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(SubCommand::Set(args)) => cmd_set(args),
        Some(SubCommand::Status) => cmd_status(),
        #[cfg(feature = "gui")]
        Some(SubCommand::Gui) => gui::run_gui(),
        None => cmd_run(cli),
    }
}

fn cmd_run(cli: Cli) -> anyhow::Result<()> {
    control::ignore_sigpipe();

    let mut config = AppConfig::from_cli(cli.run)?;

    eprintln!("virtual-ascii v{}", env!("CARGO_PKG_VERSION"));
    eprintln!(
        "  Theme:      {} (fg: #{:02x}{:02x}{:02x}, bg: #{:02x}{:02x}{:02x})",
        config.theme.name,
        config.theme.fg.r,
        config.theme.fg.g,
        config.theme.fg.b,
        config.theme.bg.r,
        config.theme.bg.g,
        config.theme.bg.b,
    );
    eprintln!(
        "  Definition: {} ({} columns, {} chars)",
        config.definition,
        config.ascii_columns,
        config.charset.len()
    );
    eprintln!("  Curve:      {:?}", config.brightness_curve);
    eprintln!("  FPS:        {}", config.fps);
    let camera_name =
        detect::device_name(config.camera_index).unwrap_or_else(|| "unknown".to_string());
    eprintln!(
        "  Camera:     /dev/video{} ({})",
        config.camera_index, camera_name
    );
    eprintln!("  Output:     {}", config.output_device);

    if let Some((w, h)) = config.resolution {
        eprintln!("  Resolution: {}x{} (user-specified)", w, h);
    }

    let probe_res = probe_camera_resolution(config.camera_index, config.resolution, config.fps)?;
    let (out_w, out_h) = probe_res;
    let detected_max_fps =
        detect::max_fps_for_resolution(config.camera_index, out_w, out_h);
    if let Some(max_fps) = detected_max_fps {
        if config.fps > max_fps {
            eprintln!(
                "  Warning: requested {}fps exceeds camera maximum ({}fps), clamping",
                config.fps, max_fps
            );
            config.fps = max_fps;
        }
    }
    eprintln!("  Source:     {}x{}", out_w, out_h);
    if let Some(max_fps) = detected_max_fps {
        eprintln!("  Max FPS:    {} (detected)", max_fps);
    }

    let v4l2_output = V4l2Output::new(&config.output_device, out_w, out_h)?;
    let (negotiated_w, negotiated_h) = v4l2_output.resolution();
    eprintln!("  V4L2 out:   {}x{}", negotiated_w, negotiated_h);

    let ascii_renderer = AsciiRenderer::new(
        &config.charset,
        config.theme.fg,
        config.theme.bg,
        config.brightness_curve,
        config.invert,
        negotiated_w,
        negotiated_h,
        config.ascii_columns,
        &config.theme.name,
    )
    .map_err(|e| anyhow::anyhow!("Renderer init failed: {}", e))?;

    // Set up shutdown signal
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_ctrlc = shutdown.clone();
    ctrlc::set_handler(move || {
        eprintln!("\nShutting down...");
        shutdown_ctrlc.store(true, Ordering::SeqCst);
    })?;

    // Create command channels
    let (capture_cmd_tx, capture_cmd_rx) = crossbeam_channel::bounded(4);
    let (render_cmd_tx, render_cmd_rx) = crossbeam_channel::bounded(4);

    // Initialize runtime state
    let state = Arc::new(Mutex::new(RuntimeState {
        camera_index: config.camera_index,
        resolution: config.resolution,
        fps: config.fps,
        max_fps: detected_max_fps.unwrap_or(240),
        theme_name: config.theme.name.clone(),
        fg: config.theme.fg,
        bg: config.theme.bg,
        definition: config.definition,
        brightness_curve: config.brightness_curve,
        invert: config.invert,
    }));

    // Start control socket listener
    match control::start_listener(
        state.clone(),
        capture_cmd_tx,
        render_cmd_tx,
        shutdown.clone(),
    ) {
        Ok(_handle) => {
            eprintln!("  Control:    abstract socket \"virtual-ascii\"");
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                return Err(anyhow::anyhow!(
                    "Another virtual-ascii instance is already running.\n\
                     Hint: Use 'virtual-ascii status' to check, or kill the other instance first."
                ));
            }
            eprintln!("  Control:    socket failed ({}), hot-reload disabled", e);
        }
    }

    eprintln!("  Starting pipeline...");
    eprintln!("  Press Ctrl+C to stop");

    let pipeline = Pipeline::start(
        config.camera_index,
        config.resolution,
        config.fps,
        ascii_renderer,
        Some(v4l2_output),
        shutdown.clone(),
        capture_cmd_rx,
        render_cmd_rx,
        #[cfg(feature = "gui")]
        None,
        #[cfg(feature = "gui")]
        None,
    )?;

    pipeline.wait();
    eprintln!("Shutdown complete.");

    Ok(())
}

fn cmd_set(args: SetArgs) -> anyhow::Result<()> {
    let mut stream = control::connect_abstract_stream().map_err(|e| {
        anyhow::anyhow!(
            "Cannot connect to virtual-ascii: {}.\nIs virtual-ascii running?",
            e
        )
    })?;

    // Build SET commands from args
    let mut lines = String::new();
    if let Some(i) = args.camera_index {
        lines.push_str(&format!("SET camera_index={}\n", i));
    }
    if let Some((w, h)) = args.resolution {
        lines.push_str(&format!("SET resolution={}x{}\n", w, h));
    }
    if let Some(f) = args.fps {
        lines.push_str(&format!("SET fps={}\n", f));
    }
    if let Some(ref t) = args.theme {
        lines.push_str(&format!("SET theme={}\n", t));
    }
    if let Some(ref c) = args.fg_color {
        lines.push_str(&format!("SET fg_color={}\n", c));
    }
    if let Some(ref c) = args.bg_color {
        lines.push_str(&format!("SET bg_color={}\n", c));
    }
    if let Some(d) = args.definition {
        lines.push_str(&format!("SET definition={}\n", d));
    }
    if let Some(ref c) = args.brightness_curve {
        lines.push_str(&format!("SET brightness_curve={}\n", c));
    }
    if let Some(v) = args.invert {
        lines.push_str(&format!("SET invert={}\n", v));
    }

    if lines.is_empty() {
        eprintln!("No settings specified. Use --help for options.");
        return Ok(());
    }

    stream.write_all(lines.as_bytes())?;
    // Shut down write side so the server sees EOF
    stream.shutdown(std::net::Shutdown::Write)?;

    // Read responses
    let reader = BufReader::new(&stream);
    let mut had_error = false;
    for line in reader.lines() {
        match line {
            Ok(l) => {
                let trimmed = l.trim();
                if !trimmed.is_empty() {
                    if trimmed.starts_with("ERR") {
                        had_error = true;
                    }
                    println!("{}", trimmed);
                }
            }
            Err(_) => break,
        }
    }

    if had_error {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_status() -> anyhow::Result<()> {
    let mut stream = control::connect_abstract_stream().map_err(|e| {
        anyhow::anyhow!(
            "Cannot connect to virtual-ascii: {}.\nIs virtual-ascii running?",
            e
        )
    })?;

    stream.write_all(b"STATUS\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let reader = BufReader::new(&stream);
    for line in reader.lines() {
        match line {
            Ok(l) => {
                let trimmed = l.trim();
                if trimmed == "END" {
                    break;
                }
                if !trimmed.is_empty() {
                    println!("{}", trimmed);
                }
            }
            Err(_) => break,
        }
    }

    Ok(())
}

/// Quick probe to get camera resolution without keeping it open
pub fn probe_camera_resolution(
    camera_index: u32,
    resolution: Option<(u32, u32)>,
    fps: u32,
) -> anyhow::Result<(u32, u32)> {
    use nokhwa::utils::CameraIndex;
    use nokhwa::Camera;

    let index = CameraIndex::Index(camera_index);
    let format = capture::requested_format(resolution, fps);
    let camera = Camera::new(index, format).map_err(|e| {
        let base = format!(
            "Cannot open camera index {}: {}.\n\
             Hint: Check that a webcam is connected and you have permission.",
            camera_index, e
        );
        if let Some((w, h)) = resolution {
            anyhow::anyhow!(
                "{}\nHint: Camera may not support {}x{}.\n\
                 Try: v4l2-ctl --list-formats-ext -d /dev/video{}",
                base,
                w,
                h,
                camera_index
            )
        } else {
            anyhow::anyhow!(base)
        }
    })?;

    let cam_format = camera.camera_format();
    Ok((
        cam_format.resolution().width_x,
        cam_format.resolution().height_y,
    ))
}
