use std::time::Instant;

use crate::config::{BrightnessCurve, Rgb};
use crate::glyph_cache::GlyphCache;
use crate::rain::MatrixRainState;

const BLOOM_DS_FACTOR: usize = 4;
const BLOOM_BLUR_RADIUS: usize = 12;
const BLOOM_BLUR_PASSES: usize = 3;
const BLOOM_STRENGTH: f32 = 1.0;
const BLOOM_THRESHOLD: u8 = 12;

pub struct AsciiRenderer {
    glyph_cache: GlyphCache,
    charset: Vec<char>,
    fg: Rgb,
    bg: Rgb,
    brightness_curve: BrightnessCurve,
    invert: bool,
    pub output_width: u32,
    pub output_height: u32,
    ascii_cols: u32,
    ascii_rows: u32,
    /// Font ascent in pixels (for glyph placement within cell)
    ascent: f32,
    rain_state: Option<MatrixRainState>,
    last_render: Instant,
    bloom_buf: Vec<u16>,
    bloom_tmp: Vec<u16>,
    is_color_mode: bool,
}

impl AsciiRenderer {
    pub fn new(
        charset: &[char],
        fg: Rgb,
        bg: Rgb,
        brightness_curve: BrightnessCurve,
        invert: bool,
        output_width: u32,
        output_height: u32,
        ascii_columns: u32,
        theme_name: &str,
    ) -> Result<Self, String> {
        // Probe the font at a reference size to find the width-to-size ratio,
        // then compute the font_size that makes ascii_columns fill output_width.
        let probe_size = 100.0_f32;
        let mirror = theme_name == "matrix";
        let bold = theme_name == "matrix";
        let probe_cache = GlyphCache::new(charset, probe_size, mirror, false)?;
        let advance_per_unit = probe_cache.cell_width as f32 / probe_size;

        let desired_cell_width = output_width as f32 / ascii_columns as f32;
        let font_size = (desired_cell_width / advance_per_unit).max(6.0);

        let glyph_cache = GlyphCache::new(charset, font_size, mirror, bold)?;

        let cell_w = glyph_cache.cell_width as u32;
        let cell_h = glyph_cache.cell_height as u32;

        if cell_w == 0 || cell_h == 0 {
            return Err("Glyph cell dimensions are zero".to_string());
        }

        // Compute actual grid dimensions that fit in the output
        let ascii_cols = ascii_columns.min(output_width / cell_w);
        let ascii_rows = output_height / cell_h;

        if ascii_cols == 0 || ascii_rows == 0 {
            return Err(format!(
                "Output {}x{} too small for font size {:.1} (cell {}x{})",
                output_width, output_height, font_size, cell_w, cell_h
            ));
        }

        let ascent = glyph_cache.ascent;

        let is_matrix = theme_name == "matrix";
        let rain_state = if is_matrix {
            Some(MatrixRainState::new(
                ascii_cols,
                ascii_rows,
                charset.len(),
                true,
            ))
        } else {
            None
        };

        let ds_w = output_width as usize / BLOOM_DS_FACTOR;
        let ds_h = output_height as usize / BLOOM_DS_FACTOR;
        let bloom_buf = vec![0u16; ds_w * ds_h * 3];
        let bloom_tmp = vec![0u16; ds_w * ds_h * 3];

        let is_color_mode = theme_name == "color";

        Ok(AsciiRenderer {
            glyph_cache,
            charset: charset.to_vec(),
            fg,
            bg,
            brightness_curve,
            invert,
            output_width,
            output_height,
            ascii_cols,
            ascii_rows,
            ascent,
            rain_state,
            last_render: Instant::now(),
            bloom_buf,
            bloom_tmp,
            is_color_mode,
        })
    }

    /// Convert an RGB frame to an ASCII-art RGB frame
    pub fn render(&mut self, rgb_frame: &[u8], frame_width: u32, frame_height: u32) -> Vec<u8> {
        let out_w = self.output_width as usize;
        let out_h = self.output_height as usize;
        let mut output = vec![0u8; out_w * out_h * 3];

        // Fill background
        for pixel in output.chunks_exact_mut(3) {
            pixel[0] = self.bg.r;
            pixel[1] = self.bg.g;
            pixel[2] = self.bg.b;
        }

        // Guard against short/malformed frames from the camera
        let expected = (frame_width as usize) * (frame_height as usize) * 3;
        if rgb_frame.len() < expected {
            return output;
        }

        // Step 1: Convert to grayscale
        let grayscale = rgb_to_grayscale(rgb_frame, frame_width, frame_height);

        // Step 2: Downsample to ASCII grid (sqrt lifts midtones for all themes)
        let grid: Vec<f32> = self.downsample_to_grid(&grayscale, frame_width, frame_height)
            .into_iter()
            .map(|b| b.sqrt())
            .collect();

        if self.rain_state.is_some() {
            // Rain path: advance simulation, compute cells, composite
            let now = Instant::now();
            let dt = now.duration_since(self.last_render).as_secs_f32();
            self.last_render = now;

            let rain = self.rain_state.as_mut().unwrap();
            rain.advance(dt);

            // Re-borrow as immutable for compute_cells
            let rain = self.rain_state.as_ref().unwrap();
            let cells = rain.compute_cells(
                &grid,
                &self.charset,
                self.brightness_curve,
                self.invert,
                self.fg,
            );

            self.composite_rain_glyphs(&cells, &mut output);
            apply_bloom(
                &mut output,
                &mut self.bloom_buf,
                &mut self.bloom_tmp,
                out_w,
                out_h,
            );
        } else if self.is_color_mode {
            // Color mode: per-cell webcam color
            let color_grid = self.downsample_to_color_grid(rgb_frame, frame_width, frame_height);
            let chars = self.map_to_characters(&grid);
            let cells: Vec<crate::rain::CellRender> = grid
                .iter()
                .zip(chars.iter())
                .zip(color_grid.iter())
                .map(|((&brightness, &ch), color)| {
                    let mut t = self.brightness_curve.apply(brightness);
                    if self.invert {
                        t = 1.0 - t;
                    }
                    crate::rain::CellRender {
                        ch,
                        color: *color,
                        intensity: t,
                    }
                })
                .collect();
            self.composite_rain_glyphs(&cells, &mut output);
        } else {
            // Normal path: map brightness to characters and composite
            let chars = self.map_to_characters(&grid);
            self.composite_glyphs(&chars, &mut output);
        }

        output
    }

    fn downsample_to_grid(&self, gray: &[u8], src_w: u32, src_h: u32) -> Vec<f32> {
        let cols = self.ascii_cols as usize;
        let rows = self.ascii_rows as usize;
        let mut grid = vec![0.0f32; cols * rows];

        let cell_src_w = src_w as f32 / cols as f32;
        let cell_src_h = src_h as f32 / rows as f32;

        for row in 0..rows {
            for col in 0..cols {
                let x0 = (col as f32 * cell_src_w) as usize;
                let y0 = (row as f32 * cell_src_h) as usize;
                let x1 = ((col + 1) as f32 * cell_src_w) as usize;
                let y1 = ((row + 1) as f32 * cell_src_h) as usize;

                let x1 = x1.min(src_w as usize);
                let y1 = y1.min(src_h as usize);

                let mut sum: u32 = 0;
                let mut count: u32 = 0;
                for y in y0..y1 {
                    for x in x0..x1 {
                        sum += gray[y * src_w as usize + x] as u32;
                        count += 1;
                    }
                }

                let avg = if count > 0 {
                    sum as f32 / count as f32 / 255.0
                } else {
                    0.0
                };
                grid[row * cols + col] = avg;
            }
        }

        grid
    }

    fn downsample_to_color_grid(&self, rgb: &[u8], src_w: u32, src_h: u32) -> Vec<Rgb> {
        let cols = self.ascii_cols as usize;
        let rows = self.ascii_rows as usize;
        let mut grid = Vec::with_capacity(cols * rows);

        let cell_src_w = src_w as f32 / cols as f32;
        let cell_src_h = src_h as f32 / rows as f32;

        for row in 0..rows {
            for col in 0..cols {
                let x0 = (col as f32 * cell_src_w) as usize;
                let y0 = (row as f32 * cell_src_h) as usize;
                let x1 = ((col + 1) as f32 * cell_src_w).min(src_w as f32) as usize;
                let y1 = ((row + 1) as f32 * cell_src_h).min(src_h as f32) as usize;

                let mut sum_r: u32 = 0;
                let mut sum_g: u32 = 0;
                let mut sum_b: u32 = 0;
                let mut count: u32 = 0;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let idx = (y * src_w as usize + x) * 3;
                        sum_r += rgb[idx] as u32;
                        sum_g += rgb[idx + 1] as u32;
                        sum_b += rgb[idx + 2] as u32;
                        count += 1;
                    }
                }

                if count > 0 {
                    grid.push(Rgb {
                        r: (sum_r / count) as u8,
                        g: (sum_g / count) as u8,
                        b: (sum_b / count) as u8,
                    });
                } else {
                    grid.push(Rgb { r: 0, g: 0, b: 0 });
                }
            }
        }
        grid
    }

    fn map_to_characters(&self, grid: &[f32]) -> Vec<char> {
        let n = self.charset.len();
        if n == 0 {
            return vec![' '; grid.len()];
        }

        grid.iter()
            .map(|&brightness| {
                let mut t = self.brightness_curve.apply(brightness);
                if self.invert {
                    t = 1.0 - t;
                }
                let idx = (t * (n - 1) as f32).round() as usize;
                self.charset[idx.min(n - 1)]
            })
            .collect()
    }

    fn composite_glyphs(&self, chars: &[char], output: &mut [u8]) {
        let out_w = self.output_width as usize;
        let cell_w = self.glyph_cache.cell_width;
        let cell_h = self.glyph_cache.cell_height;
        let cols = self.ascii_cols as usize;
        let rows = self.ascii_rows as usize;
        let ascent = self.ascent;

        for row in 0..rows {
            for col in 0..cols {
                let ch = chars[row * cols + col];

                // Skip space characters (they're just background)
                if ch == ' ' {
                    continue;
                }

                let glyph = match self.glyph_cache.get(ch) {
                    Some(g) => g,
                    None => continue,
                };

                if glyph.width == 0 || glyph.height == 0 {
                    continue;
                }

                // Cell top-left in output
                let cell_x = col * cell_w;
                let cell_y = row * cell_h;

                // Glyph position within cell:
                // x: offset by xmin (horizontal bearing)
                // y: ascent - ymin - height gives top of glyph from top of cell
                let glyph_x = cell_x as i32 + glyph.xmin;
                let glyph_y = cell_y as i32 + (ascent as i32 - glyph.ymin - glyph.height as i32);

                // Blit glyph with alpha blending
                for gy in 0..glyph.height {
                    let out_y = glyph_y + gy as i32;
                    if out_y < 0 || out_y >= self.output_height as i32 {
                        continue;
                    }

                    for gx in 0..glyph.width {
                        let out_x = glyph_x + gx as i32;
                        if out_x < 0 || out_x >= self.output_width as i32 {
                            continue;
                        }

                        let alpha = glyph.coverage[gy * glyph.width + gx];
                        if alpha == 0 {
                            continue;
                        }

                        let idx = (out_y as usize * out_w + out_x as usize) * 3;
                        if alpha == 255 {
                            output[idx] = self.fg.r;
                            output[idx + 1] = self.fg.g;
                            output[idx + 2] = self.fg.b;
                        } else {
                            let a = alpha as u16;
                            let inv_a = 255 - a;
                            output[idx] =
                                ((self.fg.r as u16 * a + output[idx] as u16 * inv_a) / 255) as u8;
                            output[idx + 1] = ((self.fg.g as u16 * a
                                + output[idx + 1] as u16 * inv_a)
                                / 255) as u8;
                            output[idx + 2] = ((self.fg.b as u16 * a
                                + output[idx + 2] as u16 * inv_a)
                                / 255) as u8;
                        }
                    }
                }
            }
        }
    }

    fn composite_rain_glyphs(
        &self,
        cells: &[crate::rain::CellRender],
        output: &mut [u8],
    ) {
        let out_w = self.output_width as usize;
        let cell_w = self.glyph_cache.cell_width;
        let cell_h = self.glyph_cache.cell_height;
        let cols = self.ascii_cols as usize;
        let rows = self.ascii_rows as usize;
        let ascent = self.ascent;

        for row in 0..rows {
            for col in 0..cols {
                let cell = &cells[row * cols + col];

                if cell.ch == ' ' || cell.intensity < 0.005 {
                    continue;
                }

                let glyph = match self.glyph_cache.get(cell.ch) {
                    Some(g) => g,
                    None => continue,
                };

                if glyph.width == 0 || glyph.height == 0 {
                    continue;
                }

                // Compute effective color: cell color scaled by intensity
                let eff_r = (cell.color.r as f32 * cell.intensity) as u16;
                let eff_g = (cell.color.g as f32 * cell.intensity) as u16;
                let eff_b = (cell.color.b as f32 * cell.intensity) as u16;

                let cell_x = col * cell_w;
                let cell_y = row * cell_h;

                // Per-intensity glow: bright characters get progressively thicker
                // by blending from base glyph toward a pre-dilated (expanded) variant.
                if let Some(ref glow) = glyph.glow {
                    if cell.intensity > 0.2 {
                        let glow_blend = ((cell.intensity - 0.2) / 0.8).min(1.0);
                        let glow_blend = glow_blend * glow_blend;

                        let glow_x = cell_x as i32 + glow.xmin;
                        let glow_y = cell_y as i32
                            + (ascent as i32 - glow.ymin - glow.height as i32);

                        // Offset from glow bitmap coords to base bitmap coords
                        let expand = (glyph.xmin - glow.xmin) as i32;

                        for gy in 0..glow.height {
                            let out_y = glow_y + gy as i32;
                            if out_y < 0 || out_y >= self.output_height as i32 {
                                continue;
                            }

                            for gx in 0..glow.width {
                                let out_x = glow_x + gx as i32;
                                if out_x < 0 || out_x >= self.output_width as i32 {
                                    continue;
                                }

                                let glow_a = glow.coverage[gy * glow.width + gx] as f32;

                                // Base alpha at corresponding position
                                let bx = gx as i32 - expand;
                                let by = gy as i32 - expand;
                                let base_a = if bx >= 0
                                    && (bx as usize) < glyph.width
                                    && by >= 0
                                    && (by as usize) < glyph.height
                                {
                                    glyph.coverage[by as usize * glyph.width + bx as usize] as f32
                                } else {
                                    0.0
                                };

                                let alpha =
                                    (base_a + (glow_a - base_a) * glow_blend) as u16;
                                if alpha == 0 {
                                    continue;
                                }

                                let idx =
                                    (out_y as usize * out_w + out_x as usize) * 3;
                                let inv_a = 255 - alpha;
                                output[idx] = ((eff_r * alpha
                                    + output[idx] as u16 * inv_a)
                                    / 255)
                                    as u8;
                                output[idx + 1] = ((eff_g * alpha
                                    + output[idx + 1] as u16 * inv_a)
                                    / 255)
                                    as u8;
                                output[idx + 2] = ((eff_b * alpha
                                    + output[idx + 2] as u16 * inv_a)
                                    / 255)
                                    as u8;
                            }
                        }
                        continue;
                    }
                }

                // Normal glyph compositing (low intensity or no glow variant)
                let glyph_x = cell_x as i32 + glyph.xmin;
                let glyph_y =
                    cell_y as i32 + (ascent as i32 - glyph.ymin - glyph.height as i32);

                for gy in 0..glyph.height {
                    let out_y = glyph_y + gy as i32;
                    if out_y < 0 || out_y >= self.output_height as i32 {
                        continue;
                    }

                    for gx in 0..glyph.width {
                        let out_x = glyph_x + gx as i32;
                        if out_x < 0 || out_x >= self.output_width as i32 {
                            continue;
                        }

                        let alpha = glyph.coverage[gy * glyph.width + gx] as u16;
                        if alpha == 0 {
                            continue;
                        }

                        let idx = (out_y as usize * out_w + out_x as usize) * 3;
                        let inv_a = 255 - alpha;
                        output[idx] =
                            ((eff_r * alpha + output[idx] as u16 * inv_a) / 255) as u8;
                        output[idx + 1] =
                            ((eff_g * alpha + output[idx + 1] as u16 * inv_a) / 255) as u8;
                        output[idx + 2] =
                            ((eff_b * alpha + output[idx + 2] as u16 * inv_a) / 255) as u8;
                    }
                }
            }
        }
    }
}

/// Horizontal box blur with clamp-to-edge boundaries. O(1) per pixel via sliding window.
fn box_blur_h(src: &[u16], dst: &mut [u16], w: usize, h: usize, radius: usize) {
    let d = (2 * radius + 1) as u32;
    let r = radius as isize;

    for y in 0..h {
        let row = y * w * 3;

        // Clamped read helper
        let get = |x: isize, c: usize| -> u32 {
            let cx = x.max(0).min(w as isize - 1) as usize;
            src[row + cx * 3 + c] as u32
        };

        // Initialize sums for x=0
        let mut sums = [0u32; 3];
        for i in -r..=r {
            for c in 0..3 {
                sums[c] += get(i, c);
            }
        }

        for c in 0..3 {
            dst[row + c] = (sums[c] / d) as u16;
        }

        // Slide window across row
        for x in 1..w {
            let xi = x as isize;
            for c in 0..3 {
                sums[c] += get(xi + r, c);
                sums[c] -= get(xi - r - 1, c);
                dst[row + x * 3 + c] = (sums[c] / d) as u16;
            }
        }
    }
}

/// Vertical box blur with clamp-to-edge boundaries. O(1) per pixel via sliding window.
fn box_blur_v(src: &[u16], dst: &mut [u16], w: usize, h: usize, radius: usize) {
    let d = (2 * radius + 1) as u32;
    let r = radius as isize;
    let stride = w * 3;

    for x in 0..w {
        let col = x * 3;

        let get = |y: isize, c: usize| -> u32 {
            let cy = y.max(0).min(h as isize - 1) as usize;
            src[cy * stride + col + c] as u32
        };

        let mut sums = [0u32; 3];
        for i in -r..=r {
            for c in 0..3 {
                sums[c] += get(i, c);
            }
        }

        for c in 0..3 {
            dst[col + c] = (sums[c] / d) as u16;
        }

        for y in 1..h {
            let yi = y as isize;
            for c in 0..3 {
                sums[c] += get(yi + r, c);
                sums[c] -= get(yi - r - 1, c);
                dst[y * stride + col + c] = (sums[c] / d) as u16;
            }
        }
    }
}

/// Post-processing bloom: downsample → blur → bilinear upscale + additive blend.
fn apply_bloom(
    output: &mut [u8],
    bloom_buf: &mut [u16],
    bloom_tmp: &mut [u16],
    width: usize,
    height: usize,
) {
    let ds_w = width / BLOOM_DS_FACTOR;
    let ds_h = height / BLOOM_DS_FACTOR;

    if ds_w < 2 || ds_h < 2 {
        return;
    }

    // Step 1: Downsample — average each 4×4 pixel block
    let block = BLOOM_DS_FACTOR;
    let count = (block * block) as u32;

    for by in 0..ds_h {
        for bx in 0..ds_w {
            let mut sum_r: u32 = 0;
            let mut sum_g: u32 = 0;
            let mut sum_b: u32 = 0;

            for dy in 0..block {
                let sy = by * block + dy;
                if sy >= height {
                    continue;
                }
                let row_off = sy * width * 3;
                for dx in 0..block {
                    let sx = bx * block + dx;
                    if sx >= width {
                        continue;
                    }
                    let idx = row_off + sx * 3;
                    // Threshold: only accumulate brightness above floor
                    // This preserves webcam contrast in dark areas
                    sum_r += output[idx].saturating_sub(BLOOM_THRESHOLD) as u32;
                    sum_g += output[idx + 1].saturating_sub(BLOOM_THRESHOLD) as u32;
                    sum_b += output[idx + 2].saturating_sub(BLOOM_THRESHOLD) as u32;
                }
            }

            let didx = (by * ds_w + bx) * 3;
            bloom_buf[didx] = (sum_r / count) as u16;
            bloom_buf[didx + 1] = (sum_g / count) as u16;
            bloom_buf[didx + 2] = (sum_b / count) as u16;
        }
    }

    // Step 2: Multi-pass blur (two passes ≈ tent/Gaussian falloff)
    for _ in 0..BLOOM_BLUR_PASSES {
        box_blur_h(bloom_buf, bloom_tmp, ds_w, ds_h, BLOOM_BLUR_RADIUS);
        box_blur_v(bloom_tmp, bloom_buf, ds_w, ds_h, BLOOM_BLUR_RADIUS);
    }

    // Step 3: Bilinear upscale + additive blend
    // Pre-compute strength as fixed-point 8.8
    let strength = (BLOOM_STRENGTH * 256.0) as u32;

    // Pre-compute x mapping table: (source_index, fractional_part_8bit)
    let mut x_map: Vec<(usize, u32)> = Vec::with_capacity(width);
    for x in 0..width {
        let fx = (x as f32 + 0.5) / BLOOM_DS_FACTOR as f32 - 0.5;
        let fx = fx.max(0.0).min((ds_w - 1) as f32);
        let ix = (fx as usize).min(ds_w - 2);
        let frac = ((fx - ix as f32) * 256.0) as u32;
        x_map.push((ix, frac));
    }

    for y in 0..height {
        let fy = (y as f32 + 0.5) / BLOOM_DS_FACTOR as f32 - 0.5;
        let fy = fy.max(0.0).min((ds_h - 1) as f32);
        let iy = (fy as usize).min(ds_h - 2);
        let fy_frac = ((fy - iy as f32) * 256.0) as u32;
        let inv_fy = 256 - fy_frac;

        let row0 = iy * ds_w * 3;
        let row1 = (iy + 1) * ds_w * 3;

        for x in 0..width {
            let (ix, fx_frac) = x_map[x];
            let inv_fx = 256 - fx_frac;

            let idx00 = row0 + ix * 3;
            let idx10 = row0 + (ix + 1) * 3;
            let idx01 = row1 + ix * 3;
            let idx11 = row1 + (ix + 1) * 3;

            let out_idx = (y * width + x) * 3;

            for c in 0..3 {
                let v00 = bloom_buf[idx00 + c] as u32;
                let v10 = bloom_buf[idx10 + c] as u32;
                let v01 = bloom_buf[idx01 + c] as u32;
                let v11 = bloom_buf[idx11 + c] as u32;

                let top = (v00 * inv_fx + v10 * fx_frac) >> 8;
                let bot = (v01 * inv_fx + v11 * fx_frac) >> 8;
                let val = (top * inv_fy + bot * fy_frac) >> 8;

                let bloom_val = (val * strength) >> 8;
                output[out_idx + c] =
                    output[out_idx + c].saturating_add(bloom_val.min(255) as u8);
            }
        }
    }
}

fn rgb_to_grayscale(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = (width * height) as usize;
    let mut gray = Vec::with_capacity(pixel_count);

    for i in 0..pixel_count {
        let r = rgb[i * 3] as f32;
        let g = rgb[i * 3 + 1] as f32;
        let b = rgb[i * 3 + 2] as f32;
        // Rec. 709 luminance
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        gray.push(lum.round() as u8);
    }

    gray
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BrightnessCurve;

    #[test]
    fn test_render_uniform_gray() {
        let charset: Vec<char> = " .:#@".chars().collect();
        let fg = Rgb { r: 0, g: 255, b: 0 };
        let bg = Rgb { r: 0, g: 0, b: 0 };
        let out_w = 320;
        let out_h = 240;

        let mut renderer = AsciiRenderer::new(
            &charset,
            fg,
            bg,
            BrightnessCurve::Linear,
            false,
            out_w,
            out_h,
            40,
            "mono",
        )
        .expect("Failed to create renderer");

        // Create a uniform gray input frame
        let in_w = 640;
        let in_h = 480;
        let frame: Vec<u8> = vec![128; (in_w * in_h * 3) as usize];

        let output = renderer.render(&frame, in_w, in_h);

        // Output should be the right size
        assert_eq!(output.len(), (out_w * out_h * 3) as usize);

        // Should not be all zeros (background was filled + glyphs composited)
        assert!(
            output.iter().any(|&b| b != 0),
            "Output should not be all zeros"
        );
    }
}
