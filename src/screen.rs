//! Panel geometry. scribe is reMarkable-2-only: 1404×1872 portrait.

pub const SCREEN_W: usize = 1404;
pub const SCREEN_H: usize = 1872;

/// Grow-only pixel bounding box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BBox {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl BBox {
    pub fn empty() -> Self {
        Self { x0: i32::MAX, y0: i32::MAX, x1: i32::MIN, y1: i32::MIN }
    }
    pub fn is_empty(&self) -> bool {
        self.x0 > self.x1
    }
    pub fn add(&mut self, x: i32, y: i32, margin: i32) {
        self.x0 = self.x0.min(x - margin).max(0);
        self.y0 = self.y0.min(y - margin).max(0);
        self.x1 = self.x1.max(x + margin).min(SCREEN_W as i32 - 1);
        self.y1 = self.y1.max(y + margin).min(SCREEN_H as i32 - 1);
    }
    pub fn w(&self) -> i32 {
        self.x1 - self.x0 + 1
    }
    pub fn h(&self) -> i32 {
        self.y1 - self.y0 + 1
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
    pub fn diag(&self) -> f32 {
        ((self.w() as f32).powi(2) + (self.h() as f32).powi(2)).sqrt()
    }
}
