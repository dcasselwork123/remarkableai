//! The trigger gesture: draw a circle around some ink, then HOLD the pen
//! still (touching the glass) for ~a second before lifting.
//!
//! Detection is timing-first: the hold is what makes it near-impossible to
//! fire accidentally — nobody rests a touching pen motionless for a full
//! second mid-writing. The loop-shape check on top is deliberately crude.
//! (This mirrors reMarkable's own "snap shapes" draw-and-hold interaction;
//! leave "Enable shapes" off in documents where scribe is used.)

use crate::screen::BBox;
use crate::shadow::Stroke;

pub struct Config {
    /// Pen must sit still at least this long before lifting.
    pub hold_ms: u64,
    /// "Still" = within this many px of the final point.
    pub jitter_px: i32,
    /// Loop bbox must be at least this wide / tall.
    pub min_w: i32,
    pub min_h: i32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hold_ms: std::env::var("SCRIBE_HOLD_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(900),
            jitter_px: 12,
            min_w: 60,
            min_h: 30,
        }
    }
}

/// If `stroke` is a circle-plus-hold, return the region it encloses.
pub fn detect(stroke: &Stroke, cfg: &Config) -> Option<BBox> {
    let n = stroke.pts.len();
    if n < 10 {
        return None;
    }
    let last = *stroke.pts.last().unwrap();
    let end_ms = *stroke.ms.last().unwrap();

    // Walk back through the motionless tail.
    let mut tail_start = n - 1;
    while tail_start > 0 {
        let p = stroke.pts[tail_start - 1];
        let (dx, dy) = (p.0 - last.0, p.1 - last.1);
        if dx * dx + dy * dy > cfg.jitter_px * cfg.jitter_px {
            break;
        }
        tail_start -= 1;
    }
    let hold = end_ms.saturating_sub(stroke.ms[tail_start]);
    if hold < cfg.hold_ms {
        return None;
    }

    // The loop body is everything before the hold.
    let body = &stroke.pts[..tail_start.max(1)];
    if body.len() < 8 {
        return None;
    }
    let mut bbox = BBox::empty();
    for &(x, y) in body {
        bbox.add(x, y, 0);
    }
    if bbox.w() < cfg.min_w || bbox.h() < cfg.min_h {
        return None;
    }
    let diag = bbox.diag();

    // Closed-ish: the stroke returns near where it started. (The hold point
    // is the end of the body, so compare body start to body end.)
    let (sx, sy) = body[0];
    let (ex, ey) = body[body.len() - 1];
    let closure = (((ex - sx).pow(2) + (ey - sy).pow(2)) as f32).sqrt();
    if closure > (0.35 * diag).max(60.0) {
        return None;
    }

    // Long enough to have gone AROUND something, not just out-and-back.
    let path: f32 = body
        .windows(2)
        .map(|w| {
            let (dx, dy) = ((w[1].0 - w[0].0) as f32, (w[1].1 - w[0].1) as f32);
            (dx * dx + dy * dy).sqrt()
        })
        .sum();
    if path < 1.4 * diag {
        return None;
    }

    Some(bbox)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn circle_stroke(cx: i32, cy: i32, rx: i32, ry: i32, hold_ms: u64) -> Stroke {
        let mut pts = Vec::new();
        let mut ms = Vec::new();
        let steps = 60;
        for i in 0..=steps {
            let a = i as f32 / steps as f32 * std::f32::consts::TAU;
            pts.push((cx + (rx as f32 * a.cos()) as i32, cy + (ry as f32 * a.sin()) as i32));
            ms.push(i as u64 * 8); // ~0.5s of drawing
        }
        // Hold at the end.
        let t0 = *ms.last().unwrap();
        let hold_samples = (hold_ms / 20).max(1);
        for i in 1..=hold_samples {
            pts.push(*pts.last().unwrap());
            ms.push(t0 + i * 20);
        }
        Stroke { pts, ms }
    }

    #[test]
    fn circle_with_hold_triggers() {
        let cfg = Config { hold_ms: 900, jitter_px: 12, min_w: 60, min_h: 30 };
        let s = circle_stroke(400, 300, 180, 80, 1100);
        let bbox = detect(&s, &cfg).expect("should trigger");
        assert!(bbox.x0 <= 230 && bbox.x1 >= 570, "{bbox:?}");
        assert!(bbox.y0 <= 230 && bbox.y1 >= 370, "{bbox:?}");
    }

    #[test]
    fn circle_without_hold_does_not_trigger() {
        let cfg = Config { hold_ms: 900, jitter_px: 12, min_w: 60, min_h: 30 };
        let s = circle_stroke(400, 300, 180, 80, 200);
        assert!(detect(&s, &cfg).is_none());
    }

    #[test]
    fn straight_line_with_hold_does_not_trigger() {
        let cfg = Config { hold_ms: 900, jitter_px: 12, min_w: 60, min_h: 30 };
        let mut pts = Vec::new();
        let mut ms = Vec::new();
        for i in 0..80 {
            pts.push((100 + i * 8, 300));
            ms.push(i as u64 * 8);
        }
        for i in 0..60u64 {
            pts.push(*pts.last().unwrap());
            ms.push(640 + i * 20);
        }
        let s = Stroke { pts, ms };
        assert!(detect(&s, &cfg).is_none(), "open stroke must not trigger");
    }

    #[test]
    fn tiny_loop_does_not_trigger() {
        // Cursive letter 'o'-sized loops with a natural pause must not fire.
        let cfg = Config { hold_ms: 900, jitter_px: 12, min_w: 60, min_h: 30 };
        let s = circle_stroke(400, 300, 15, 12, 1200);
        assert!(detect(&s, &cfg).is_none());
    }

    #[test]
    fn handwriting_squiggle_does_not_trigger() {
        // A zigzag whose ends happen to be near each other but path/diag is
        // in the ambiguous zone, with no hold: never triggers.
        let cfg = Config::default();
        let mut pts = Vec::new();
        let mut ms = Vec::new();
        for i in 0..100 {
            pts.push((300 + (i % 20) * 10, 300 + i * 2));
            ms.push(i as u64 * 6);
        }
        let s = Stroke { pts, ms };
        assert!(detect(&s, &cfg).is_none());
    }
}
