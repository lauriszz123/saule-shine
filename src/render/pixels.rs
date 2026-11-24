//! Editing an image's pixels directly: reading and writing single pixels,
//! and the handful of operations a pixel-art editor is built from — lines,
//! rectangles, flood fill, copies and crops.
//!
//! These *replace* pixels rather than blending into them: a pixel editor
//! paints exactly the colour picked, alpha included, so erasing is just
//! painting `0x00000000`. Colours are packed straight (non-premultiplied)
//! ARGB, `0xAARRGGBB`, the same layout the surface stores.
//!
//! They work on images and canvases alike — both live in the same registry —
//! but never on the screen, which is redrawn every frame anyway.

use super::*;
use crate::raster::Surface;

impl Renderer {
    /// The pixels behind an image or canvas handle, for writing.
    fn pixels_mut(&mut self, handle: i64, func: &str) -> Result<&mut Surface, String> {
        let idx = self.canvas_index(handle, func)?;
        self.touch_cpu(Which::Canvas(idx));
        self.canvases[idx]
            .value
            .as_mut()
            .ok_or_else(|| format!("{func}: the image is on loan to a draw call"))
    }

    fn pixels(&mut self, handle: i64, func: &str) -> Result<&Surface, String> {
        let idx = self.canvas_index(handle, func)?;
        self.sync_cpu(Which::Canvas(idx));
        self.canvases[idx]
            .value
            .as_ref()
            .ok_or_else(|| format!("{func}: the image is on loan to a draw call"))
    }

    /// A new, fully transparent image.
    pub(crate) fn new_blank_image(&mut self, w: i64, h: i64) -> Result<i64, String> {
        if w <= 0 || h <= 0 {
            return Err("Graphics.newBlankImage: width and height must be positive".into());
        }
        if w > 16384 || h > 16384 {
            return Err("Graphics.newBlankImage: dimensions may not exceed 16384".into());
        }
        Ok(insert(
            &mut self.canvases,
            &mut self.free_canvases,
            Surface::new(w as usize, h as usize),
        ))
    }

    /// The colour at `(x, y)`, or `0` (transparent) outside the image.
    pub(crate) fn get_pixel(&mut self, handle: i64, x: i64, y: i64) -> Result<u32, String> {
        let s = self.pixels(handle, "Image.getPixel")?;
        Ok(at(s, x, y).map(|i| s.buf[i]).unwrap_or(0))
    }

    /// Set one pixel; outside the image it is ignored.
    pub(crate) fn set_pixel(
        &mut self,
        handle: i64,
        x: i64,
        y: i64,
        argb: u32,
    ) -> Result<(), String> {
        let s = self.pixels_mut(handle, "Image.setPixel")?;
        if let Some(i) = at(s, x, y) {
            s.buf[i] = argb;
        }
        Ok(())
    }

    /// Fill a rectangle, clipped to the image. A negative size is measured
    /// from the other corner, so a drag in any direction fills what it covers.
    pub(crate) fn fill_rect(
        &mut self,
        handle: i64,
        x: i64,
        y: i64,
        w: i64,
        h: i64,
        argb: u32,
    ) -> Result<(), String> {
        let s = self.pixels_mut(handle, "Image.fillRect")?;
        let (x0, x1) = span(x, w, s.w);
        let (y0, y1) = span(y, h, s.h);
        for py in y0..y1 {
            let row = py * s.w;
            s.buf[row + x0..row + x1].fill(argb);
        }
        Ok(())
    }

    /// A one-pixel line from `(x0, y0)` to `(x1, y1)`, both ends included —
    /// Bresenham's, so every step touches exactly one pixel and a stroke has
    /// no gaps and no doubled corners.
    pub(crate) fn line_pixels(
        &mut self,
        handle: i64,
        x0: i64,
        y0: i64,
        x1: i64,
        y1: i64,
        argb: u32,
    ) -> Result<(), String> {
        let s = self.pixels_mut(handle, "Image.drawLine")?;
        let (mut x, mut y) = (x0, y0);
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            if let Some(i) = at(s, x, y) {
                s.buf[i] = argb;
            }
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
        Ok(())
    }

    /// Repaint the 4-connected region of exactly the colour at `(x, y)`.
    /// Returns how many pixels changed — `0` outside the image, or when the
    /// region already is `argb`.
    pub(crate) fn flood_fill(
        &mut self,
        handle: i64,
        x: i64,
        y: i64,
        argb: u32,
    ) -> Result<i64, String> {
        let s = self.pixels_mut(handle, "Image.floodFill")?;
        let Some(start) = at(s, x, y) else {
            return Ok(0);
        };
        let target = s.buf[start];
        if target == argb {
            return Ok(0);
        }

        // An explicit stack rather than recursion: a big empty area is
        // thousands of pixels deep.
        let mut stack = vec![(x as usize, y as usize)];
        let mut changed = 0;
        while let Some((px, py)) = stack.pop() {
            let i = py * s.w + px;
            if s.buf[i] != target {
                continue;
            }
            s.buf[i] = argb;
            changed += 1;
            if px > 0 {
                stack.push((px - 1, py));
            }
            if px + 1 < s.w {
                stack.push((px + 1, py));
            }
            if py > 0 {
                stack.push((px, py - 1));
            }
            if py + 1 < s.h {
                stack.push((px, py + 1));
            }
        }
        Ok(changed)
    }

    /// A new image holding a copy of these pixels — what undo snapshots are.
    pub(crate) fn copy_image(&mut self, handle: i64) -> Result<i64, String> {
        let copy = self.pixels(handle, "Image.copy")?.clone();
        Ok(insert(&mut self.canvases, &mut self.free_canvases, copy))
    }

    /// A new image holding the `w × h` region at `(x, y)`; whatever of the
    /// region lies outside the source comes out transparent.
    pub(crate) fn crop_image(
        &mut self,
        handle: i64,
        x: i64,
        y: i64,
        w: i64,
        h: i64,
    ) -> Result<i64, String> {
        if w <= 0 || h <= 0 {
            return Err("Image.crop: width and height must be positive".into());
        }
        let src = self.pixels(handle, "Image.crop")?;
        let mut out = Surface::new(w as usize, h as usize);
        for oy in 0..h {
            for ox in 0..w {
                if let Some(i) = at(src, x + ox, y + oy) {
                    out.buf[(oy * w + ox) as usize] = src.buf[i];
                }
            }
        }
        Ok(insert(&mut self.canvases, &mut self.free_canvases, out))
    }

    /// Overwrite an image with a frame of little-endian `0x00RRGGBB` pixels
    /// — another process's window, as it arrives in a display server —
    /// made fully opaque, since a framebuffer's alpha byte means nothing.
    /// The frame must be the image's size.
    pub(crate) fn upload_opaque(&mut self, handle: i64, frame: &[u8]) -> Result<(), String> {
        let idx = self.canvas_index(handle, "Compositor")?;
        let size = self.canvases[idx].value.as_ref().map(|s| s.w * s.h * 4);
        if size.is_some_and(|n| frame.len() >= n) && self.upload_frame(idx, frame) {
            return Ok(());
        }
        let dst = self.pixels_mut(handle, "Compositor")?;
        if frame.len() < dst.buf.len() * 4 {
            return Err("Compositor: a frame smaller than its window".into());
        }
        let (pixels, _) = frame.as_chunks::<4>();
        for (px, src) in dst.buf.iter_mut().zip(pixels) {
            *px = 0xFF00_0000 | u32::from_le_bytes([src[0], src[1], src[2], 0]);
        }
        Ok(())
    }

    /// Overwrite these pixels with another image's, which must be the same
    /// size — how an undo puts a snapshot back.
    pub(crate) fn replace_pixels(&mut self, handle: i64, from: i64) -> Result<(), String> {
        let src = self.pixels(from, "Image.replacePixels")?.clone();
        let dst = self.pixels_mut(handle, "Image.replacePixels")?;
        if (src.w, src.h) != (dst.w, dst.h) {
            return Err(format!(
                "Image.replacePixels: the images differ in size ({}x{} and {}x{})",
                dst.w, dst.h, src.w, src.h
            ));
        }
        dst.buf.copy_from_slice(&src.buf);
        Ok(())
    }
}

/// Index of `(x, y)` in `s`, or `None` outside it.
fn at(s: &Surface, x: i64, y: i64) -> Option<usize> {
    (x >= 0 && y >= 0 && (x as usize) < s.w && (y as usize) < s.h)
        .then(|| y as usize * s.w + x as usize)
}

/// The half-open range a rectangle edge covers along one axis, clipped to
/// `0..limit`. A negative length runs backwards from `start`.
fn span(start: i64, len: i64, limit: usize) -> (usize, usize) {
    let (a, b) = if len >= 0 {
        (start, start + len)
    } else {
        (start + len, start)
    };
    let clamp = |v: i64| v.clamp(0, limit as i64) as usize;
    (clamp(a), clamp(b))
}
