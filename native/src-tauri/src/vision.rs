//! Camera Assist vision (M3). Screen capture + player health-bar detection,
//! ported from the Python `cameraassist.py` numpy pipeline.
//!
//! M3 status: capture is verified at runtime. Detection (below) is a faithful
//! port but its ACCURACY is validation-blocked — it must be confirmed against a
//! real in-game frame (capture-and-compare vs the Python output). Treat the
//! detection as first-pass until validated live.
#![allow(dead_code)] // wired into the recenter loop as M3 progresses

use image::RgbaImage;

/// Capture the primary monitor as an RGBA image. `None` if no monitor / capture
/// failed (e.g. headless).
pub fn capture_primary() -> Option<RgbaImage> {
    Capturer::new().capture()
}

/// Primary-monitor capturer that caches the monitor handle across frames
/// (re-enumerating monitors every tick is needless OS overhead at ~12 Hz).
/// The cache is dropped on a failed capture so display changes self-heal.
pub struct Capturer {
    monitor: Option<xcap::Monitor>,
}

impl Capturer {
    pub fn new() -> Self {
        Self { monitor: None }
    }

    pub fn capture(&mut self) -> Option<RgbaImage> {
        if self.monitor.is_none() {
            self.monitor = Some(xcap::Monitor::all().ok()?.into_iter().next()?);
        }
        match self.monitor.as_ref().unwrap().capture_image().ok() {
            Some(img) => Some(img),
            None => {
                self.monitor = None;
                None
            }
        }
    }
}

/// Expected on-screen player anchor (screen center, nudged down) — where the
/// champion usually sits. Matches the Python `expected_player_anchor`.
const PLAYER_ANCHOR_Y_OFFSET: i32 = 50;

pub fn expected_player_anchor(w: i32, h: i32) -> (i32, i32) {
    (w / 2, (h / 2 + PLAYER_ANCHOR_Y_OFFSET).clamp(0, (h - 1).max(0)))
}

pub fn distance(a: (i32, i32), b: (i32, i32)) -> f64 {
    (((a.0 - b.0).pow(2) + (a.1 - b.1).pow(2)) as f64).sqrt()
}

// Health-bar geometry (defaults from the Python config). Detection is a
// faithful port of cameraassist.py; ACCURACY needs live in-game validation
// (use the `capture_debug_frame` command in a real game and compare).
const BAR_MIN_WIDTH: i32 = 66;
const BAR_MAX_WIDTH: i32 = 190;
const BAR_MIN_HEIGHT: i32 = 2;
const BAR_MAX_HEIGHT: i32 = 18;
const HEALTHBAR_TO_PLAYER_OFFSET_Y: i32 = 82;
pub const TARGET_TRACK_SEC: f64 = 1.6;

#[derive(Clone, Copy, Debug)]
pub struct PlayerCandidate {
    pub bar_box: (i32, i32, i32, i32), // left, top, right, bottom
    pub player_anchor: (i32, i32),
    pub width: i32,
    pub height: i32,
    pub confidence: f64,
    pub mana_bonus: f64,
}

fn clamp_i(v: i32, lo: i32, hi: i32) -> i32 {
    v.max(lo).min(hi)
}

fn gameplay_region_ok(x: i32, y: i32, w: i32, h: i32) -> bool {
    let (wf, hf, xf, yf) = (w as f64, h as f64, x as f64, y as f64);
    if yf < hf * 0.055 || yf > hf * 0.84 {
        return false;
    }
    if xf > wf * 0.82 && yf > hf * 0.60 {
        return false;
    }
    if xf < wf * 0.18 && yf > hf * 0.72 {
        return false;
    }
    true
}

fn median(values: &mut [i32]) -> i32 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        values[mid]
    } else {
        ((values[mid - 1] + values[mid]) as f64 / 2.0) as i32
    }
}

fn blue_bonus(blue: &[bool], w: i32, h: i32, left: i32, right: i32, bottom: i32) -> f64 {
    let y0 = clamp_i(bottom + 2, 0, h - 1);
    let y1 = clamp_i(bottom + 20, 0, h - 1);
    if y1 <= y0 {
        return 0.0;
    }
    let x0 = clamp_i(left - 6, 0, w - 1);
    let x1 = clamp_i(right + 6, 0, w - 1);
    if x1 <= x0 {
        return 0.0;
    }
    let mut best = 0.0_f64;
    for y in y0..=y1 {
        let mut count = 0;
        for x in x0..=x1 {
            if blue[(y * w + x) as usize] {
                count += 1;
            }
        }
        let coverage = count as f64 / ((x1 - x0 + 1).max(1) as f64);
        if coverage > best {
            best = coverage;
        }
    }
    (best * 2.2).min(1.0)
}

struct Group {
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
    last_y: i32,
    last_start: i32,
    last_end: i32,
    widths: Vec<i32>,
    rows: i32,
}

thread_local! {
    /// Reused green/blue mask buffers — avoids ~4 MB of fresh allocation per
    /// frame at 1080p in the Camera Assist hot loop.
    static MASKS: std::cell::RefCell<(Vec<bool>, Vec<bool>)> = std::cell::RefCell::new((Vec::new(), Vec::new()));
}

/// Detect candidate player health-bars (faithful port of
/// `cameraassist.py::detect_player_candidates`).
pub fn detect_player_candidates(frame: &RgbaImage) -> Vec<PlayerCandidate> {
    let w = frame.width() as i32;
    let h = frame.height() as i32;
    let buf = frame.as_raw(); // RGBA8

    MASKS.with_borrow_mut(|(green, blue)| {
        green.resize((w * h) as usize, false);
        blue.resize((w * h) as usize, false);
        for i in 0..(w * h) as usize {
            let r = buf[i * 4] as i32;
            let g = buf[i * 4 + 1] as i32;
            let b = buf[i * 4 + 2] as i32;
            green[i] = g >= 118
                && b <= 150
                && g >= r - 22
                && (g - b) >= 34
                && (r <= 190 || (g - r) >= 18);
            blue[i] = b >= 105 && g >= 55 && r <= 130 && (b - r) >= 32;
        }

        let mut active: Vec<Group> = Vec::new();
        let mut finished: Vec<Group> = Vec::new();

        for y in 0..h {
            // Run-length green segments in this row, filtered by width + region.
            let mut segments: Vec<(i32, i32)> = Vec::new();
            let mut x = 0;
            while x < w {
                if green[(y * w + x) as usize] {
                    let start = x;
                    while x < w && green[(y * w + x) as usize] {
                        x += 1;
                    }
                    let end = x - 1;
                    let seg_w = end - start + 1;
                    if seg_w >= BAR_MIN_WIDTH && seg_w <= BAR_MAX_WIDTH {
                        let cx = (start + end) / 2;
                        if gameplay_region_ok(cx, y, w, h) {
                            segments.push((start, end));
                        }
                    }
                } else {
                    x += 1;
                }
            }

            let mut matched = vec![false; active.len()];
            for seg in &segments {
                let mut best: i32 = -1;
                let mut best_overlap = 0.0_f64;
                for (gi, g) in active.iter().enumerate() {
                    if y - g.last_y > 1 {
                        continue;
                    }
                    let overlap = (seg.1.min(g.last_end) - seg.0.max(g.last_start) + 1).max(0);
                    let min_w = (seg.1 - seg.0 + 1).min(g.last_end - g.last_start + 1);
                    let ratio = overlap as f64 / (min_w.max(1) as f64);
                    if ratio > 0.45 && ratio > best_overlap {
                        best = gi as i32;
                        best_overlap = ratio;
                    }
                }
                if best >= 0 {
                    let g = &mut active[best as usize];
                    g.min_x = g.min_x.min(seg.0);
                    g.max_x = g.max_x.max(seg.1);
                    g.min_y = g.min_y.min(y);
                    g.max_y = g.max_y.max(y);
                    g.last_y = y;
                    g.last_start = seg.0;
                    g.last_end = seg.1;
                    g.widths.push(seg.1 - seg.0 + 1);
                    g.rows += 1;
                    matched[best as usize] = true;
                } else {
                    active.push(Group {
                        min_x: seg.0,
                        max_x: seg.1,
                        min_y: y,
                        max_y: y,
                        last_y: y,
                        last_start: seg.0,
                        last_end: seg.1,
                        widths: vec![seg.1 - seg.0 + 1],
                        rows: 1,
                    });
                    matched.push(true);
                }
            }

            // Retire groups not seen for >1 row and not touched this row.
            let mut still: Vec<Group> = Vec::with_capacity(active.len());
            for (gi, g) in active.into_iter().enumerate() {
                if g.last_y < y - 1 && !matched.get(gi).copied().unwrap_or(false) {
                    finished.push(g);
                } else {
                    still.push(g);
                }
            }
            active = still;
        }
        finished.extend(active);

        let mut candidates = Vec::new();
        for mut g in finished {
            let group_height = g.max_y - g.min_y + 1;
            let group_width = median(&mut g.widths);
            if g.rows < 2 || group_height < BAR_MIN_HEIGHT || group_height > BAR_MAX_HEIGHT {
                continue;
            }
            let (left, top, right, bottom) = (g.min_x, g.min_y, g.max_x, g.max_y);
            let cx = (left + right) / 2;
            let cy = (top + bottom) / 2;
            if !gameplay_region_ok(cx, cy, w, h) {
                continue;
            }
            let mana = blue_bonus(blue, w, h, left, right, bottom);
            let width_score = 1.0 - (group_width - 112).abs() as f64 / (BAR_MAX_WIDTH.max(1) as f64);
            let height_score = (g.rows as f64 / BAR_MAX_HEIGHT.max(1) as f64).min(1.0);
            let confidence = width_score.max(0.0) + height_score + mana;
            candidates.push(PlayerCandidate {
                bar_box: (left, top, right, bottom),
                player_anchor: (
                    clamp_i(cx, 0, w - 1),
                    clamp_i(bottom + HEALTHBAR_TO_PLAYER_OFFSET_Y, 0, h - 1),
                ),
                width: group_width,
                height: group_height,
                confidence,
                mana_bonus: mana,
            });
        }
        candidates
    })
}

/// Pick the best candidate, biased toward the tracked/expected anchor (faithful
/// port of `choose_player_candidate`).
pub fn choose_player(
    candidates: &[PlayerCandidate],
    w: i32,
    h: i32,
    tracking_anchor: (i32, i32),
) -> Option<PlayerCandidate> {
    if candidates.is_empty() {
        return None;
    }
    let expected = expected_player_anchor(w, h);
    let falloff = (w.max(h) as f64) * 0.42;
    let mut best: Option<PlayerCandidate> = None;
    let mut best_score = f64::MIN;
    for c in candidates {
        let track_d = distance(c.player_anchor, tracking_anchor);
        let exp_d = distance(c.player_anchor, expected);
        let track_score = (1.0 - track_d / falloff.max(1.0)).max(0.0);
        let exp_score = (1.0 - exp_d / falloff.max(1.0)).max(0.0);
        let score = c.confidence + track_score * 2.2 + exp_score + c.mana_bonus;
        if score > best_score {
            best_score = score;
            best = Some(*c);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_returns_a_real_frame() {
        // Spike: prove screen capture works on this machine.
        let img = capture_primary().expect("primary monitor capture failed");
        assert!(img.width() > 0 && img.height() > 0, "captured frame has zero dimensions");
    }

    #[test]
    fn detects_a_synthetic_healthbar() {
        // A ~100px green bar near screen center should be detected with the
        // anchor below it. Validates the detection port on a controlled input
        // (live in-game accuracy still needs the capture_debug_frame check).
        let (w, h) = (800u32, 600u32);
        let mut img = RgbaImage::from_pixel(w, h, image::Rgba([10, 10, 10, 255]));
        let bar_y = (h / 2) as i32 + 10;
        let bar_x0 = (w / 2) as i32 - 50;
        for y in bar_y..bar_y + 6 {
            for x in bar_x0..bar_x0 + 100 {
                img.put_pixel(x as u32, y as u32, image::Rgba([40, 200, 40, 255]));
            }
        }
        let cands = detect_player_candidates(&img);
        assert!(!cands.is_empty(), "expected to detect the synthetic bar");
        let c = cands[0];
        assert!((c.player_anchor.0 - (w / 2) as i32).abs() < 30, "anchor x off-center: {:?}", c.player_anchor);
        assert!(c.player_anchor.1 > bar_y, "anchor should be below the bar");
    }
}
