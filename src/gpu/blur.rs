//! The blur on the GPU (see `raster::blur` for what it is): the target's
//! pixels round the rectangle copied aside, blurred across into a second
//! texture, then down from that into the target, mixed by the rounded
//! rectangle's coverage. Two textures kept between blurs (`BlurTextures`),
//! grown when a blur needs more.

use std::rc::Rc;

use glow::HasContext;

use super::device::Device;
use super::texture::Texture;
use super::Clip;
use crate::raster::BlurArea;

/// The two textures a blur works in: the pixels it reads, copied from the
/// target, and the blur across.
pub(crate) struct BlurTextures {
    copy: Texture,
    across: Texture,
}

impl BlurTextures {
    /// Textures for a `w` × `h` target's blur of `area`: `kept`, if it is
    /// large enough, else new ones.
    pub(crate) fn fit(
        dev: &Rc<Device>,
        kept: Option<BlurTextures>,
        area: &BlurArea,
        (w, h): (usize, usize),
    ) -> Result<BlurTextures, String> {
        let (sx0, sy0, sx1, sy1) = area.source(w, h);
        let (x0, _, x1, _) = area.pixels;
        let (cw, ch) = (sx1 - sx0, sy1 - sy0);
        let aw = x1 - x0;
        if let Some(kept) = kept
            && kept.copy.w >= cw
            && kept.copy.h >= ch
            && kept.across.w >= aw
            && kept.across.h >= ch
        {
            return Ok(kept);
        }
        // A little room to grow, so a panel that changes size by a pixel
        // doesn't make new ones every frame.
        let up = |n: usize| (n + 63) / 64 * 64;
        Ok(BlurTextures {
            copy: Texture::new(dev, up(cw), up(ch))?,
            across: Texture::new(dev, up(aw), up(ch))?,
        })
    }
}

/// Blur `area` of `dst`, a `w` × `h` surface, using `textures`.
pub(crate) fn blur(dst: &Texture, textures: &BlurTextures, area: &BlurArea) {
    let dev = &dst.dev;
    let gl = &dev.gl;
    let p = &dev.blur;
    let (w, h) = (dst.w, dst.h);
    let (sx0, sy0, sx1, sy1) = area.source(w, h);
    let (x0, y0, x1, y1) = area.pixels;
    let (copy, across) = (&textures.copy, &textures.across);

    // What is there now, round the rectangle, aside.
    copy.copy_region_from(dst, sx0, sy0, sx1 - sx0, sy1 - sy0);

    // SAFETY: GL calls on the device's current context.
    unsafe {
        gl.use_program(Some(p.program));
        gl.uniform_1_f32(p.reach.as_ref(), area.k as f32);
        gl.uniform_1_f32(p.sigma.as_ref(), area.sigma);
        gl.uniform_2_f32(p.surface.as_ref(), w as f32, h as f32);
        let (a, b, c, d) = area.shape;
        gl.uniform_4_f32(p.shape.as_ref(), a, b, c, d);
        gl.uniform_1_f32(p.corner.as_ref(), area.corner);
        gl.uniform_1_f32(p.opacity.as_ref(), area.opacity);
        gl.uniform_1_f32(p.fade.as_ref(), area.fade);
    }

    // Across: every row read below, the columns written.
    across.bind_target();
    Clip { x0: 0, y0: 0, x1: x1 - x0, y1: sy1 - sy0 }.apply(gl);
    // SAFETY: as above, with `across` the render target.
    unsafe {
        gl.disable(glow::BLEND);
        gl.use_program(Some(p.program));
        gl.uniform_1_f32(p.down.as_ref(), 0.0);
        gl.uniform_2_f32(p.here.as_ref(), x0 as f32, sy0 as f32);
        gl.uniform_2_f32(p.source_origin.as_ref(), sx0 as f32, sy0 as f32);
        gl.uniform_2_f32(p.source_size.as_ref(), copy.w as f32, copy.h as f32);
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(copy.texture));
        dev.bind_quad();
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
    }

    // Down, into the target, over what was there.
    dst.bind_target();
    Clip { x0, y0, x1, y1 }.apply(gl);
    // SAFETY: as above, with `dst` the render target; it is sampled from
    // neither pass, only from its copy.
    unsafe {
        gl.disable(glow::BLEND);
        gl.use_program(Some(p.program));
        gl.uniform_1_f32(p.down.as_ref(), 1.0);
        gl.uniform_2_f32(p.here.as_ref(), 0.0, 0.0);
        gl.uniform_2_f32(p.source_origin.as_ref(), x0 as f32, sy0 as f32);
        gl.uniform_2_f32(p.source_size.as_ref(), across.w as f32, across.h as f32);
        gl.uniform_2_f32(p.original_origin.as_ref(), sx0 as f32, sy0 as f32);
        gl.uniform_2_f32(p.original_size.as_ref(), copy.w as f32, copy.h as f32);
        gl.active_texture(glow::TEXTURE1);
        gl.bind_texture(glow::TEXTURE_2D, Some(copy.texture));
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(across.texture));
        dev.bind_quad();
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
    }
}
