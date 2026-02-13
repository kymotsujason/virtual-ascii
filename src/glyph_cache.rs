use std::collections::HashMap;

static FONT_ASCII: &[u8] = include_bytes!("../fonts/SourceCodePro-Regular.ttf");
static FONT_MATRIX: &[u8] = include_bytes!("../fonts/MatrixGlyphs.otf");

/// Pre-dilated glyph variant with expanded bounding box
#[derive(Debug)]
pub struct GlowGlyph {
    pub coverage: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub xmin: i32,
    pub ymin: i32,
}

#[derive(Debug)]
pub struct GlyphBitmap {
    /// Alpha coverage values (0-255), row-major
    pub coverage: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// Offset from cell origin to bitmap start
    pub xmin: i32,
    pub ymin: i32,
    /// 2x dilated variant for per-intensity glow (matrix mode only)
    pub glow: Option<GlowGlyph>,
}

pub struct GlyphCache {
    glyphs: HashMap<char, GlyphBitmap>,
    /// Uniform cell width (max across all glyphs)
    pub cell_width: usize,
    /// Uniform cell height (max across all glyphs)
    pub cell_height: usize,
    /// Font ascent in pixels (baseline to top of tallest glyph)
    pub ascent: f32,
}

/// Horizontally flip a coverage bitmap (row-by-row pixel reversal)
fn mirror_bitmap(coverage: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut mirrored = vec![0u8; coverage.len()];
    for y in 0..height {
        for x in 0..width {
            mirrored[y * width + (width - 1 - x)] = coverage[y * width + x];
        }
    }
    mirrored
}

/// Full morphological dilation with 3x3 max kernel, expanding bounds by 1px on each side.
/// Returns (coverage, new_width, new_height). Caller adjusts xmin/ymin by -1.
fn dilate_expand(src: &[u8], w: usize, h: usize) -> (Vec<u8>, usize, usize) {
    let new_w = w + 2;
    let new_h = h + 2;
    let mut dst = vec![0u8; new_w * new_h];

    for dy in 0..new_h {
        for dx in 0..new_w {
            // Center in source coordinates
            let cx = dx as i32 - 1;
            let cy = dy as i32 - 1;
            let mut max_val: u8 = 0;
            for ky in -1..=1i32 {
                let sy = cy + ky;
                if sy < 0 || sy as usize >= h {
                    continue;
                }
                for kx in -1..=1i32 {
                    let sx = cx + kx;
                    if sx < 0 || sx as usize >= w {
                        continue;
                    }
                    max_val = max_val.max(src[sy as usize * w + sx as usize]);
                }
            }
            dst[dy * new_w + dx] = max_val;
        }
    }

    (dst, new_w, new_h)
}

/// Soft dilation: each pixel becomes max of itself and half of its strongest neighbor.
/// This thickens strokes by ~0.5px while preserving anti-aliased edges.
fn bolden_coverage(coverage: &mut [u8], width: usize, height: usize) {
    if width == 0 || height == 0 {
        return;
    }
    let src = coverage.to_vec();
    for y in 0..height {
        for x in 0..width {
            let orig = src[y * width + x] as u16;
            let mut nmax = 0u16;
            if x > 0 {
                nmax = nmax.max(src[y * width + x - 1] as u16);
            }
            if x + 1 < width {
                nmax = nmax.max(src[y * width + x + 1] as u16);
            }
            if y > 0 {
                nmax = nmax.max(src[(y - 1) * width + x] as u16);
            }
            if y + 1 < height {
                nmax = nmax.max(src[(y + 1) * width + x] as u16);
            }
            coverage[y * width + x] = orig.max(nmax / 2).min(255) as u8;
        }
    }
}

impl GlyphCache {
    pub fn new(charset: &[char], font_size: f32, mirror_glyphs: bool, bold: bool) -> Result<Self, String> {
        let font_data = if mirror_glyphs {
            FONT_MATRIX
        } else {
            FONT_ASCII
        };
        let font = fontdue::Font::from_bytes(font_data, fontdue::FontSettings::default())
            .map_err(|e| format!("Failed to load font: {}", e))?;

        let mut glyphs = HashMap::new();
        let mut max_width: usize = 0;
        let mut max_height: usize = 0;

        let line_metrics = font.horizontal_line_metrics(font_size);
        let ascent = line_metrics.map(|m| m.ascent).unwrap_or(font_size * 0.8);
        let cell_height_from_metrics = line_metrics
            .map(|m| (m.ascent - m.descent).ceil() as usize)
            .unwrap_or(0);

        // First pass: rasterize all glyphs, track max dimensions
        let mut raw_glyphs: Vec<(char, fontdue::Metrics, Vec<u8>)> = Vec::new();
        for &ch in charset {
            let (metrics, coverage) = font.rasterize(ch, font_size);
            if metrics.width > max_width {
                max_width = metrics.width;
            }
            if metrics.height > max_height {
                max_height = metrics.height;
            }
            raw_glyphs.push((ch, metrics, coverage));
        }

        // Use font line metrics for height if available, otherwise fall back to max glyph height
        let cell_height = if cell_height_from_metrics > max_height {
            cell_height_from_metrics
        } else {
            max_height
        };

        // For monospace font, advance_width should be consistent
        let cell_width = if let Some(first_char) = charset.first() {
            let metrics = font.metrics(*first_char, font_size);
            metrics.advance_width.ceil() as usize
        } else {
            max_width
        };

        // Ensure minimum dimensions
        let cell_width = cell_width.max(max_width).max(1);
        let cell_height = cell_height.max(1);

        // Second pass: build bitmaps (with optional mirroring)
        for (ch, metrics, coverage) in raw_glyphs {
            let mut bitmap = if mirror_glyphs && metrics.width > 0 && metrics.height > 0 {
                let mirrored = mirror_bitmap(&coverage, metrics.width, metrics.height);
                let new_xmin = cell_width as i32 - metrics.xmin - metrics.width as i32;
                GlyphBitmap {
                    coverage: mirrored,
                    width: metrics.width,
                    height: metrics.height,
                    xmin: new_xmin,
                    ymin: metrics.ymin,
                    glow: None,
                }
            } else {
                GlyphBitmap {
                    coverage,
                    width: metrics.width,
                    height: metrics.height,
                    xmin: metrics.xmin,
                    ymin: metrics.ymin,
                    glow: None,
                }
            };

            if bold && bitmap.width > 0 && bitmap.height > 0 {
                // Static bolden for baseline thickness
                bolden_coverage(&mut bitmap.coverage, bitmap.width, bitmap.height);

                // Pre-compute 2x dilated glow for per-intensity thickening
                let (d1, w1, h1) =
                    dilate_expand(&bitmap.coverage, bitmap.width, bitmap.height);
                let (d2, w2, h2) = dilate_expand(&d1, w1, h1);
                bitmap.glow = Some(GlowGlyph {
                    coverage: d2,
                    width: w2,
                    height: h2,
                    xmin: bitmap.xmin - 2,
                    ymin: bitmap.ymin - 2,
                });
            }

            glyphs.insert(ch, bitmap);
        }

        // Validate: warn about missing glyphs (fontdue falls back to default glyph)
        for &ch in charset {
            if let Some(glyph) = glyphs.get(&ch) {
                if ch != ' ' && glyph.coverage.is_empty() && glyph.width == 0 {
                    eprintln!(
                        "Warning: glyph for '{}' (U+{:04X}) may be missing from font",
                        ch, ch as u32
                    );
                }
            }
        }

        Ok(GlyphCache {
            glyphs,
            cell_width,
            cell_height,
            ascent,
        })
    }

    pub fn get(&self, ch: char) -> Option<&GlyphBitmap> {
        self.glyphs.get(&ch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glyph_cache_basic() {
        let charset: Vec<char> = " .:#@".chars().collect();
        let cache = GlyphCache::new(&charset, 16.0, false, false).expect("Failed to create glyph cache");

        // All chars should be present
        for ch in &charset {
            assert!(cache.get(*ch).is_some(), "Missing glyph for '{}'", ch);
        }

        // Cell dimensions should be positive
        assert!(cache.cell_width > 0, "cell_width should be > 0");
        assert!(cache.cell_height > 0, "cell_height should be > 0");

        // Non-space chars should have non-empty coverage
        let at = cache.get('@').unwrap();
        assert!(at.width > 0);
        assert!(at.height > 0);
        assert!(!at.coverage.is_empty());
    }

    #[test]
    fn test_mirror_bitmap() {
        // 3x2 bitmap: [1,2,3, 4,5,6]
        let coverage = vec![1, 2, 3, 4, 5, 6];
        let mirrored = mirror_bitmap(&coverage, 3, 2);
        // Row 0: [3,2,1], Row 1: [6,5,4]
        assert_eq!(mirrored, vec![3, 2, 1, 6, 5, 4]);
    }

    #[test]
    fn test_matrix_font_loads() {
        // Verify the matrix font can be loaded with katakana characters
        let charset: Vec<char> = "ｦｧｨｩｪ0123456789".chars().collect();
        let cache = GlyphCache::new(&charset, 16.0, true, true).expect("Failed to create matrix glyph cache");

        assert!(cache.cell_width > 0);
        assert!(cache.cell_height > 0);

        // Katakana should have non-empty coverage
        let wo = cache.get('ｦ').unwrap();
        assert!(wo.width > 0, "Katakana glyph should have width");
        assert!(wo.height > 0, "Katakana glyph should have height");
    }
}
