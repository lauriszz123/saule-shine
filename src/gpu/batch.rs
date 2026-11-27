//! Batching: shapes drawn one after another onto the same target in the
//! same blend mode — rectangles and shapes from the shape atlas, the bulk of
//! a UI — are gathered and drawn together, one draw for the run.
//!
//! A shape adds its quad to the batch ([`add`]); anything else the GPU is
//! asked to do first draws what has gathered ([`flush`]): every other
//! drawing call, every upload, read-back and copy, putting a frame on the
//! screen, and anything that changes the shape atlas the batch samples. So
//! the order things land in is always the order they were asked for.

use std::cell::RefCell;

use glow::HasContext;

use super::device::{bytes_of, Device};
use super::texture::Texture;
use crate::raster::BlendMode;

/// Floats in one vertex: place (2), rectangle (4), colour (4), mask (4),
/// flags (2).
const FLOATS: usize = 16;

/// The most squares in one draw: their corners are numbered in sixteen
/// bits (`Device::squares`). A run longer than this is drawn in parts.
///
/// A square is its four corners, and which three of them make each of its
/// triangles is said once, for every square there will ever be. Sent as
/// two triangles of three corners each, a third of what went to the chip
/// was said twice; and on a chip that leaves each corner to the processor
/// (see `Renderer::gpu_text_kept`) a third of a frame's drawing was that.
pub(crate) const MOST: usize = 8192;

#[derive(Default)]
pub(crate) struct Batch {
    /// Where the run draws: the target's framebuffer and size.
    target: Option<(glow::Framebuffer, usize, usize)>,
    blend: Option<BlendMode>,
    vertices: Vec<f32>,
}

pub(crate) type Pending = RefCell<Batch>;

/// One shape for the batch: the pixels it covers, how its coverage is found
/// (`rect` worked out, or a mask in the shape atlas), its colour (straight
/// RGBA) and whether it is antialiased.
pub(crate) struct Quad {
    pub(crate) area: (f32, f32, f32, f32),
    pub(crate) rect: [f32; 4],
    pub(crate) mask: Option<[f32; 4]>,
    pub(crate) colour: [f32; 4],
    pub(crate) antialias: bool,
}

/// Add `quad`, drawn onto `dst` in `blend`, to the run; a run onto another
/// target or in another mode is drawn first.
pub(crate) fn add(dst: &Texture, blend: BlendMode, quad: &Quad) {
    let dev = &dst.dev;
    let framebuffer = dst.framebuffer();
    // The target by its framebuffer and its size: a texture made after
    // another was deleted (the screen's, when the window is resized) can be
    // given the same framebuffer name, and must not be drawn on at the old
    // one's size.
    let same = {
        let batch = dev.batch.borrow();
        batch.target == Some((framebuffer, dst.w, dst.h)) && batch.blend == Some(blend)
    };
    if !same {
        flush(dev);
        let mut batch = dev.batch.borrow_mut();
        batch.target = Some((framebuffer, dst.w, dst.h));
        batch.blend = Some(blend);
    }
    if dev.batch.borrow().vertices.len() >= MOST * 4 * FLOATS {
        flush(dev);
        let mut batch = dev.batch.borrow_mut();
        batch.target = Some((framebuffer, dst.w, dst.h));
        batch.blend = Some(blend);
    }
    let mut batch = dev.batch.borrow_mut();
    let (x0, y0, x1, y1) = quad.area;
    let [r, g, b, a] = quad.colour;
    let mask = quad.mask.unwrap_or([0.0; 4]);
    let flags = [
        if quad.mask.is_some() { 1.0 } else { 0.0 },
        if quad.antialias { 1.0 } else { 0.0 },
    ];
    for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
        batch.vertices.extend_from_slice(&[x, y]);
        batch.vertices.extend_from_slice(&quad.rect);
        // Red and blue swapped, as the textures hold them.
        batch.vertices.extend_from_slice(&[b, g, r, a]);
        batch.vertices.extend_from_slice(&mask);
        batch.vertices.extend_from_slice(&flags);
    }
}

/// Draw what has gathered, if anything.
pub(crate) fn flush(dev: &Device) {
    let mut batch = dev.batch.borrow_mut();
    // Drawn or not, the run ends here: the next shape starts a new one,
    // whatever its target is now.
    let (target, blend) = (batch.target.take(), batch.blend.take());
    if batch.vertices.is_empty() {
        return;
    }
    let (Some((framebuffer, w, h)), Some(blend)) = (target, blend) else {
        batch.vertices.clear();
        return;
    };
    let gl = &dev.gl;
    let p = &dev.fill_batch;
    let shapes = dev.shapes.borrow();
    // SAFETY: GL calls on the device's current context; the framebuffer is
    // the target's, which can't be gone (a texture flushes before it goes).
    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
        gl.viewport(0, 0, w as i32, h as i32);
        gl.disable(glow::SCISSOR_TEST);
        super::blend_state(gl, blend);
        gl.use_program(Some(p.program));
        gl.uniform_2_f32(p.target.as_ref(), w as f32, h as f32);
        gl.uniform_1_f32(p.mask_scale.as_ref(), 1.0 / shapes.size as f32);
        gl.uniform_1_f32(p.mode.as_ref(), super::mode_number(blend));
        gl.active_texture(glow::TEXTURE1);
        gl.bind_texture(glow::TEXTURE_2D, Some(shapes.texture));
        gl.active_texture(glow::TEXTURE0);
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(dev.stream));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes_of(&batch.vertices), glow::STREAM_DRAW);
        let stride = (FLOATS * 4) as i32;
        for (location, size, offset) in [(0, 2, 0), (1, 4, 2), (2, 4, 6), (3, 4, 10), (4, 2, 14)] {
            gl.enable_vertex_attrib_array(location);
            gl.vertex_attrib_pointer_f32(location, size, glow::FLOAT, false, stride, offset * 4);
        }
        let squares = batch.vertices.len() / FLOATS / 4;
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(dev.squares));
        gl.draw_elements(glow::TRIANGLES, (squares * 6) as i32, glow::UNSIGNED_SHORT, 0);
        for location in 1..5 {
            gl.disable_vertex_attrib_array(location);
        }
    }
    crate::trace::batched((batch.vertices.len() / FLOATS / 4) as u32);
    batch.vertices.clear();
}
