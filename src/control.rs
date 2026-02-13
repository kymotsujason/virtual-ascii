use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Sender;

use crate::config::{
    definition_to_params, parse_hex_color, parse_resolution, BrightnessCurve, ColorTheme, Rgb,
};
use crate::detect;

// --- Command types ---

pub struct CaptureCommand {
    pub action: CaptureAction,
    pub response_tx: crossbeam_channel::Sender<Result<String, String>>,
}

pub enum CaptureAction {
    ChangeCamera {
        index: u32,
        resolution: Option<(u32, u32)>,
    },
    ChangeFps {
        fps: u32,
    },
}

pub struct RenderCommand {
    pub action: RenderAction,
    pub response_tx: crossbeam_channel::Sender<Result<String, String>>,
}

pub enum RenderAction {
    Rebuild {
        charset: Vec<char>,
        ascii_columns: u32,
        fg: Rgb,
        bg: Rgb,
        brightness_curve: BrightnessCurve,
        invert: bool,
        theme_name: String,
    },
}

// --- Runtime state ---

pub struct RuntimeState {
    pub camera_index: u32,
    pub resolution: Option<(u32, u32)>,
    pub fps: u32,
    pub max_fps: u32,
    pub theme_name: String,
    pub fg: Rgb,
    pub bg: Rgb,
    pub definition: u8,
    pub brightness_curve: BrightnessCurve,
    pub invert: bool,
}

impl RuntimeState {
    pub fn format_status(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("camera_index={}\n", self.camera_index));
        if let Some((w, h)) = self.resolution {
            out.push_str(&format!("resolution={}x{}\n", w, h));
        } else {
            out.push_str("resolution=auto\n");
        }
        out.push_str(&format!("fps={}\n", self.fps));
        out.push_str(&format!("theme={}\n", self.theme_name));
        out.push_str(&format!("fg_color={}\n", self.fg.to_hex()));
        out.push_str(&format!("bg_color={}\n", self.bg.to_hex()));
        out.push_str(&format!("definition={}\n", self.definition));
        out.push_str(&format!("brightness_curve={}\n", self.brightness_curve.name()));
        out.push_str(&format!("invert={}\n", self.invert));
        out.push_str("END\n");
        out
    }
}

// --- Abstract namespace socket helpers ---

const SOCKET_NAME: &[u8] = b"virtual-ascii";

fn make_abstract_addr() -> (libc::sockaddr_un, libc::socklen_t) {
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    // Abstract namespace: sun_path[0] = 0, then the name
    addr.sun_path[0] = 0;
    for (i, &b) in SOCKET_NAME.iter().enumerate() {
        addr.sun_path[i + 1] = b as libc::c_char;
    }
    // Length: family + NUL byte + name length (no trailing NUL needed for abstract)
    let len = std::mem::size_of::<libc::sa_family_t>() + 1 + SOCKET_NAME.len();
    (addr, len as libc::socklen_t)
}

pub fn bind_abstract_listener() -> std::io::Result<UnixListener> {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let (addr, addr_len) = make_abstract_addr();
    let ret = unsafe {
        libc::bind(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            addr_len,
        )
    };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }

    let ret = unsafe { libc::listen(fd, 5) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }

    Ok(unsafe { UnixListener::from_raw_fd(fd) })
}

pub fn connect_abstract_stream() -> std::io::Result<UnixStream> {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let (addr, addr_len) = make_abstract_addr();
    let ret = unsafe {
        libc::connect(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            addr_len,
        )
    };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }

    Ok(unsafe { UnixStream::from_raw_fd(fd) })
}

pub fn ignore_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

// --- Socket listener ---

pub fn start_listener(
    state: Arc<Mutex<RuntimeState>>,
    capture_cmd_tx: Sender<CaptureCommand>,
    render_cmd_tx: Sender<RenderCommand>,
    shutdown: Arc<AtomicBool>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let listener = bind_abstract_listener()?;
    listener.set_nonblocking(true)?;

    let handle = std::thread::Builder::new()
        .name("control".into())
        .spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // Reject connections from other UIDs (fail-closed)
                        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
                        let mut len =
                            std::mem::size_of::<libc::ucred>() as libc::socklen_t;
                        let ret = unsafe {
                            libc::getsockopt(
                                stream.as_raw_fd(),
                                libc::SOL_SOCKET,
                                libc::SO_PEERCRED,
                                &mut cred as *mut _ as *mut libc::c_void,
                                &mut len,
                            )
                        };
                        if ret != 0
                            || len != std::mem::size_of::<libc::ucred>() as libc::socklen_t
                            || cred.uid != unsafe { libc::getuid() }
                        {
                            continue;
                        }

                        // Set read timeout to prevent a misbehaving client from blocking forever
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                        handle_connection(
                            stream,
                            &state,
                            &capture_cmd_tx,
                            &render_cmd_tx,
                        );
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(e) => {
                        if !shutdown.load(Ordering::Relaxed) {
                            eprintln!("Control socket error: {}", e);
                        }
                        break;
                    }
                }
            }
        })
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    Ok(handle)
}

// --- Connection handler ---

const MAX_LINE_LENGTH: usize = 4096;
const MAX_COMMANDS_PER_CONNECTION: usize = 100;

fn handle_connection(
    stream: UnixStream,
    state: &Arc<Mutex<RuntimeState>>,
    capture_cmd_tx: &Sender<CaptureCommand>,
    render_cmd_tx: &Sender<RenderCommand>,
) {
    let mut reader = BufReader::new(&stream);
    let mut writer = &stream;

    // Collect all lines with length and count limits
    let mut commands: Vec<String> = Vec::new();
    let mut line_buf = String::new();
    loop {
        line_buf.clear();
        match reader.read_line(&mut line_buf) {
            Ok(0) => break, // EOF
            Ok(n) if n > MAX_LINE_LENGTH => {
                let _ = writer.write_all(b"ERR line too long\n");
                return;
            }
            Ok(_) => {
                let trimmed = line_buf.trim().to_string();
                if !trimmed.is_empty() {
                    commands.push(trimmed);
                }
                if commands.len() >= MAX_COMMANDS_PER_CONNECTION {
                    let _ = writer.write_all(b"ERR too many commands\n");
                    return;
                }
            }
            Err(_) => break,
        }
    }

    if commands.is_empty() {
        return;
    }

    // Check for STATUS command
    if commands.iter().any(|c| c.eq_ignore_ascii_case("STATUS")) {
        let st = state.lock().unwrap_or_else(|e| e.into_inner());
        let status = st.format_status();
        let _ = writer.write_all(status.as_bytes());
        return;
    }

    // Parse all SET commands
    let mut responses: Vec<String> = Vec::new();
    let mut capture_changes = CaptureChanges::default();
    let mut render_changes = RenderChanges::default();

    for cmd in &commands {
        let upper = cmd.to_uppercase();
        if !upper.starts_with("SET ") {
            responses.push(format!("ERR unknown command: {}\n", cmd));
            continue;
        }

        let payload = cmd[4..].trim();
        let (key, value) = match payload.splitn(2, '=').collect::<Vec<_>>()[..] {
            [k, v] => (k.trim().to_lowercase(), v.trim().to_string()),
            _ => {
                responses.push(format!("ERR invalid format: {}\n", payload));
                continue;
            }
        };

        let current_max_fps = state.lock().unwrap_or_else(|e| e.into_inner()).max_fps;

        match key.as_str() {
            "camera_index" => match value.parse::<u32>() {
                Ok(i) => capture_changes.camera_index = Some(i),
                Err(_) => {
                    responses.push(format!("ERR invalid camera_index: {}\n", value));
                    continue;
                }
            },
            "resolution" => match parse_resolution(&value) {
                Ok(res) => capture_changes.resolution = Some(Some(res)),
                Err(e) => {
                    responses.push(format!("ERR {}\n", e));
                    continue;
                }
            },
            "fps" => match value.parse::<u32>() {
                Ok(f) if (1..=current_max_fps).contains(&f) => capture_changes.fps = Some(f),
                _ => {
                    responses.push(format!(
                        "ERR invalid fps: {} (must be 1-{})\n",
                        value, current_max_fps
                    ));
                    continue;
                }
            },
            "theme" => match ColorTheme::from_name(&value) {
                Some(t) => {
                    render_changes.theme_name = Some(t.name.clone());
                    render_changes.fg = Some(t.fg);
                    render_changes.bg = Some(t.bg);
                }
                None => {
                    responses.push(format!(
                        "ERR unknown theme '{}'. Available: mono, green, amber, blue, matrix, vaporwave, fire, color\n",
                        value
                    ));
                    continue;
                }
            },
            "fg_color" => match parse_hex_color(&value) {
                Some(c) => render_changes.fg = Some(c),
                None => {
                    responses.push(format!(
                        "ERR invalid fg_color '{}'. Use 6-digit hex (e.g. ff00ff)\n",
                        value
                    ));
                    continue;
                }
            },
            "bg_color" => match parse_hex_color(&value) {
                Some(c) => render_changes.bg = Some(c),
                None => {
                    responses.push(format!(
                        "ERR invalid bg_color '{}'. Use 6-digit hex (e.g. 001100)\n",
                        value
                    ));
                    continue;
                }
            },
            "definition" => match value.parse::<u8>() {
                Ok(d) if (1..=10).contains(&d) => render_changes.definition = Some(d),
                _ => {
                    responses.push(format!(
                        "ERR invalid definition: {} (must be 1-10)\n",
                        value
                    ));
                    continue;
                }
            },
            "brightness_curve" => match BrightnessCurve::from_name(&value) {
                Some(c) => render_changes.brightness_curve = Some(c),
                None => {
                    responses.push(format!(
                        "ERR unknown brightness_curve '{}'. Available: linear, exponential, sigmoid\n",
                        value
                    ));
                    continue;
                }
            },
            "invert" => match value.as_str() {
                "true" => render_changes.invert = Some(true),
                "false" => render_changes.invert = Some(false),
                _ => {
                    responses.push(format!(
                        "ERR invalid invert: {} (must be true or false)\n",
                        value
                    ));
                    continue;
                }
            },
            _ => {
                responses.push(format!("ERR unknown key: {}\n", key));
                continue;
            }
        }
    }

    // Snapshot current state
    let snapshot = {
        let st = state.lock().unwrap_or_else(|e| e.into_inner());
        StateSnapshot {
            camera_index: st.camera_index,
            resolution: st.resolution,
            theme_name: st.theme_name.clone(),
            fg: st.fg,
            bg: st.bg,
            definition: st.definition,
            brightness_curve: st.brightness_curve,
            invert: st.invert,
        }
    };

    // Route capture changes
    if capture_changes.has_changes() {
        let cam_idx = capture_changes.camera_index.unwrap_or(snapshot.camera_index);
        let resolution = capture_changes.resolution.unwrap_or(snapshot.resolution);
        let fps = capture_changes.fps;

        // Camera/resolution change requires ChangeCamera
        let needs_camera_change =
            capture_changes.camera_index.is_some() || capture_changes.resolution.is_some();

        if needs_camera_change {
            let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
            let cmd = CaptureCommand {
                action: CaptureAction::ChangeCamera {
                    index: cam_idx,
                    resolution,
                },
                response_tx: resp_tx,
            };
            if capture_cmd_tx.send(cmd).is_ok() {
                match resp_rx.recv_timeout(Duration::from_secs(5)) {
                    Ok(Ok(msg)) => {
                        responses.push(format!("OK {}\n", msg));
                        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                        st.camera_index = cam_idx;
                        st.resolution = resolution;
                        // Refresh max_fps for the new camera/resolution
                        let new_max = if let Some((w, h)) = resolution {
                            detect::max_fps_for_resolution(cam_idx, w, h)
                                .unwrap_or(240)
                        } else {
                            // Auto mode: max across all resolutions
                            detect::list_resolutions(cam_idx)
                                .iter()
                                .filter_map(|(w, h)| {
                                    detect::max_fps_for_resolution(cam_idx, *w, *h)
                                })
                                .max()
                                .unwrap_or(240)
                        };
                        st.max_fps = new_max;
                        if st.fps > st.max_fps {
                            st.fps = st.max_fps;
                        }
                    }
                    Ok(Err(msg)) => responses.push(format!("ERR {}\n", msg)),
                    Err(_) => responses.push("ERR camera change timed out\n".to_string()),
                }
            } else {
                responses.push("ERR pipeline shutting down\n".to_string());
            }
        }

        if let Some(new_fps) = fps {
            let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
            let cmd = CaptureCommand {
                action: CaptureAction::ChangeFps { fps: new_fps },
                response_tx: resp_tx,
            };
            if capture_cmd_tx.send(cmd).is_ok() {
                match resp_rx.recv_timeout(Duration::from_secs(5)) {
                    Ok(Ok(msg)) => {
                        responses.push(format!("OK {}\n", msg));
                        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                        st.fps = new_fps;
                    }
                    Ok(Err(msg)) => responses.push(format!("ERR {}\n", msg)),
                    Err(_) => responses.push("ERR fps change timed out\n".to_string()),
                }
            } else {
                responses.push("ERR pipeline shutting down\n".to_string());
            }
        }
    }

    // Route render changes
    if render_changes.has_changes() {
        let theme_name = render_changes
            .theme_name
            .unwrap_or(snapshot.theme_name.clone());
        let fg = render_changes.fg.unwrap_or(snapshot.fg);
        let bg = render_changes.bg.unwrap_or(snapshot.bg);
        let definition = render_changes.definition.unwrap_or(snapshot.definition);
        let brightness_curve = render_changes
            .brightness_curve
            .unwrap_or(snapshot.brightness_curve);
        let invert = render_changes.invert.unwrap_or(snapshot.invert);

        let (ascii_columns, charset) = definition_to_params(definition, &theme_name);

        let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
        let cmd = RenderCommand {
            action: RenderAction::Rebuild {
                charset,
                ascii_columns,
                fg,
                bg,
                brightness_curve,
                invert,
                theme_name: theme_name.clone(),
            },
            response_tx: resp_tx,
        };
        if render_cmd_tx.send(cmd).is_ok() {
            match resp_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(msg)) => {
                    responses.push(format!("OK {}\n", msg));
                    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                    st.theme_name = theme_name;
                    st.fg = fg;
                    st.bg = bg;
                    st.definition = definition;
                    st.brightness_curve = brightness_curve;
                    st.invert = invert;
                }
                Ok(Err(msg)) => responses.push(format!("ERR {}\n", msg)),
                Err(_) => responses.push("ERR render rebuild timed out\n".to_string()),
            }
        } else {
            responses.push("ERR pipeline shutting down\n".to_string());
        }
    }

    // Send all responses
    for resp in &responses {
        let _ = writer.write_all(resp.as_bytes());
    }
}

// --- Change tracking ---

#[derive(Default)]
struct CaptureChanges {
    camera_index: Option<u32>,
    resolution: Option<Option<(u32, u32)>>,
    fps: Option<u32>,
}

impl CaptureChanges {
    fn has_changes(&self) -> bool {
        self.camera_index.is_some() || self.resolution.is_some() || self.fps.is_some()
    }
}

#[derive(Default)]
struct RenderChanges {
    theme_name: Option<String>,
    fg: Option<Rgb>,
    bg: Option<Rgb>,
    definition: Option<u8>,
    brightness_curve: Option<BrightnessCurve>,
    invert: Option<bool>,
}

impl RenderChanges {
    fn has_changes(&self) -> bool {
        self.theme_name.is_some()
            || self.fg.is_some()
            || self.bg.is_some()
            || self.definition.is_some()
            || self.brightness_curve.is_some()
            || self.invert.is_some()
    }
}

struct StateSnapshot {
    camera_index: u32,
    resolution: Option<(u32, u32)>,
    theme_name: String,
    fg: Rgb,
    bg: Rgb,
    definition: u8,
    brightness_curve: BrightnessCurve,
    invert: bool,
}
