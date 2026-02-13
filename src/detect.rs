use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;

// V4L2 capability flags
const V4L2_CAP_VIDEO_CAPTURE: u32 = 0x00000001;
const V4L2_CAP_VIDEO_CAPTURE_MPLANE: u32 = 0x00001000;
const V4L2_CAP_DEVICE_CAPS: u32 = 0x80000000;

// V4L2 buffer/format types
const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;

// V4L2 frame size types
const V4L2_FRMSIZE_TYPE_DISCRETE: u32 = 1;

// V4L2 frame interval types
const V4L2_FRMIVAL_TYPE_DISCRETE: u32 = 1;

// MJPEG fourcc
const V4L2_PIX_FMT_MJPEG: u32 =
    (b'M' as u32) | ((b'J' as u32) << 8) | ((b'P' as u32) << 16) | ((b'G' as u32) << 24);

/// V4L2 format descriptor for VIDIOC_ENUM_FMT
#[repr(C)]
struct V4l2FmtDesc {
    index: u32,
    type_: u32,
    flags: u32,
    description: [u8; 32],
    pixelformat: u32,
    mbus_code: u32,
    reserved: [u32; 3],
}

// Verify struct matches kernel layout (64 bytes)
const _: () = assert!(std::mem::size_of::<V4l2FmtDesc>() == 64);

/// V4L2 frame size enumerator for VIDIOC_ENUM_FRAMESIZES
#[repr(C)]
struct V4l2FrmSizeEnum {
    index: u32,
    pixel_format: u32,
    type_: u32,
    // union, for discrete (type=1): width, height
    width: u32,
    height: u32,
    _padding: [u8; 16],
    reserved: [u32; 2],
}

// Verify struct matches kernel layout (44 bytes)
const _: () = assert!(std::mem::size_of::<V4l2FrmSizeEnum>() == 44);

/// V4L2 frame interval enumerator for VIDIOC_ENUM_FRAMEINTERVALS
#[repr(C)]
struct V4l2FrmIvalEnum {
    index: u32,
    pixel_format: u32,
    width: u32,
    height: u32,
    type_: u32,
    // union, for discrete (type=1): numerator, denominator
    numerator: u32,
    denominator: u32,
    _padding: [u8; 16],
    reserved: [u32; 2],
}

// Verify struct matches kernel layout (52 bytes)
const _: () = assert!(std::mem::size_of::<V4l2FrmIvalEnum>() == 52);

// VIDIOC_ENUM_FMT = _IOWR('V', 2, struct v4l2_fmtdesc)
nix::ioctl_readwrite!(vidioc_enum_fmt, b'V', 2, V4l2FmtDesc);
// VIDIOC_ENUM_FRAMESIZES = _IOWR('V', 74, struct v4l2_frmsizeenum)
nix::ioctl_readwrite!(vidioc_enum_framesizes, b'V', 74, V4l2FrmSizeEnum);
// VIDIOC_ENUM_FRAMEINTERVALS = _IOWR('V', 75, struct v4l2_frmivalenum)
nix::ioctl_readwrite!(vidioc_enum_frameintervals, b'V', 75, V4l2FrmIvalEnum);

#[repr(C)]
struct V4l2Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

// Verify struct matches kernel layout (104 bytes)
const _: () = assert!(std::mem::size_of::<V4l2Capability>() == 104);

// VIDIOC_QUERYCAP = _IOR('V', 0, struct v4l2_capability)
nix::ioctl_read!(vidioc_querycap, b'V', 0, V4l2Capability);

fn query_cap(index: u32) -> Option<V4l2Capability> {
    let path = format!("/dev/video{}", index);
    let file = OpenOptions::new().read(true).open(&path).ok()?;
    let mut cap: V4l2Capability = unsafe { std::mem::zeroed() };
    unsafe { vidioc_querycap(file.as_raw_fd(), &mut cap).ok()? };
    Some(cap)
}

fn cap_driver(cap: &V4l2Capability) -> &str {
    let len = cap.driver.iter().position(|&b| b == 0).unwrap_or(cap.driver.len());
    std::str::from_utf8(&cap.driver[..len]).unwrap_or("")
}

fn cap_bus_info(cap: &V4l2Capability) -> &str {
    let len = cap.bus_info.iter().position(|&b| b == 0).unwrap_or(cap.bus_info.len());
    std::str::from_utf8(&cap.bus_info[..len]).unwrap_or("")
}

fn effective_caps(cap: &V4l2Capability) -> u32 {
    if cap.capabilities & V4L2_CAP_DEVICE_CAPS != 0 {
        cap.device_caps
    } else {
        cap.capabilities
    }
}

fn is_loopback(cap: &V4l2Capability) -> bool {
    cap_driver(cap).contains("v4l2 loopback")
        || cap_bus_info(cap).starts_with("platform:v4l2loopback-")
}

fn is_capture(cap: &V4l2Capability) -> bool {
    let caps = effective_caps(cap);
    caps & V4L2_CAP_VIDEO_CAPTURE != 0 || caps & V4L2_CAP_VIDEO_CAPTURE_MPLANE != 0
}

/// Find the first real capture camera, skipping loopback and the configured output device.
pub fn detect_camera(output_device: &str) -> Option<u32> {
    for index in 0..64 {
        let dev_path = format!("/dev/video{}", index);
        if dev_path == output_device {
            continue;
        }
        if let Some(cap) = query_cap(index) {
            if is_loopback(&cap) || !is_capture(&cap) {
                continue;
            }
            return Some(index);
        }
    }
    None
}

/// Get the human-readable name (card field) for a video device.
pub fn device_name(index: u32) -> Option<String> {
    let cap = query_cap(index)?;
    let len = cap
        .card
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(cap.card.len());
    Some(String::from_utf8_lossy(&cap.card[..len]).into_owned())
}

pub struct CameraInfo {
    pub index: u32,
    pub name: String,
}

/// List all real capture cameras, skipping loopback and the configured output device.
pub fn list_cameras(output_device: &str) -> Vec<CameraInfo> {
    let mut cameras = Vec::new();
    for index in 0..64 {
        let dev_path = format!("/dev/video{}", index);
        if dev_path == output_device {
            continue;
        }
        if let Some(cap) = query_cap(index) {
            if is_loopback(&cap) || !is_capture(&cap) {
                continue;
            }
            let len = cap
                .card
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(cap.card.len());
            let name = String::from_utf8_lossy(&cap.card[..len]).into_owned();
            cameras.push(CameraInfo { index, name });
        }
    }
    cameras
}

/// Find the MJPEG fourcc for a capture device by enumerating pixel formats.
fn find_mjpeg_fourcc(fd: std::os::unix::io::RawFd) -> Option<u32> {
    for i in 0u32.. {
        let mut desc: V4l2FmtDesc = unsafe { std::mem::zeroed() };
        desc.index = i;
        desc.type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE;
        if unsafe { vidioc_enum_fmt(fd, &mut desc) }.is_err() {
            break;
        }
        if desc.pixelformat == V4L2_PIX_FMT_MJPEG {
            return Some(desc.pixelformat);
        }
    }
    None
}

/// List all supported resolutions for a camera (MJPEG format, discrete sizes).
/// Returns sorted by pixel count (largest first). Empty on error.
pub fn list_resolutions(camera_index: u32) -> Vec<(u32, u32)> {
    let path = format!("/dev/video{}", camera_index);
    let file = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let fd = file.as_raw_fd();

    let fourcc = match find_mjpeg_fourcc(fd) {
        Some(f) => f,
        None => return Vec::new(),
    };

    let mut resolutions = Vec::new();
    for i in 0u32.. {
        let mut frmsize: V4l2FrmSizeEnum = unsafe { std::mem::zeroed() };
        frmsize.index = i;
        frmsize.pixel_format = fourcc;
        if unsafe { vidioc_enum_framesizes(fd, &mut frmsize) }.is_err() {
            break;
        }
        if frmsize.type_ == V4L2_FRMSIZE_TYPE_DISCRETE {
            resolutions.push((frmsize.width, frmsize.height));
        }
    }

    resolutions.sort_by(|a, b| {
        let pa = (a.0 as u64) * (a.1 as u64);
        let pb = (b.0 as u64) * (b.1 as u64);
        pb.cmp(&pa)
    });
    resolutions.dedup();
    resolutions
}

/// Query the maximum FPS for a given resolution (MJPEG format).
/// Returns None on error or if no discrete intervals are reported.
pub fn max_fps_for_resolution(camera_index: u32, width: u32, height: u32) -> Option<u32> {
    let path = format!("/dev/video{}", camera_index);
    let file = OpenOptions::new().read(true).write(true).open(&path).ok()?;
    let fd = file.as_raw_fd();

    let fourcc = find_mjpeg_fourcc(fd)?;

    let mut max_fps: Option<u32> = None;
    for i in 0u32.. {
        let mut frmival: V4l2FrmIvalEnum = unsafe { std::mem::zeroed() };
        frmival.index = i;
        frmival.pixel_format = fourcc;
        frmival.width = width;
        frmival.height = height;
        if unsafe { vidioc_enum_frameintervals(fd, &mut frmival) }.is_err() {
            break;
        }
        if frmival.type_ == V4L2_FRMIVAL_TYPE_DISCRETE && frmival.numerator > 0 {
            let fps = frmival.denominator / frmival.numerator;
            max_fps = Some(max_fps.map_or(fps, |prev| prev.max(fps)));
        }
    }
    max_fps
}
