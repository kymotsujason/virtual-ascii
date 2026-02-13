use clap::{Args, Parser, Subcommand};

use crate::detect;

pub fn parse_resolution(s: &str) -> Result<(u32, u32), String> {
    let (w_str, h_str) = s
        .split_once('x')
        .or_else(|| s.split_once('X'))
        .ok_or_else(|| format!("invalid resolution '{}': expected WxH (e.g. 1920x1080)", s))?;
    // Strip trailing non-digit content (e.g., "1080 (60fps)" -> "1080")
    let w_digits: &str = w_str.trim().split(|c: char| !c.is_ascii_digit()).next().unwrap_or(w_str);
    let h_digits: &str = h_str.trim().split(|c: char| !c.is_ascii_digit()).next().unwrap_or(h_str);
    let w: u32 = w_digits
        .parse()
        .map_err(|_| format!("invalid width '{}': must be a positive integer", w_str))?;
    let h: u32 = h_digits
        .parse()
        .map_err(|_| format!("invalid height '{}': must be a positive integer", h_str))?;
    if w == 0 || h == 0 {
        return Err("width and height must be non-zero".into());
    }
    Ok((w, h))
}

#[derive(Parser, Debug)]
#[command(
    name = "virtual-ascii",
    about = "Convert webcam feed to ASCII art virtual camera",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<SubCommand>,

    #[command(flatten)]
    pub run: RunArgs,
}

#[derive(Subcommand, Debug)]
pub enum SubCommand {
    /// Change settings on a running instance
    Set(SetArgs),
    /// Query current settings from a running instance
    Status,
    #[cfg(feature = "gui")]
    /// Launch the graphical interface
    Gui,
}

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Definition level (1=blocky, 10=ultra-fine)
    #[arg(short, long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(1..=10))]
    pub definition: u8,

    /// Color theme
    #[arg(short, long, default_value = "matrix")]
    pub theme: String,

    /// Target FPS
    #[arg(short, long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=240))]
    pub fps: u32,

    /// Resolution WxH (e.g. 1920x1080). Auto-selects highest if omitted
    #[arg(short = 'r', long, value_parser = parse_resolution)]
    pub resolution: Option<(u32, u32)>,

    /// Webcam device index (auto-detected if not specified)
    #[arg(short = 'i', long)]
    pub camera_index: Option<u32>,

    /// V4L2 loopback device path
    #[arg(short = 'o', long, default_value = "/dev/video20")]
    pub output_device: String,

    /// Override foreground color (hex, e.g. ff00ff)
    #[arg(long)]
    pub fg_color: Option<String>,

    /// Override background color (hex, e.g. 001100)
    #[arg(long)]
    pub bg_color: Option<String>,

    /// Brightness curve
    #[arg(short = 'c', long, default_value = "linear")]
    pub brightness_curve: String,

    /// Invert brightness mapping
    #[arg(long, default_value_t = false)]
    pub invert: bool,
}

#[derive(Args, Debug)]
pub struct SetArgs {
    /// Definition level (1=blocky, 10=ultra-fine)
    #[arg(short, long, value_parser = clap::value_parser!(u8).range(1..=10))]
    pub definition: Option<u8>,

    /// Color theme
    #[arg(short, long)]
    pub theme: Option<String>,

    /// Target FPS
    #[arg(short, long, value_parser = clap::value_parser!(u32).range(1..=240))]
    pub fps: Option<u32>,

    /// Resolution WxH (e.g. 1920x1080)
    #[arg(short = 'r', long, value_parser = parse_resolution)]
    pub resolution: Option<(u32, u32)>,

    /// Webcam device index
    #[arg(short = 'i', long)]
    pub camera_index: Option<u32>,

    /// Override foreground color (hex, e.g. ff00ff)
    #[arg(long)]
    pub fg_color: Option<String>,

    /// Override background color (hex, e.g. 001100)
    #[arg(long)]
    pub bg_color: Option<String>,

    /// Brightness curve
    #[arg(short = 'c', long)]
    pub brightness_curve: Option<String>,

    /// Invert brightness mapping
    #[arg(long)]
    pub invert: Option<bool>,
}

#[derive(Debug, Clone, Copy)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub fn to_hex(self) -> String {
        format!("{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

#[derive(Debug, Clone)]
pub struct ColorTheme {
    pub name: String,
    pub fg: Rgb,
    pub bg: Rgb,
}

impl ColorTheme {
    pub fn from_name(name: &str) -> Option<Self> {
        let (fg, bg) = match name {
            "mono" => (
                Rgb {
                    r: 255,
                    g: 255,
                    b: 255,
                },
                Rgb { r: 0, g: 0, b: 0 },
            ),
            "green" => (Rgb { r: 0, g: 255, b: 0 }, Rgb { r: 0, g: 10, b: 0 }),
            "amber" => (
                Rgb {
                    r: 255,
                    g: 176,
                    b: 0,
                },
                Rgb { r: 20, g: 10, b: 0 },
            ),
            "blue" => (
                Rgb {
                    r: 100,
                    g: 180,
                    b: 255,
                },
                Rgb { r: 0, g: 5, b: 20 },
            ),
            "matrix" => (Rgb { r: 0, g: 255, b: 0 }, Rgb { r: 0, g: 15, b: 0 }),
            "vaporwave" => (
                Rgb {
                    r: 255,
                    g: 100,
                    b: 255,
                },
                Rgb { r: 10, g: 0, b: 20 },
            ),
            "fire" => (
                Rgb {
                    r: 255,
                    g: 100,
                    b: 0,
                },
                Rgb { r: 20, g: 5, b: 0 },
            ),
            "color" => (
                Rgb {
                    r: 255,
                    g: 255,
                    b: 255,
                },
                Rgb { r: 0, g: 0, b: 0 },
            ),
            _ => return None,
        };
        Some(ColorTheme {
            name: name.to_string(),
            fg,
            bg,
        })
    }
}

pub fn parse_hex_color(hex: &str) -> Option<Rgb> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Rgb { r, g, b })
}

#[derive(Debug, Clone, Copy)]
pub enum BrightnessCurve {
    Linear,
    Exponential,
    Sigmoid,
}

impl BrightnessCurve {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "linear" => Some(Self::Linear),
            "exponential" | "exp" => Some(Self::Exponential),
            "sigmoid" => Some(Self::Sigmoid),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Exponential => "exponential",
            Self::Sigmoid => "sigmoid",
        }
    }

    /// Map a 0.0..=1.0 brightness value through the curve
    pub fn apply(self, t: f32) -> f32 {
        match self {
            Self::Linear => t,
            Self::Exponential => t * t,
            Self::Sigmoid => {
                let k = 10.0_f32;
                let raw = 1.0 / (1.0 + (-k * (t - 0.5)).exp());
                let min = 1.0 / (1.0 + (k * 0.5).exp());
                let max = 1.0 / (1.0 + (-k * 0.5).exp());
                (raw - min) / (max - min)
            }
        }
    }
}

/// Movie-authentic matrix character set: half-width katakana + numerals + symbols
pub fn matrix_charset() -> Vec<char> {
    let mut chars = Vec::new();
    // Half-width katakana (U+FF65-U+FF9F)
    for code in 0xFF65u32..=0xFF9F {
        if let Some(ch) = char::from_u32(code) {
            chars.push(ch);
        }
    }
    // Numerals + symbols
    chars.extend("0123456789*+:=.<>\"|¦_Z".chars());
    chars
}

/// Maps definition level 1-10 to (ascii_columns, charset).
/// For the "matrix" theme, returns the katakana charset; for others, ASCII charsets.
pub fn definition_to_params(level: u8, theme_name: &str) -> (u32, Vec<char>) {
    if theme_name == "matrix" {
        let columns = match level {
            1 => 40,
            2 => 50,
            3 => 60,
            4 => 70,
            5 => 80,
            6 => 100,
            7 => 120,
            8 => 140,
            9 => 160,
            10 => 200,
            _ => 80,
        };
        return (columns, matrix_charset());
    }

    let (columns, charset_str) = match level {
        1 => (40, " .:#"),
        2 => (50, " .-:=+#"),
        3 => (60, " .-:=+*#%@"),
        4 => (70, " .,-:;=+*#%@"),
        5 => (80, " .'`,-.:;=+*#%@"),
        6 => (100, " .'`^\",-.:;=!+*#%@"),
        7 => (
            120,
            " .'`^\",:;Il!i><~+_-?][}{1)(|/tfjrxnuvczXYUJCLQ0OZmwqpdbkhao*#MW&8%B@$",
        ),
        8 => (
            140,
            " .'`^\",:;Il!i><~+_-?][}{1)(|/tfjrxnuvczXYUJCLQ0OZmwqpdbkhao*#MW&8%B@$",
        ),
        9 => (
            160,
            " .'`^\",:;Il!i><~+_-?][}{1)(|/tfjrxnuvczXYUJCLQ0OZmwqpdbkhao*#MW&8%B@$",
        ),
        10 => (
            200,
            " .'`^\",:;Il!i><~+_-?][}{1)(|/tfjrxnuvczXYUJCLQ0OZmwqpdbkhao*#MW&8%B@$",
        ),
        _ => (80, " .'`,-.:;=+*#%@"),
    };
    (columns, charset_str.chars().collect())
}

#[derive(Debug)]
pub struct AppConfig {
    pub theme: ColorTheme,
    pub definition: u8,
    pub ascii_columns: u32,
    pub charset: Vec<char>,
    pub brightness_curve: BrightnessCurve,
    pub invert: bool,
    pub fps: u32,
    pub camera_index: u32,
    pub resolution: Option<(u32, u32)>,
    pub output_device: String,
}

impl AppConfig {
    pub fn from_cli(args: RunArgs) -> anyhow::Result<Self> {
        let mut theme = ColorTheme::from_name(&args.theme).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown theme '{}'. Available: mono, green, amber, blue, matrix, vaporwave, fire, color",
                args.theme
            )
        })?;

        if let Some(ref hex) = args.fg_color {
            theme.fg = parse_hex_color(hex).ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid foreground color '{}'. Use 6-digit hex (e.g. ff00ff)",
                    hex
                )
            })?;
        }
        if let Some(ref hex) = args.bg_color {
            theme.bg = parse_hex_color(hex).ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid background color '{}'. Use 6-digit hex (e.g. 001100)",
                    hex
                )
            })?;
        }

        let brightness_curve =
            BrightnessCurve::from_name(&args.brightness_curve).ok_or_else(|| {
                anyhow::anyhow!(
                    "Unknown brightness curve '{}'. Available: linear, exponential, sigmoid",
                    args.brightness_curve
                )
            })?;

        let (ascii_columns, charset) = definition_to_params(args.definition, &args.theme);

        let camera_index = match args.camera_index {
            Some(i) => i,
            None => {
                if let Some(i) = detect::detect_camera(&args.output_device) {
                    let name = detect::device_name(i).unwrap_or_default();
                    eprintln!("Auto-detected camera: /dev/video{} ({})", i, name);
                    i
                } else {
                    eprintln!("Warning: no camera auto-detected, falling back to index 0");
                    0
                }
            }
        };

        Ok(AppConfig {
            theme,
            definition: args.definition,
            ascii_columns,
            charset,
            brightness_curve,
            invert: args.invert,
            fps: args.fps,
            camera_index,
            resolution: args.resolution,
            output_device: args.output_device,
        })
    }
}

pub fn theme_names() -> &'static [&'static str] {
    &[
        "mono",
        "green",
        "amber",
        "blue",
        "matrix",
        "vaporwave",
        "fire",
        "color",
    ]
}
