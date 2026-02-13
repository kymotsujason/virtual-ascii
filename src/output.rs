use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;

// V4L2 constants
const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const V4L2_PIX_FMT_RGB24: u32 = fourcc(b'R', b'G', b'B', b'3');
const V4L2_FIELD_NONE: u32 = 1;

const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

// V4L2 format structs (minimal subset for our needs)
#[repr(C)]
#[derive(Copy, Clone)]
struct V4l2PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    // unions for encoding/quantization — zero them out
    encoding: u32,
    quantization: u32,
    xfer_func: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct V4l2Format {
    type_: u32,
    // 4 bytes alignment padding: the kernel's fmt union contains v4l2_window
    // which has pointer fields (8-byte aligned on x86_64), forcing the union
    // to start at offset 8, not offset 4.
    _align_pad: u32,
    fmt: V4l2PixFormat,
    // Remaining bytes to fill the 200-byte fmt union
    _padding: [u8; 200 - std::mem::size_of::<V4l2PixFormat>()],
}

// Verify struct matches kernel layout (208 bytes on x86_64)
const _: () = assert!(std::mem::size_of::<V4l2Format>() == 208);

// Generate ioctl wrapper using nix macros
// VIDIOC_S_FMT = _IOWR('V', 5, struct v4l2_format)
nix::ioctl_readwrite!(vidioc_s_fmt, b'V', 5, V4l2Format);

pub struct V4l2Output {
    file: File,
    width: u32,
    height: u32,
    frame_size: usize,
}

impl V4l2Output {
    pub fn new(device_path: &str, width: u32, height: u32) -> anyhow::Result<Self> {
        let path = Path::new(device_path);

        if !path.exists() {
            return Err(anyhow::anyhow!(
                "V4L2 loopback device '{}' not found.\n\
                 Hint: Load the v4l2loopback kernel module:\n\
                 sudo modprobe v4l2loopback devices=1 video_nr=20 exclusive_caps=1 card_label=\"Virtual ASCII\"",
                device_path
            ));
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(device_path)
            .map_err(|e| {
                anyhow::anyhow!(
                    "Cannot open '{}': {}.\n\
                     Hint: Check permissions. You may need to add your user to the 'video' group:\n\
                     sudo usermod -aG video $USER",
                    device_path,
                    e
                )
            })?;

        let bytesperline = width * 3; // RGB24 = 3 bytes per pixel
        let sizeimage = bytesperline * height;

        // Set the output format via VIDIOC_S_FMT
        let mut fmt = V4l2Format {
            type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
            _align_pad: 0,
            fmt: V4l2PixFormat {
                width,
                height,
                pixelformat: V4L2_PIX_FMT_RGB24,
                field: V4L2_FIELD_NONE,
                bytesperline,
                sizeimage,
                colorspace: 0,
                priv_: 0,
                flags: 0,
                encoding: 0,
                quantization: 0,
                xfer_func: 0,
            },
            _padding: [0u8; 200 - std::mem::size_of::<V4l2PixFormat>()],
        };

        eprintln!("V4L2: setting format {}x{} RGB24 on {}", width, height, device_path);

        let fd = file.as_raw_fd();
        unsafe {
            vidioc_s_fmt(fd, &mut fmt).map_err(|e| {
                anyhow::anyhow!(
                    "VIDIOC_S_FMT failed on '{}': {}.\n\
                     Hint: Is this actually a v4l2loopback device? Check: v4l2-ctl --device={} --all",
                    device_path, e, device_path
                )
            })?;
        }

        // Read back negotiated values
        let negotiated_w = fmt.fmt.width;
        let negotiated_h = fmt.fmt.height;
        if negotiated_w != width || negotiated_h != height {
            eprintln!(
                "Warning: v4l2loopback negotiated {}x{} (requested {}x{})",
                negotiated_w, negotiated_h, width, height
            );
        }

        let frame_size =
            (fmt.fmt.sizeimage as usize).max((negotiated_w * negotiated_h * 3) as usize);

        Ok(V4l2Output {
            file,
            width: negotiated_w,
            height: negotiated_h,
            frame_size,
        })
    }

    pub fn write_frame(&mut self, rgb_data: &[u8]) -> anyhow::Result<()> {
        let mut written = 0;
        let data = &rgb_data[..self.frame_size.min(rgb_data.len())];
        while written < data.len() {
            match self.file.write(&data[written..]) {
                Ok(0) => return Err(anyhow::anyhow!("Write to v4l2 device returned 0 bytes")),
                Ok(n) => written += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(anyhow::anyhow!("Write to v4l2 device failed: {}", e)),
            }
        }
        Ok(())
    }

    pub fn resolution(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}
