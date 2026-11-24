//! Blurring what is already drawn under a rectangle: a Gaussian blur, as a
//! frosted panel shows what is behind it. The CPU's way here; the GPU's
//! (`gpu::blur`) works through the same steps with the same numbers, so the
//! two agree to a level or so of each channel.
//!
//! Exact rather than approximated: every pixel within `k` of a pixel
//! counts, weighted by the Gaussian, in two passes — across, then down.
//! Past the surface's edges the edge pixels stand in, so a panel at the
//! edge of the screen doesn't darken there. The rectangle may have rounded
//! corners, antialiased, and the blur may be faded in (`opacity`), which a
//! panel coming onto the screen uses.

use super::pixel::Surface;

/// The widest blur: pixels within this many of each other mix.
pub(crate) const MOST: usize = 64;

/// One blur, in device pixels.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BlurArea {
    /// The pixels written: the rectangle cut to the clip and the surface.
    pub(crate) pixels: (usize, usize, usize, usize),
    /// The rectangle itself, and the radius of its corners.
    pub(crate) shape: (f32, f32, f32, f32),
    pub(crate) corner: f32,
    /// How far the blur reaches, and its Gaussian's deviation.
    pub(crate) k: usize,
    pub(crate) sigma: f32,
    /// How much of the blur shows: 1 all of it.
    pub(crate) opacity: f32,
    /// How far up from the rectangle's bottom edge the blur fades out, to
    /// nothing at the edge; 0 for no fade. A bar along the top of a screen
    /// fades into what is under it this way.
    pub(crate) fade: f32,
}

impl BlurArea {
    /// A blur of the rectangle `shape` reaching `radius` pixels, written
    /// inside `clip` (pixel bounds) of a `w` × `h` surface; `None` when it
    /// would change nothing.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        shape: (f32, f32, f32, f32),
        corner: f32,
        radius: f32,
        opacity: f32,
        fade: f32,
        clip: (usize, usize, usize, usize),
        (w, h): (usize, usize),
    ) -> Option<BlurArea> {
        let (sx0, sy0, sx1, sy1) = shape;
        if !(radius >= 0.5) || !(opacity > 0.0) || sx1 <= sx0 || sy1 <= sy0 {
            return None;
        }
        let x0 = (sx0.floor().max(0.0) as usize).max(clip.0);
        let y0 = (sy0.floor().max(0.0) as usize).max(clip.1);
        let x1 = (sx1.ceil().max(0.0) as usize).min(clip.2).min(w);
        let y1 = (sy1.ceil().max(0.0) as usize).min(clip.3).min(h);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let k = (radius.ceil() as usize).clamp(1, MOST);
        Some(BlurArea {
            pixels: (x0, y0, x1, y1),
            shape,
            corner: corner.max(0.0),
            k,
            // The reach is three deviations: past it, a weight is under 1%
            // of the middle one.
            sigma: (radius.min(MOST as f32) / 3.0).max(0.3),
            opacity: opacity.min(1.0),
            fade: fade.max(0.0),
        })
    }

    /// The pixels read: those written and `k` round them, inside the
    /// surface.
    pub(crate) fn source(&self, w: usize, h: usize) -> (usize, usize, usize, usize) {
        let (x0, y0, x1, y1) = self.pixels;
        (
            x0.saturating_sub(self.k),
            y0.saturating_sub(self.k),
            (x1 + self.k).min(w),
            (y1 + self.k).min(h),
        )
    }

    /// The Gaussian's weights from `-k` to `k`, and their sum.
    fn weights(&self) -> (Vec<f32>, f32) {
        let k = self.k as i64;
        let two_s2 = 2.0 * self.sigma * self.sigma;
        let weights: Vec<f32> = (-k..=k).map(|i| (-((i * i) as f32) / two_s2).exp()).collect();
        let total = weights.iter().sum();
        (weights, total)
    }

    /// How much of the pixel centred at (`px`, `py`) the rounded rectangle
    /// covers, by its distance from the edge, times the opacity.
    pub(crate) fn coverage(&self, px: f32, py: f32) -> f32 {
        let (x0, y0, x1, y1) = self.shape;
        let r = self.corner.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0);
        let qx = (px - (x0 + x1) / 2.0).abs() - ((x1 - x0) / 2.0 - r);
        let qy = (py - (y0 + y1) / 2.0).abs() - ((y1 - y0) / 2.0 - r);
        let outside = qx.max(0.0).hypot(qy.max(0.0));
        let d = outside + qx.max(qy).min(0.0) - r;
        let fading = if self.fade > 0.0 {
            ((y1 - py) / self.fade).clamp(0.0, 1.0)
        } else {
            1.0
        };
        (0.5 - d).clamp(0.0, 1.0) * self.opacity * fading
    }
}

fn channels(p: u32) -> [f32; 4] {
    [
        ((p >> 24) & 0xFF) as f32,
        ((p >> 16) & 0xFF) as f32,
        ((p >> 8) & 0xFF) as f32,
        (p & 0xFF) as f32,
    ]
}

fn word(c: [f32; 4]) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 255.0) + 0.5) as u32;
    (q(c[0]) << 24) | (q(c[1]) << 16) | (q(c[2]) << 8) | q(c[3])
}

/// Blur `area` of `surface`, in place.
pub(crate) fn blur(surface: &mut Surface, area: &BlurArea) {
    let (w, h) = (surface.w, surface.h);
    let (x0, y0, x1, y1) = area.pixels;
    let (_, py0, _, py1) = area.source(w, h);
    let (weights, total) = area.weights();
    let k = area.k as i64;
    let rw = x1 - x0;

    // Across, for every row a pixel written reads: kept to whole levels, as
    // the GPU keeps them in a texture between its two passes.
    let mut across = vec![0u32; rw * (py1 - py0)];
    for y in py0..py1 {
        let row = &surface.buf[y * w..(y + 1) * w];
        for x in x0..x1 {
            let mut sum = [0.0f32; 4];
            for (i, weight) in weights.iter().enumerate() {
                let sx = (x as i64 + i as i64 - k).clamp(0, w as i64 - 1) as usize;
                let c = channels(row[sx]);
                for n in 0..4 {
                    sum[n] += weight * c[n];
                }
            }
            across[(y - py0) * rw + (x - x0)] = word(sum.map(|s| s / total));
        }
    }

    // Down, mixed into what was there by the coverage.
    for y in y0..y1 {
        for x in x0..x1 {
            let c = area.coverage(x as f32 + 0.5, y as f32 + 0.5);
            if c <= 0.0 {
                continue;
            }
            let mut sum = [0.0f32; 4];
            for (j, weight) in weights.iter().enumerate() {
                let sy = (y as i64 + j as i64 - k).clamp(0, h as i64 - 1) as usize;
                let v = channels(across[(sy - py0) * rw + (x - x0)]);
                for n in 0..4 {
                    sum[n] += weight * v[n];
                }
            }
            let at = y * w + x;
            let was = channels(surface.buf[at]);
            let mut out = [0.0f32; 4];
            for n in 0..4 {
                out[n] = was[n] + (sum[n] / total - was[n]) * c;
            }
            surface.buf[at] = word(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(shape: (f32, f32, f32, f32), radius: f32, w: usize, h: usize) -> BlurArea {
        BlurArea::new(shape, 0.0, radius, 1.0, 0.0, (0, 0, w, h), (w, h)).expect("an area")
    }

    #[test]
    fn a_flat_colour_stays_as_it_is() {
        let mut s = Surface::opaque(40, 30);
        s.buf.fill(0xFF33_6699);
        blur(&mut s, &area((5.0, 5.0, 35.0, 25.0), 8.0, 40, 30));
        assert!(s.buf.iter().all(|&p| p == 0xFF33_6699));
    }

    #[test]
    fn an_edge_is_softened_both_ways_and_kept_in_order() {
        // Left half black, right half white: across the edge the blur rises
        // steadily from one to the other.
        let mut s = Surface::opaque(60, 10);
        for y in 0..10 {
            for x in 30..60 {
                s.buf[y * 60 + x] = 0xFFFF_FFFF;
            }
        }
        blur(&mut s, &area((0.0, 0.0, 60.0, 10.0), 9.0, 60, 10));
        let row: Vec<u32> = (0..60).map(|x| s.buf[5 * 60 + x] & 0xFF).collect();
        assert_eq!(row[0], 0);
        assert_eq!(row[59], 255);
        assert!(row[29] > 60 && row[29] < 128, "{row:?}");
        assert!(row[30] > 128 && row[30] < 195, "{row:?}");
        assert!(row.windows(2).all(|p| p[0] <= p[1]), "{row:?}");
    }

    #[test]
    fn only_the_rectangle_changes() {
        let mut s = Surface::opaque(40, 40);
        for (i, p) in s.buf.iter_mut().enumerate() {
            *p = 0xFF00_0000 | ((i as u32 * 37) & 0xFF);
        }
        let before = s.buf.clone();
        blur(&mut s, &area((10.0, 10.0, 20.0, 20.0), 4.0, 40, 40));
        for y in 0..40 {
            for x in 0..40 {
                if !(10..20).contains(&x) || !(10..20).contains(&y) {
                    assert_eq!(s.buf[y * 40 + x], before[y * 40 + x], "({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn rounded_corners_leave_the_corner_pixel() {
        let mut s = Surface::opaque(40, 40);
        for (i, p) in s.buf.iter_mut().enumerate() {
            *p = if i % 2 == 0 { 0xFFFF_FFFF } else { 0xFF00_0000 };
        }
        let before = s.buf.clone();
        let a = BlurArea::new((0.0, 0.0, 40.0, 40.0), 12.0, 4.0, 1.0, 0.0, (0, 0, 40, 40), (40, 40)).unwrap();
        blur(&mut s, &a);
        assert_eq!(s.buf[0], before[0]);
        assert_ne!(s.buf[20 * 40 + 20], before[20 * 40 + 20]);
    }
}
