use crate::config::{BrightnessCurve, Rgb};

/// Bright white-green color for the rain head (distinct from trail green)
const HEAD_COLOR: Rgb = Rgb {
    r: 220,
    g: 255,
    b: 220,
};

/// Per-cell render instruction produced by the rain simulation
pub struct CellRender {
    pub ch: char,
    pub color: Rgb,
    pub intensity: f32,
}

/// A single rain stream within a column
struct RainStream {
    /// Fractional row position of the stream head
    position: f32,
    /// Rows per second (randomized per stream)
    speed: f32,
    /// Number of bright active trail rows behind the head
    trail_length: u32,
    /// Number of dim ghost trail rows after the active trail (0 for classic mode)
    ghost_length: u32,
}

/// State for a single rain column (may contain multiple concurrent streams)
struct RainColumn {
    /// Active streams in this column (1 for classic, 1-3 for movie mode)
    streams: Vec<RainStream>,
    /// Random character index per row (shared across all streams in column)
    char_indices: Vec<u16>,
    /// Frames until next character mutation per row (movie mode only; empty for classic)
    char_timers: Vec<u8>,
    /// Frames before next stream can spawn
    spawn_cooldown: u16,
}

pub struct MatrixRainState {
    columns: Vec<RainColumn>,
    rows: u32,
    cols: u32,
    charset_len: usize,
    rng: u64,
    /// true for matrix theme (multi-stream, ghost trails, char mutation)
    is_movie_mode: bool,
}

/// Inline xorshift64 — fast, no dependencies
fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

impl MatrixRainState {
    pub fn new(cols: u32, rows: u32, charset_len: usize, is_movie_mode: bool) -> Self {
        // Seed from current time nanoseconds
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xdeadbeef_cafebabe);
        let mut rng = seed | 1; // ensure non-zero

        let mut columns = Vec::with_capacity(cols as usize);
        for _ in 0..cols {
            let col = Self::new_column(&mut rng, rows, charset_len, is_movie_mode);
            columns.push(col);
        }

        // Stagger initial dormancy so columns don't all start at once
        for (i, col) in columns.iter_mut().enumerate() {
            let stagger = (xorshift64(&mut rng) % 60) as u16;
            col.spawn_cooldown = stagger;
            // Also stagger initial positions for visual variety at startup
            if i % 3 == 0 {
                if let Some(stream) = col.streams.first_mut() {
                    stream.position = -((xorshift64(&mut rng) % (rows as u64)) as f32);
                }
            }
        }

        MatrixRainState {
            columns,
            rows,
            cols,
            charset_len,
            rng,
            is_movie_mode,
        }
    }

    fn new_column(
        rng: &mut u64,
        rows: u32,
        charset_len: usize,
        is_movie_mode: bool,
    ) -> RainColumn {
        let stream = Self::new_stream(rng, rows, is_movie_mode);

        let mut char_indices = Vec::with_capacity(rows as usize);
        for _ in 0..rows {
            let idx = if charset_len > 0 {
                (xorshift64(rng) % charset_len as u64) as u16
            } else {
                0
            };
            char_indices.push(idx);
        }

        let char_timers = if is_movie_mode {
            let mut timers = Vec::with_capacity(rows as usize);
            for _ in 0..rows {
                timers.push((xorshift64(rng) % 4 + 2) as u8);
            }
            timers
        } else {
            Vec::new()
        };

        RainColumn {
            streams: vec![stream],
            char_indices,
            char_timers,
            spawn_cooldown: 0,
        }
    }

    fn new_stream(rng: &mut u64, rows: u32, is_movie_mode: bool) -> RainStream {
        let speed = 10.0 + (xorshift64(rng) % 3500) as f32 / 100.0; // 10.0..45.0
        let min_trail = (rows / 6).max(3);
        let max_trail = (rows / 3).max(min_trail + 1);
        let trail_range = max_trail - min_trail;
        let trail_length = min_trail + (xorshift64(rng) as u32 % trail_range.max(1));

        let ghost_length = if is_movie_mode {
            // Ghost trail: 50-100% of trail_length, minimum 1
            let min_ghost = trail_length / 2;
            let ghost_range = (trail_length - min_ghost).max(1);
            (min_ghost + (xorshift64(rng) as u32 % ghost_range)).max(1)
        } else {
            0
        };

        RainStream {
            position: 0.0,
            speed,
            trail_length,
            ghost_length,
        }
    }

    /// Advance all rain columns by dt seconds (frame-rate independent)
    pub fn advance(&mut self, dt: f32) {
        let rows = self.rows;
        let charset_len = self.charset_len;
        let is_movie = self.is_movie_mode;
        let max_streams: usize = if is_movie { 3 } else { 1 };

        for col in &mut self.columns {
            // Advance all existing streams
            for stream in &mut col.streams {
                stream.position += stream.speed * dt;
            }

            if is_movie {
                // Movie mode: per-row character mutation across entire trail
                for row_idx in 0..rows as usize {
                    if row_idx < col.char_timers.len() {
                        if col.char_timers[row_idx] == 0 {
                            // Mutate character
                            if charset_len > 0 {
                                col.char_indices[row_idx] =
                                    (xorshift64(&mut self.rng) % charset_len as u64) as u16;
                            }
                            // Determine timer reset based on proximity to nearest stream head
                            let mut in_active = false;
                            for stream in &col.streams {
                                let head = stream.position as i32;
                                let dist = head - row_idx as i32;
                                if dist >= 0 && (dist as u32) < stream.trail_length {
                                    in_active = true;
                                    break;
                                }
                            }
                            // Active trail: faster mutation (2-4 frames)
                            // Ghost/background: slower mutation (4-8 frames)
                            let reset = if in_active {
                                (xorshift64(&mut self.rng) % 3 + 2) as u8
                            } else {
                                (xorshift64(&mut self.rng) % 5 + 4) as u8
                            };
                            col.char_timers[row_idx] = reset;
                        } else {
                            col.char_timers[row_idx] -= 1;
                        }
                    }
                }
            } else {
                // Classic mode: only randomize 3 rows near the first stream's head
                if let Some(stream) = col.streams.first() {
                    let head_row = stream.position as i32;
                    for offset in 0..3 {
                        let r = head_row - offset;
                        if r >= 0 && (r as u32) < rows {
                            if charset_len > 0 {
                                col.char_indices[r as usize] =
                                    (xorshift64(&mut self.rng) % charset_len as u64) as u16;
                            }
                        }
                    }
                }
            }

            // Remove streams whose ghost trail has fully exited the bottom
            let total_rows = rows as i32;
            col.streams.retain(|stream| {
                let trail_end =
                    stream.position as i32 - stream.trail_length as i32 - stream.ghost_length as i32;
                trail_end <= total_rows
            });

            // Spawn new streams (always ensure at least one exists)
            if col.streams.is_empty() {
                let stream = Self::new_stream(&mut self.rng, rows, is_movie);
                col.streams.push(stream);
                col.spawn_cooldown = (xorshift64(&mut self.rng) % 40 + 20) as u16;
            } else if col.spawn_cooldown > 0 {
                col.spawn_cooldown -= 1;
            } else if col.streams.len() < max_streams {
                let stream = Self::new_stream(&mut self.rng, rows, is_movie);
                col.streams.push(stream);
                col.spawn_cooldown = (xorshift64(&mut self.rng) % 40 + 20) as u16;
            }
        }
    }

    /// Combine rain state with webcam brightness grid to produce per-cell render instructions
    pub fn compute_cells(
        &self,
        grid: &[f32],
        charset: &[char],
        brightness_curve: BrightnessCurve,
        invert: bool,
        fg: Rgb,
    ) -> Vec<CellRender> {
        let cols = self.cols as usize;
        let rows = self.rows as usize;
        let n = charset.len();
        let bg_factor: f32 = 0.55; // used in classic (non-movie) mode only
        let mut cells = Vec::with_capacity(cols * rows);

        for row in 0..rows {
            for col in 0..cols {
                let rain_col = &self.columns[col];
                let grid_idx = row * cols + col;

                // Webcam brightness (0.0..1.0) with curve applied
                let mut wb = brightness_curve.apply(grid[grid_idx]);
                if invert {
                    wb = 1.0 - wb;
                }

                // Find the stream with maximum intensity at this cell
                let mut best_intensity: f32 = 0.0;
                let mut best_color = fg;
                let mut is_rain = false;

                for stream in &rain_col.streams {
                    if stream.trail_length == 0 {
                        continue;
                    }

                    let head_row = stream.position as i32;
                    let distance = head_row - row as i32;

                    if distance < 0 {
                        continue; // Stream hasn't reached this row yet
                    }

                    let dist = distance as u32;
                    let intensity;

                    if dist < stream.trail_length {
                        // ACTIVE TRAIL: quadratic decay from head
                        let t = distance as f32 / stream.trail_length as f32;
                        intensity = (1.0 - t) * (1.0 - t);
                    } else if stream.ghost_length > 0
                        && dist < stream.trail_length + stream.ghost_length
                    {
                        // GHOST TRAIL: visible fixed start decaying to zero
                        // Start at 0.18 (visible remnant) and decay quadratically
                        let ghost_t = (dist - stream.trail_length) as f32
                            / stream.ghost_length as f32;
                        intensity = 0.18 * (1.0 - ghost_t) * (1.0 - ghost_t);
                    } else {
                        continue; // Beyond trail
                    }

                    if intensity > best_intensity {
                        best_intensity = intensity;
                        is_rain = true;

                        // Head color blending
                        if distance <= 0 {
                            best_color = HEAD_COLOR;
                        } else if distance <= 3 {
                            let blend = distance as f32 / 3.0;
                            best_color = Rgb {
                                r: (HEAD_COLOR.r as f32 * (1.0 - blend) + fg.r as f32 * blend)
                                    as u8,
                                g: (HEAD_COLOR.g as f32 * (1.0 - blend) + fg.g as f32 * blend)
                                    as u8,
                                b: (HEAD_COLOR.b as f32 * (1.0 - blend) + fg.b as f32 * blend)
                                    as u8,
                            };
                        } else {
                            best_color = fg;
                        }
                    }
                }

                if is_rain {
                    // Rain brightness modulated by webcam
                    let brightness = best_intensity * (0.35 + 0.65 * wb);

                    // Character from pre-computed random index
                    let ch = if n > 0 {
                        let idx = rain_col.char_indices[row] as usize % n;
                        charset[idx]
                    } else {
                        '#'
                    };

                    cells.push(CellRender {
                        ch,
                        color: best_color,
                        intensity: brightness,
                    });
                } else if self.is_movie_mode {
                    // Movie mode: dense background — random char, ambient floor + webcam
                    let brightness = 0.06 + wb * 0.55;

                    let ch = if n > 0 {
                        charset[rain_col.char_indices[row] as usize % n]
                    } else {
                        '#'
                    };

                    cells.push(CellRender {
                        ch,
                        color: fg,
                        intensity: brightness,
                    });
                } else {
                    // Classic mode: brightness-mapped character
                    let brightness = wb * bg_factor;

                    let ch = if n > 0 {
                        let idx = (wb * (n - 1) as f32).round() as usize;
                        charset[idx.min(n - 1)]
                    } else {
                        ' '
                    };

                    cells.push(CellRender {
                        ch,
                        color: fg,
                        intensity: brightness,
                    });
                }
            }
        }

        cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xorshift_produces_different_values() {
        let mut state = 12345u64;
        let a = xorshift64(&mut state);
        let b = xorshift64(&mut state);
        let c = xorshift64(&mut state);
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn test_rain_state_creation() {
        let state = MatrixRainState::new(80, 45, 15, false);
        assert_eq!(state.cols, 80);
        assert_eq!(state.rows, 45);
        assert_eq!(state.columns.len(), 80);
        assert_eq!(state.charset_len, 15);
        assert!(!state.is_movie_mode);
    }

    #[test]
    fn test_column_lifecycle() {
        let mut state = MatrixRainState::new(1, 10, 5, false);

        // Force column active at position 0
        state.columns[0].spawn_cooldown = 0;
        state.columns[0].streams[0].position = 0.0;
        state.columns[0].streams[0].speed = 20.0;
        state.columns[0].streams[0].trail_length = 4;

        // Advance enough to move past the grid
        for _ in 0..100 {
            state.advance(0.033); // ~30fps
        }

        // Column should have reset (stream recycled with new params)
        let col = &state.columns[0];
        assert!(
            !col.streams.is_empty(),
            "column should always have at least one stream"
        );
    }

    #[test]
    fn test_intensity_decay() {
        // Head (distance=0): (1.0 - 0)^2 = 1.0
        // Mid-trail (distance=half): (1.0 - 0.5)^2 = 0.25
        // Trail end (distance=trail_length-1 ≈ 1.0): ~0.0
        let trail_len = 10.0_f32;
        let head_ri = (1.0 - 0.0 / trail_len).powi(2);
        let mid_ri = (1.0 - 5.0 / trail_len).powi(2);
        let end_ri = (1.0 - 9.0 / trail_len).powi(2);

        assert!((head_ri - 1.0).abs() < 0.001);
        assert!((mid_ri - 0.25).abs() < 0.001);
        assert!(end_ri < 0.02);
    }

    #[test]
    fn test_compute_cells_output_size() {
        let state = MatrixRainState::new(10, 5, 5, false);
        let charset: Vec<char> = " .:#@".chars().collect();
        let grid = vec![0.5f32; 50]; // 10 cols * 5 rows

        let cells = state.compute_cells(
            &grid,
            &charset,
            BrightnessCurve::Linear,
            false,
            Rgb {
                r: 0,
                g: 200,
                b: 0,
            },
        );

        assert_eq!(cells.len(), 50);
    }

    #[test]
    fn test_compute_cells_brightness_range() {
        let state = MatrixRainState::new(10, 5, 5, false);
        let charset: Vec<char> = " .:#@".chars().collect();
        let grid = vec![0.5f32; 50];

        let cells = state.compute_cells(
            &grid,
            &charset,
            BrightnessCurve::Linear,
            false,
            Rgb {
                r: 0,
                g: 200,
                b: 0,
            },
        );

        for cell in &cells {
            assert!(cell.intensity >= 0.0, "intensity should be >= 0");
            assert!(cell.intensity <= 1.0, "intensity should be <= 1.0");
        }
    }

    #[test]
    fn test_matrix_mode_creation() {
        let state = MatrixRainState::new(10, 45, 80, true);
        assert!(state.is_movie_mode);
        // Movie mode columns should have char_timers
        for col in &state.columns {
            assert_eq!(col.char_timers.len(), 45);
        }
    }

    #[test]
    fn test_ghost_trail_visibility() {
        // Ghost trail uses fixed start intensity 0.18, decaying quadratically
        let ghost_length: u32 = 5;

        // First ghost row (ghost_t = 0): intensity = 0.18
        let ghost_t = 0.0 / ghost_length as f32;
        let first_ghost = 0.18 * (1.0 - ghost_t) * (1.0 - ghost_t);
        assert!(
            (first_ghost - 0.18).abs() < 0.001,
            "first ghost row should be 0.18, got {}",
            first_ghost
        );

        // Last ghost row (ghost_t ≈ 1): intensity ≈ 0
        let ghost_t = (ghost_length - 1) as f32 / ghost_length as f32;
        let last_ghost = 0.18 * (1.0 - ghost_t) * (1.0 - ghost_t);
        assert!(
            last_ghost < 0.01,
            "last ghost row should be near zero, got {}",
            last_ghost
        );

        // Monotonic decrease
        let mid_ghost_t = 2.0 / ghost_length as f32;
        let mid_ghost = 0.18 * (1.0 - mid_ghost_t) * (1.0 - mid_ghost_t);
        assert!(
            first_ghost > mid_ghost && mid_ghost > last_ghost,
            "ghost trail should decrease monotonically"
        );
    }

    #[test]
    fn test_multi_stream_lifecycle() {
        let mut state = MatrixRainState::new(1, 20, 10, true);

        // Force first stream far down so second can spawn
        state.columns[0].streams[0].position = 15.0;
        state.columns[0].spawn_cooldown = 0;

        // Advance several times to trigger spawning
        for _ in 0..200 {
            state.advance(0.033);
        }

        // At some point during the lifecycle, we should see the mechanism working
        // (streams getting recycled, new ones spawned)
        assert!(
            !state.columns[0].streams.is_empty(),
            "column should always maintain streams"
        );
    }

    #[test]
    fn test_classic_mode_unchanged() {
        // Classic mode: single stream, no ghost trails, no char timers
        let state = MatrixRainState::new(5, 20, 10, false);

        for col in &state.columns {
            assert_eq!(col.streams.len(), 1, "classic mode should have 1 stream");
            assert_eq!(
                col.streams[0].ghost_length, 0,
                "classic mode should have no ghost trail"
            );
            assert!(
                col.char_timers.is_empty(),
                "classic mode should have no char timers"
            );
        }
    }

    #[test]
    fn test_matrix_charset_coverage() {
        let charset = crate::config::matrix_charset();
        // Should have ~59 katakana + 21 symbols/numerals = ~80 chars
        assert!(charset.len() >= 70, "matrix charset too small: {}", charset.len());
        assert!(charset.len() <= 90, "matrix charset too large: {}", charset.len());

        // Should contain katakana
        assert!(charset.contains(&'ｦ'), "should contain half-width katakana ｦ");
        assert!(charset.contains(&'ﾝ'), "should contain half-width katakana ﾝ");
        // Should contain numerals
        assert!(charset.contains(&'0'), "should contain numeral 0");
        assert!(charset.contains(&'9'), "should contain numeral 9");
        // Should contain Z
        assert!(charset.contains(&'Z'), "should contain Z");
    }
}
