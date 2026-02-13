use std::process::Command;

/// Check if a camera device is held by another process.
/// Returns Some(description) if busy, None if available.
pub fn check_camera_busy(device_index: u32) -> Option<String> {
    let device_path = format!("/dev/video{}", device_index);
    let output = Command::new("fuser").arg(&device_path).output().ok()?;

    if !output.status.success() || output.stdout.is_empty() {
        return None; // No process holding the device
    }

    let pids_str = String::from_utf8_lossy(&output.stdout);
    let my_pid = std::process::id();

    let other_pids: Vec<&str> = pids_str
        .split_whitespace()
        .filter(|pid| pid.parse::<u32>().ok() != Some(my_pid))
        .collect();

    if other_pids.is_empty() {
        return None;
    }

    // Get process names
    let mut names = Vec::new();
    for pid in &other_pids {
        let comm_path = format!("/proc/{}/comm", pid);
        if let Ok(name) = std::fs::read_to_string(&comm_path) {
            names.push(format!("{} (PID {})", name.trim(), pid));
        }
    }

    if names.is_empty() {
        Some(format!("Device held by PIDs: {}", other_pids.join(", ")))
    } else {
        Some(format!("Device held by: {}", names.join(", ")))
    }
}

/// Check if another virtual-ascii CLI instance is running
/// by trying to connect to the abstract socket.
pub fn is_cli_instance_running() -> bool {
    crate::control::connect_abstract_stream().is_ok()
}
