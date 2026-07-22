//! DRAW replies: when the user asks scribe to draw something, the oracle
//! answers with polylines instead of prose —
//!
//!   DRAW
//!   100,100 900,100 900,900 100,900 100,100
//!   200,300 800,300
//!
//! One polyline per line, x,y pairs in a 0–1000 space (origin top-left).
//! scribe scales them to fit the circled region and injects them as pen
//! strokes. (Without this the model draws ASCII art, and a cursive
//! rendering of `+--+` is nobody's square.)

use crate::screen::BBox;

/// Parse a DRAW reply. None = not a drawing (render as text instead).
pub fn parse(text: &str) -> Option<Vec<Vec<(f32, f32)>>> {
    let mut lines = text.trim().lines();
    if lines.next()?.trim().to_ascii_uppercase() != "DRAW" {
        return None;
    }
    let mut polys = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut poly = Vec::new();
        for pair in line.split_whitespace() {
            let (x, y) = pair.split_once(',')?;
            poly.push((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?));
        }
        if poly.len() >= 2 {
            polys.push(poly);
        }
    }
    if polys.is_empty() {
        None
    } else {
        Some(polys)
    }
}

/// Scale polylines uniformly to fit `region` (aspect preserved, centered).
pub fn place(polys: &[Vec<(f32, f32)>], region: &BBox) -> Vec<Vec<(i32, i32)>> {
    let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
    let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
    for p in polys.iter().flatten() {
        min_x = min_x.min(p.0);
        min_y = min_y.min(p.1);
        max_x = max_x.max(p.0);
        max_y = max_y.max(p.1);
    }
    let (dw, dh) = ((max_x - min_x).max(1.0), (max_y - min_y).max(1.0));
    let scale = ((region.w() as f32 - 8.0) / dw).min((region.h() as f32 - 8.0) / dh);
    let ox = region.x0 as f32 + (region.w() as f32 - dw * scale) / 2.0;
    let oy = region.y0 as f32 + (region.h() as f32 - dh * scale) / 2.0;
    polys
        .iter()
        .map(|poly| {
            poly.iter()
                .map(|&(x, y)| {
                    (
                        (ox + (x - min_x) * scale).round() as i32,
                        (oy + (y - min_y) * scale).round() as i32,
                    )
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_square() {
        let polys = parse("DRAW\n100,100 900,100 900,900 100,900 100,100\n").unwrap();
        assert_eq!(polys.len(), 1);
        assert_eq!(polys[0].len(), 5);
        assert_eq!(polys[0][2], (900.0, 900.0));
    }

    #[test]
    fn plain_text_is_not_a_drawing() {
        assert!(parse("Here is some text").is_none());
        assert!(parse("").is_none());
        // A DRAW header with garbage coordinates falls back to text too.
        assert!(parse("DRAW\nnot,numbers here").is_none());
        // DRAW with no polylines at all.
        assert!(parse("DRAW\n").is_none());
    }

    #[test]
    fn place_fits_and_centers_in_region() {
        let polys = parse("DRAW\n0,0 1000,0 1000,1000 0,1000 0,0").unwrap();
        let region = BBox { x0: 100, y0: 200, x1: 499, y1: 799 };
        let strokes = place(&polys, &region);
        let xs: Vec<i32> = strokes.iter().flatten().map(|p| p.0).collect();
        let ys: Vec<i32> = strokes.iter().flatten().map(|p| p.1).collect();
        // Square (aspect 1:1) must fit the 400-wide region and center in the
        // 600-tall one.
        assert!(*xs.iter().min().unwrap() >= 100 && *xs.iter().max().unwrap() <= 499);
        assert!(*ys.iter().min().unwrap() > 250 && *ys.iter().max().unwrap() < 750);
        let w = xs.iter().max().unwrap() - xs.iter().min().unwrap();
        let h = ys.iter().max().unwrap() - ys.iter().min().unwrap();
        assert!((w - h).abs() <= 2, "aspect preserved: {w}x{h}");
    }
}
