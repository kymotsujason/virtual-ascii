use std::sync::{Arc, Mutex};

/// Check if the v4l2loopback kernel module is currently loaded
pub fn is_v4l2loopback_loaded() -> bool {
    if let Ok(modules) = std::fs::read_to_string("/proc/modules") {
        modules.lines().any(|line| line.starts_with("v4l2loopback "))
    } else {
        false
    }
}

/// Load v4l2loopback module via pkexec (runs on background thread)
pub fn load_v4l2loopback(
    video_nr: u32,
    card_label: &str,
    result: Arc<Mutex<Option<Result<String, String>>>>,
) {
    let card_label = card_label.to_string();
    std::thread::spawn(move || {
        let output = std::process::Command::new("pkexec")
            .args([
                "modprobe",
                "v4l2loopback",
                "devices=1",
                &format!("video_nr={}", video_nr),
                "exclusive_caps=1",
                &format!("card_label={}", card_label),
            ])
            .output();

        let res = match output {
            Ok(out) if out.status.success() => Ok("v4l2loopback module loaded".into()),
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Err(format!("modprobe failed: {}", stderr.trim()))
            }
            Err(e) => Err(format!("pkexec failed: {}", e)),
        };
        *result.lock().unwrap() = Some(res);
    });
}

/// Unload v4l2loopback module via pkexec (runs on background thread)
pub fn unload_v4l2loopback(result: Arc<Mutex<Option<Result<String, String>>>>) {
    std::thread::spawn(move || {
        let output = std::process::Command::new("pkexec")
            .args(["modprobe", "-r", "v4l2loopback"])
            .output();

        let res = match output {
            Ok(out) if out.status.success() => Ok("v4l2loopback module unloaded".into()),
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Err(format!("modprobe -r failed: {}", stderr.trim()))
            }
            Err(e) => Err(format!("pkexec failed: {}", e)),
        };
        *result.lock().unwrap() = Some(res);
    });
}
