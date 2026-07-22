//! Synthetic pen events, written into the Wacom digitizer's evdev node.
//! xochitl cannot tell them from real pen input, so injected strokes become
//! genuine notebook ink (saved, synced, undoable) and injected eraser passes
//! genuinely erase — regardless of which tool the toolbar shows, because
//! BTN_TOOL_RUBBER acts like the Marker's flipped eraser end.

use std::io::{self, Write};
use std::time::Duration;

use crate::evdev::{
    encode, ABS_DISTANCE, ABS_PRESSURE, ABS_X, ABS_Y, BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH,
    EV_ABS, EV_KEY, EV_SYN, SYN_REPORT,
};
use crate::pen::to_raw;
use crate::shadow::interpolate;

pub struct Injector {
    file: std::fs::File,
    /// Injected event frames per second. The real Wacom digitizer runs at
    /// ~140 Hz; sustaining more than that overflows xochitl's reader on
    /// long strokes and Qt discards the whole stroke (and anything injected
    /// in the immediate aftermath). Speed comes from spatial step size, not
    /// frame rate — exactly like a fast-moving real pen.
    hz: u64,
    /// Pen stroke speed, px/s (spatial step = speed / hz).
    pen_speed: u64,
    /// Eraser pass speed, px/s.
    erase_speed: u64,
}

impl Injector {
    pub fn open(dev_path: &str) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new().write(true).open(dev_path)?;
        let hz = std::env::var("SCRIBE_FRAME_HZ").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
        let pen_speed =
            std::env::var("SCRIBE_INK_SPEED").ok().and_then(|v| v.parse().ok()).unwrap_or(2000);
        let erase_speed =
            std::env::var("SCRIBE_ERASE_SPEED").ok().and_then(|v| v.parse().ok()).unwrap_or(6000);
        eprintln!(
            "scribe: injector on {dev_path} ({hz}Hz, pen {pen_speed}px/s, erase {erase_speed}px/s)"
        );
        Ok(Self { file, hz, pen_speed, erase_speed })
    }

    fn frame(&mut self, events: &[(u16, u16, i32)]) -> io::Result<()> {
        let mut buf = Vec::with_capacity((events.len() + 1) * crate::evdev::EV_SIZE);
        for &(t, c, v) in events {
            encode(t, c, v, &mut buf);
        }
        encode(EV_SYN, SYN_REPORT, 0, &mut buf);
        self.file.write_all(&buf)
    }

    fn pace(&self) {
        std::thread::sleep(Duration::from_micros(1_000_000 / self.hz.max(30)));
    }

    fn step_for(&self, speed: u64) -> i32 {
        ((speed / self.hz.max(30)).max(1) as i32).min(40)
    }

    /// Draw one stroke (screen coords) with the pen tool, at real-digitizer
    /// frame rate.
    pub fn pen_stroke(&mut self, pts: &[(i32, i32)]) -> io::Result<()> {
        self.tool_stroke(pts, BTN_TOOL_PEN, 3200, self.step_for(self.pen_speed))
    }

    /// Retrace a path with the eraser tool: one continuous drag covering
    /// three sideways-offset passes (there, back shifted, there again), so
    /// the full ink width is covered without paying tool-up/tool-down
    /// pauses per pass.
    pub fn erase_path(&mut self, pts: &[(i32, i32)]) -> io::Result<()> {
        // Handwritten strokes carry hundreds of raw samples; the eraser
        // brush is fat, so retracing needs only sparse waypoints — xochitl
        // erases along the segments between them. Decimating is what makes
        // erasing fast: frames = path/step instead of one per raw sample.
        //
        // Coverage against leftover specks: five passes offset in both
        // perpendicular directions (alternating travel direction so the
        // eraser never jumps — a jump would erase a line through unrelated
        // ink), plus a scrub circle at each endpoint where ink pools.
        let Some((&first, &last)) = pts.first().zip(pts.last()) else {
            return Ok(());
        };
        let step = self.step_for(self.erase_speed);
        let sparse = decimate(pts, step.min(8));
        let scrub = |c: (i32, i32), out: &mut Vec<(i32, i32)>| {
            for i in 0..=8 {
                let a = i as f32 / 8.0 * std::f32::consts::TAU;
                out.push((c.0 + (6.0 * a.cos()) as i32, c.1 + (6.0 * a.sin()) as i32));
            }
        };
        const OFFS: [(i32, i32); 5] = [(0, 0), (4, 3), (-4, -3), (3, -4), (-3, 4)];
        let mut combined: Vec<(i32, i32)> = Vec::with_capacity(sparse.len() * 5 + 20);
        scrub(first, &mut combined);
        for (k, off) in OFFS.iter().enumerate() {
            if k % 2 == 0 {
                combined.extend(sparse.iter().map(|&(x, y)| (x + off.0, y + off.1)));
            } else {
                combined.extend(sparse.iter().rev().map(|&(x, y)| (x + off.0, y + off.1)));
            }
        }
        scrub(last, &mut combined);
        self.tool_stroke(&combined, BTN_TOOL_RUBBER, 2600, step)
    }

    /// Erase a whole rectangle with horizontal zigzag passes.
    pub fn erase_area(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) -> io::Result<()> {
        let mut path = Vec::new();
        let mut y = y0;
        let mut left_to_right = true;
        while y <= y1 {
            if left_to_right {
                path.push((x0, y));
                path.push((x1, y));
            } else {
                path.push((x1, y));
                path.push((x0, y));
            }
            left_to_right = !left_to_right;
            y += 12; // conservative vs xochitl's rubber brush radius
        }
        self.tool_stroke(&path, BTN_TOOL_RUBBER, 2600, self.step_for(self.erase_speed))
    }

    fn tool_stroke(
        &mut self,
        pts: &[(i32, i32)],
        tool: u16,
        pressure: i32,
        step_px: i32,
    ) -> io::Result<()> {
        if pts.is_empty() {
            return Ok(());
        }
        // Densify.
        let mut dense = Vec::new();
        if pts.len() == 1 {
            dense.push(pts[0]);
            dense.push(pts[0]);
        } else {
            for seg in pts.windows(2) {
                let mut part = interpolate(seg[0], seg[1], step_px);
                if !dense.is_empty() {
                    part.remove(0);
                }
                dense.append(&mut part);
            }
        }

        let (rx0, ry0) = to_raw(dense[0].0, dense[0].1);
        // Tool into range, hover at the start point. The pauses at tool
        // transitions give xochitl's reader room — a dropped transition is
        // what turns the rest of an operation into the wrong tool.
        self.frame(&[
            (EV_KEY, tool, 1),
            (EV_ABS, ABS_X, rx0),
            (EV_ABS, ABS_Y, ry0),
            (EV_ABS, ABS_DISTANCE, 40),
            (EV_ABS, ABS_PRESSURE, 0),
        ])?;
        std::thread::sleep(Duration::from_millis(30));
        // Touch down.
        self.frame(&[
            (EV_KEY, BTN_TOUCH, 1),
            (EV_ABS, ABS_DISTANCE, 0),
            (EV_ABS, ABS_PRESSURE, pressure),
        ])?;
        std::thread::sleep(Duration::from_millis(15));
        for &(x, y) in &dense {
            let (rx, ry) = to_raw(x, y);
            self.frame(&[(EV_ABS, ABS_X, rx), (EV_ABS, ABS_Y, ry), (EV_ABS, ABS_PRESSURE, pressure)])?;
            self.pace();
        }
        // Lift, then tool out of range — with settle room so the lift and
        // tool-out transitions are never in a dropped window.
        self.frame(&[(EV_KEY, BTN_TOUCH, 0), (EV_ABS, ABS_PRESSURE, 0), (EV_ABS, ABS_DISTANCE, 30)])?;
        std::thread::sleep(Duration::from_millis(30));
        self.frame(&[(EV_KEY, tool, 0)])?;
        std::thread::sleep(Duration::from_millis(30));
        Ok(())
    }
}

/// Thin a dense path to waypoints at least `step` px apart (endpoints kept).
fn decimate(pts: &[(i32, i32)], step: i32) -> Vec<(i32, i32)> {
    let s2 = step * step;
    let mut out: Vec<(i32, i32)> = Vec::new();
    for &p in pts {
        if let Some(&last) = out.last() {
            let (dx, dy) = (p.0 - last.0, p.1 - last.1);
            if dx * dx + dy * dy < s2 {
                continue;
            }
        }
        out.push(p);
    }
    if out.last() != pts.last() {
        if let Some(&l) = pts.last() {
            out.push(l);
        }
    }
    out
}
