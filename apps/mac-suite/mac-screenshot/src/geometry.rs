// SPDX-License-Identifier: GPL-3.0-only

//! Pure rectangle maths shared by the overlay and the cropper.

/// Axis-aligned rectangle in logical (compositor) coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Integer pixel rectangle inside an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Normalised rectangle spanning two arbitrary corner points.
    pub fn from_points(a: (f32, f32), b: (f32, f32)) -> Self {
        let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
        let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
        Self::new(x0, y0, x1 - x0, y1 - y0)
    }

    pub fn contains(&self, p: (f32, f32)) -> bool {
        p.0 >= self.x && p.0 < self.x + self.w && p.1 >= self.y && p.1 < self.y + self.h
    }

    pub fn area(&self) -> f32 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    /// Clip to `[0,width] x [0,height]`.
    pub fn clamp_to(&self, width: f32, height: f32) -> Self {
        let x0 = self.x.clamp(0.0, width);
        let y0 = self.y.clamp(0.0, height);
        let x1 = (self.x + self.w).clamp(0.0, width);
        let y1 = (self.y + self.h).clamp(0.0, height);
        Self::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Convert to a pixel rectangle of an `img_w x img_h` image that shows
    /// a `logical_w x logical_h` area. Returns `None` when empty.
    pub fn to_pixels(
        &self,
        logical_w: f32,
        logical_h: f32,
        img_w: u32,
        img_h: u32,
    ) -> Option<PixelRect> {
        if logical_w <= 0.0 || logical_h <= 0.0 {
            return None;
        }
        let r = self.clamp_to(logical_w, logical_h);
        let (sx, sy) = (img_w as f32 / logical_w, img_h as f32 / logical_h);
        let x0 = (r.x * sx).round() as u32;
        let y0 = (r.y * sy).round() as u32;
        let x1 = (((r.x + r.w) * sx).round() as u32).min(img_w);
        let y1 = (((r.y + r.h) * sy).round() as u32).min(img_h);
        (x1 > x0 && y1 > y0).then(|| PixelRect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }
}

/// Index of the window under `p`. Z-order is unknown, so the smallest
/// containing window wins (small dialogs sit on top of big windows far more
/// often than the other way round).
pub fn window_at(rects: &[Rect], p: (f32, f32)) -> Option<usize> {
    rects
        .iter()
        .enumerate()
        .filter(|(_, r)| r.contains(p))
        .min_by(|a, b| a.1.area().total_cmp(&b.1.area()))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_points_normalises() {
        let r = Rect::from_points((10.0, 20.0), (4.0, 5.0));
        assert_eq!(r, Rect::new(4.0, 5.0, 6.0, 15.0));
    }

    #[test]
    fn clamp_clips_to_bounds() {
        let r = Rect::new(-5.0, 90.0, 20.0, 50.0).clamp_to(100.0, 100.0);
        assert_eq!(r, Rect::new(0.0, 90.0, 15.0, 10.0));
    }

    #[test]
    fn pixels_scale_with_hidpi() {
        let r = Rect::new(10.0, 10.0, 50.0, 20.0);
        let p = r.to_pixels(100.0, 100.0, 200, 200).unwrap();
        assert_eq!(p, PixelRect { x: 20, y: 20, w: 100, h: 40 });
    }

    #[test]
    fn empty_selection_is_none() {
        assert!(Rect::new(5.0, 5.0, 0.0, 10.0).to_pixels(100.0, 100.0, 100, 100).is_none());
        assert!(Rect::new(200.0, 0.0, 10.0, 10.0).to_pixels(100.0, 100.0, 100, 100).is_none());
    }

    #[test]
    fn smallest_window_wins() {
        let rects = [Rect::new(0.0, 0.0, 100.0, 100.0), Rect::new(10.0, 10.0, 20.0, 20.0)];
        assert_eq!(window_at(&rects, (15.0, 15.0)), Some(1));
        assert_eq!(window_at(&rects, (50.0, 50.0)), Some(0));
        assert_eq!(window_at(&rects, (150.0, 50.0)), None);
    }
}
