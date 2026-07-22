//! The shadow page: scribe's own record of the ink currently on the page.
//! xochitl draws the real ink; scribe mirrors it from the shared pen stream
//! so it can (a) rasterize a circled region for the oracle and (b) erase
//! exactly those strokes later by retracing them.
//!
//! The shadow is a heuristic, not ground truth — it only knows strokes made
//! while the daemon was running, and it approximates xochitl's eraser and
//! undo behavior. Good enough for "circle what you just wrote".

use crate::screen::{BBox, SCREEN_H, SCREEN_W};

#[derive(Clone, Debug)]
pub struct Stroke {
    /// Screen-space points, in draw order (consecutive duplicates kept —
    /// they carry hold timing).
    pub pts: Vec<(i32, i32)>,
    /// Per-point capture time, ms since daemon start.
    pub ms: Vec<u64>,
}


pub struct Shadow {
    strokes: Vec<(u64, Stroke)>,
    next_id: u64,
    current: Stroke,
    /// Bumped on page turns (clear); pending ops from an older epoch are void.
    pub epoch: u64,
}

impl Shadow {
    pub fn new() -> Self {
        Self {
            strokes: Vec::new(),
            next_id: 1,
            current: Stroke { pts: Vec::new(), ms: Vec::new() },
            epoch: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.strokes.len()
    }

    pub fn pen_point(&mut self, x: i32, y: i32, now_ms: u64) {
        if self.current.pts.len() < 40_000 {
            self.current.pts.push((x, y));
            self.current.ms.push(now_ms);
        }
    }

    /// Pen lifted: commit the in-flight stroke. Returns its id if anything
    /// was inked. Single-sample tap dots (periods, i-dots) are kept — they
    /// are real ink that must be erasable later.
    pub fn pen_up(&mut self) -> Option<u64> {
        if self.current.pts.is_empty() {
            return None;
        }
        if self.current.pts.len() == 1 {
            let (p, t) = (self.current.pts[0], self.current.ms[0]);
            self.current.pts.push(p);
            self.current.ms.push(t);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.strokes.push((
            id,
            std::mem::replace(&mut self.current, Stroke { pts: Vec::new(), ms: Vec::new() }),
        ));
        Some(id)
    }

    pub fn get(&self, id: u64) -> Option<&Stroke> {
        self.strokes.iter().find(|(i, _)| *i == id).map(|(_, s)| s)
    }

    /// Remove and return a stroke (used for the trigger circle itself).
    pub fn take(&mut self, id: u64) -> Option<Stroke> {
        let pos = self.strokes.iter().position(|(i, _)| *i == id)?;
        Some(self.strokes.remove(pos).1)
    }

    /// Mirror xochitl's two-finger-tap undo: drop the newest stroke.
    pub fn undo_pop(&mut self) -> Option<u64> {
        self.strokes.pop().map(|(i, _)| i)
    }

    /// Page turned (or daemon lost track): forget everything.
    pub fn clear(&mut self) {
        self.strokes.clear();
        self.current.pts.clear();
        self.current.ms.clear();
        self.epoch += 1;
    }

    /// Mirror the physical eraser: drop stored points within `r` of (x, y),
    /// splitting strokes erased through the middle.
    pub fn erase_point(&mut self, x: i32, y: i32, r: i32) {
        let r2 = (r + 2) * (r + 2);
        let mut kept: Vec<(u64, Stroke)> = Vec::new();
        for (id, stroke) in self.strokes.drain(..) {
            let mut seg_pts: Vec<(i32, i32)> = Vec::new();
            let mut seg_ms: Vec<u64> = Vec::new();
            let mut first_free_id = id;
            let flush =
                |pts: &mut Vec<(i32, i32)>, ms: &mut Vec<u64>, kept: &mut Vec<(u64, Stroke)>, next: &mut u64, fid: &mut u64| {
                    if pts.len() >= 2 {
                        let use_id = if *fid != 0 {
                            std::mem::replace(fid, 0)
                        } else {
                            let n = *next;
                            *next += 1;
                            n
                        };
                        kept.push((
                            use_id,
                            Stroke { pts: std::mem::take(pts), ms: std::mem::take(ms) },
                        ));
                    } else {
                        pts.clear();
                        ms.clear();
                    }
                };
            for (p, t) in stroke.pts.iter().zip(stroke.ms.iter()) {
                let (dx, dy) = (p.0 - x, p.1 - y);
                if dx * dx + dy * dy <= r2 {
                    flush(&mut seg_pts, &mut seg_ms, &mut kept, &mut self.next_id, &mut first_free_id);
                } else {
                    seg_pts.push(*p);
                    seg_ms.push(*t);
                }
            }
            flush(&mut seg_pts, &mut seg_ms, &mut kept, &mut self.next_id, &mut first_free_id);
        }
        self.strokes = kept;
    }

    /// Insert an injected (AI-written) stroke so a later circle can target it.
    pub fn add_synthetic(&mut self, pts: Vec<(i32, i32)>, now_ms: u64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let ms = vec![now_ms; pts.len()];
        self.strokes.push((id, Stroke { pts, ms }));
        id
    }

    /// Ids of strokes lying (mostly) inside `region`: at least 60% of a
    /// stroke's points must fall in the box.
    pub fn ids_in(&self, region: &BBox) -> Vec<u64> {
        let mut out = Vec::new();
        for (id, s) in &self.strokes {
            if s.pts.is_empty() {
                continue;
            }
            let inside = s.pts.iter().filter(|&&(x, y)| region.contains(x, y)).count();
            if inside * 10 >= s.pts.len() * 6 {
                out.push(*id);
            }
        }
        out
    }

    pub fn bbox_of(&self, ids: &[u64], margin: i32) -> BBox {
        let mut b = BBox::empty();
        for (id, s) in &self.strokes {
            if ids.contains(id) {
                for &(x, y) in &s.pts {
                    b.add(x, y, margin);
                }
            }
        }
        b
    }

    pub fn bbox_all(&self, margin: i32) -> BBox {
        let mut b = BBox::empty();
        for (_, s) in &self.strokes {
            for &(x, y) in &s.pts {
                b.add(x, y, margin);
            }
        }
        b
    }

    /// Every stroke id currently on the shadow page.
    pub fn all_ids(&self) -> Vec<u64> {
        self.strokes.iter().map(|(i, _)| *i).collect()
    }

    /// Rasterize the given strokes (plus `extras`, e.g. the trigger circle
    /// and the command-zone rule so the oracle can see the page structure),
    /// cropped to `region` (padded), as a grayscale PNG. Box-downscales so
    /// the long side stays ≤ 1000 px (vision tokens dominate cost/latency
    /// past that).
    pub fn rasterize_png(
        &self,
        region: &BBox,
        ids: &[u64],
        extras: &[&Stroke],
    ) -> std::io::Result<Vec<u8>> {
        if region.is_empty() {
            return Err(std::io::Error::other("empty region"));
        }
        let pad = 16;
        let x0 = (region.x0 - pad).max(0);
        let y0 = (region.y0 - pad).max(0);
        let x1 = (region.x1 + pad).min(SCREEN_W as i32 - 1);
        let y1 = (region.y1 + pad).min(SCREEN_H as i32 - 1);
        let (w, h) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);

        let mut buf = vec![255u8; w * h];
        let mut stamp = |cx: i32, cy: i32| {
            // 5px-diameter dot: close to real ballpoint ink width.
            for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    if dx * dx + dy * dy > 5 {
                        continue;
                    }
                    let (px, py) = (cx - x0 + dx, cy - y0 + dy);
                    if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                        buf[py as usize * w + px as usize] = 0;
                    }
                }
            }
        };
        let included = self
            .strokes
            .iter()
            .filter(|(id, _)| ids.contains(id))
            .map(|(_, s)| s)
            .chain(extras.iter().copied());
        for s in included {
            for seg in s.pts.windows(2) {
                for (x, y) in interpolate(seg[0], seg[1], 2) {
                    stamp(x, y);
                }
            }
            if s.pts.len() == 1 {
                stamp(s.pts[0].0, s.pts[0].1);
            }
        }

        // Downscale (box filter) to keep the long side ≤ 1000.
        let f = (w.max(h)).div_ceil(1000).max(1);
        let (ow, oh) = ((w / f).max(1), (h / f).max(1));
        let mut gray = vec![0u8; ow * oh];
        for oy in 0..oh {
            for ox in 0..ow {
                let mut acc = 0u32;
                for sy in 0..f {
                    for sx in 0..f {
                        let (ix, iy) = ((ox * f + sx).min(w - 1), (oy * f + sy).min(h - 1));
                        acc += buf[iy * w + ix] as u32;
                    }
                }
                gray[oy * ow + ox] = (acc / (f * f) as u32) as u8;
            }
        }

        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, ow as u32, oh as u32);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_compression(png::Compression::Fast);
            let mut writer = enc.write_header().map_err(std::io::Error::other)?;
            writer.write_image_data(&gray).map_err(std::io::Error::other)?;
        }
        Ok(png_bytes)
    }
}

/// Points every ~`step` px along the segment a→b (inclusive of both ends).
pub fn interpolate(a: (i32, i32), b: (i32, i32), step: i32) -> Vec<(i32, i32)> {
    let (dx, dy) = ((b.0 - a.0) as f32, (b.1 - a.1) as f32);
    let dist = (dx * dx + dy * dy).sqrt();
    let n = (dist / step.max(1) as f32).ceil() as usize;
    if n == 0 {
        return vec![a];
    }
    (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            (a.0 + (dx * t).round() as i32, a.1 + (dy * t).round() as i32)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_stroke(shadow: &mut Shadow, y: i32, x0: i32, x1: i32) -> u64 {
        let mut t = 0;
        let mut x = x0;
        while x <= x1 {
            shadow.pen_point(x, y, t);
            t += 5;
            x += 10;
        }
        shadow.pen_up().unwrap()
    }

    #[test]
    fn strokes_in_region_by_majority() {
        let mut sh = Shadow::new();
        let a = line_stroke(&mut sh, 100, 100, 300); // inside
        let _b = line_stroke(&mut sh, 500, 100, 300); // outside
        let region = BBox { x0: 50, y0: 50, x1: 400, y1: 200 };
        let ids = sh.ids_in(&region);
        assert_eq!(ids, vec![a]);
        let bb = sh.bbox_of(&ids, 0);
        assert!(bb.x0 >= 90 && bb.x1 <= 310 && bb.y0 >= 90 && bb.y1 <= 110, "{bb:?}");
    }

    #[test]
    fn erase_splits_and_forgets() {
        let mut sh = Shadow::new();
        let id = line_stroke(&mut sh, 100, 100, 300);
        sh.erase_point(200, 100, 20);
        assert_eq!(sh.len(), 2, "middle erase should split the stroke");
        // The first fragment keeps the original id; the second gets a new one.
        assert!(sh.get(id).is_some());
        for (_, s) in &sh.strokes {
            for &(x, y) in &s.pts {
                assert!((x - 200).pow(2) + (y - 100).pow(2) > 22 * 22);
            }
        }
        sh.erase_point(150, 100, 500);
        assert_eq!(sh.len(), 0);
    }

    #[test]
    fn undo_pops_newest() {
        let mut sh = Shadow::new();
        let a = line_stroke(&mut sh, 100, 100, 300);
        let b = line_stroke(&mut sh, 200, 100, 300);
        assert_eq!(sh.undo_pop(), Some(b));
        assert_eq!(sh.len(), 1);
        assert!(sh.get(a).is_some());
    }

    #[test]
    fn rasterize_produces_valid_png_with_ink() {
        let mut sh = Shadow::new();
        let id = line_stroke(&mut sh, 100, 100, 400);
        let region = sh.bbox_of(&[id], 10);
        let bytes = sh.rasterize_png(&region, &[id], &[]).unwrap();
        assert_eq!(&bytes[1..4], b"PNG");
        // Decode back and check some pixels are dark.
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut img = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut img).unwrap();
        assert!(info.width > 100);
        assert!(img.iter().any(|&p| p < 100), "no ink rendered");
    }

    #[test]
    fn interpolate_covers_segment() {
        let pts = interpolate((0, 0), (30, 0), 3);
        assert!(pts.len() >= 10);
        assert_eq!(*pts.first().unwrap(), (0, 0));
        assert_eq!(*pts.last().unwrap(), (30, 0));
    }
}
