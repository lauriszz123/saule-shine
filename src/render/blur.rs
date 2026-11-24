//! `Graphics.blur`: what is already drawn under a rectangle, blurred — the
//! frosted glass behind a panel (see `raster::blur`, and `gpu::blur` for
//! the same on the GPU).

use super::*;
use crate::raster::{self, BlurArea, Rect};

impl Renderer {
    /// Blur the target under the rectangle at (`x`, `y`), `w` × `h` in the
    /// current coordinates, with corners rounded by `corner`: pixels within
    /// `radius` mix. `opacity` fades the blur in over what is there; `fade`
    /// fades it out over that much of the rectangle's bottom. Inside the
    /// scissor only. Turned or skewed, the rectangle's box is blurred.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn blur(
        &mut self,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        radius: f64,
        corner: f64,
        opacity: f64,
        fade: f64,
    ) -> Result<(), String> {
        let t = self.st.transform;
        let corners = [t.apply(x, y), t.apply(x + w, y), t.apply(x, y + h), t.apply(x + w, y + h)];
        let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
        let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (px, py) in corners {
            (x0, y0, x1, y1) = (x0.min(px), y0.min(py), x1.max(px), y1.max(py));
        }
        // Lengths grow with the transform's scale.
        let scale = (t.a * t.d - t.b * t.c).abs().sqrt();

        let (tw, th) = self.target_size();
        let clip = match self.st.scissor {
            Some(s) => s.intersect(&Rect::surface(tw, th)),
            None => Rect::surface(tw, th),
        };
        if clip.is_empty() {
            return Ok(());
        }
        let Some(area) = BlurArea::new(
            (x0 as f32, y0 as f32, x1 as f32, y1 as f32),
            (corner * scale) as f32,
            (radius * scale) as f32,
            opacity as f32,
            (fade * scale) as f32,
            clip.pixel_bounds(tw, th),
            (tw, th),
        ) else {
            return Ok(());
        };
        if self.gpu_blur(&area) {
            return Ok(());
        }
        raster::blur(self.target_mut(), &area);
        Ok(())
    }
}
