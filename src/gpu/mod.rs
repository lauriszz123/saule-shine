//! The GPU: an OpenGL context on the window, and what Shine does with it.
//!
//! ## What runs where
//!
//! Shine draws a frame on the CPU, into the screen [`crate::raster::Surface`],
//! and the GPU puts it on the window: the frame is uploaded into a texture
//! and drawn over the whole window, then the buffers are swapped. That
//! replaces the X server copying every pixel down its socket (`XPutImage`,
//! or the shared-memory put in `state/shm.rs`) and minifb's own resampling
//! pass, and it lets the driver pace the swap.
//!
//! Drawing itself moves onto the GPU a part at a time (`docs/gpu.md` in this
//! repository has the plan); the CPU renderer stays, as the reference every
//! GPU path is tested against and as the fallback where there is no GPU.
//!
//! ## Which OpenGL
//!
//! OpenGL ES 2.0, or desktop OpenGL 2.1 where ES isn't offered (Windows'
//! WGL, macOS). It is the newest every target has: the netbook Saule Linux
//! is built for has an Intel GMA 3150, which stops at ES 2.0, so anything
//! newer — and wgpu, which needs ES 3.0 — would leave it on the CPU. Both
//! dialects share one set of shaders (see [`Program::new`]).
//!
//! ## Where a context comes from
//!
//! glutin, on the raw window handle minifb exposes: EGL then GLX on X11,
//! WGL then EGL (ANGLE) on Windows, CGL on macOS. The GL entry points are
//! looked up at run time (glow), so nothing links against a GL library:
//! where none can be found, [`Presenter::new`] says so and the window keeps
//! presenting the way it did without a GPU.
//!
//! `SHINE_GPU=0` keeps the GPU out of it, for comparing or for a driver that
//! misbehaves. `SHINE_TRACE_PRESENT=1` reports which way frames go.

mod atlas;
pub(crate) mod batch;
pub(crate) mod blur;
mod device;
#[cfg(target_os = "linux")]
pub(crate) mod dmabuf;
mod present;
mod program;
mod texture;

use glow::HasContext;

pub(crate) use atlas::GlyphKey;
pub(crate) use device::Device;
pub(crate) use present::{show, Presenter};
pub(crate) use texture::Texture;

use crate::geom::Transform;
use crate::raster::{BlendMode, Gradient, GradientKind};

/// Whether drawing itself goes to the GPU, not only presenting: yes,
/// unless `SHINE_GPU_DRAW=0` (or `SHINE_GPU=0`) says not. Everything Shine
/// draws has a GPU path now but the replace blend mode and pixel-by-pixel
/// image edits, which the CPU does on its copy (see `docs/gpu.md`).
pub(crate) fn drawing_wanted() -> bool {
    static WANTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WANTED.get_or_init(|| {
        wanted() && std::env::var_os("SHINE_GPU_DRAW").is_none_or(|v| v != "0")
    })
}

/// A pixel rectangle `x0..x1` × `y0..y1` of a target, rows counted from the
/// top as the CPU counts them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Clip {
    pub(crate) x0: usize,
    pub(crate) y0: usize,
    pub(crate) x1: usize,
    pub(crate) y1: usize,
}

impl Clip {
    pub(crate) fn apply(&self, gl: &glow::Context) {
        // SAFETY: GL calls on the current context. Target textures aren't
        // flipped, so the scissor's rows are the CPU's.
        unsafe {
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(
                self.x0 as i32,
                self.y0 as i32,
                self.x1.saturating_sub(self.x0) as i32,
                self.y1.saturating_sub(self.y0) as i32,
            );
        }
    }
}

/// One image region drawn onto a target, as `raster::blit_surface_sub` draws
/// it with the alpha blend mode.
pub(crate) struct Blit {
    /// The region of the source: its corner and its size, in pixels.
    pub(crate) sub: (f64, f64, f64, f64),
    /// Region space to the target's pixels.
    pub(crate) xform: Transform,
    /// The paint colour, straight RGBA: the image is multiplied by it.
    pub(crate) tint: [f32; 4],
    pub(crate) corner_radius: f64,
    pub(crate) linear: bool,
    pub(crate) clip: Clip,
    /// Whether the shader decides which pixels are inside the region: not
    /// when `clip` already holds exactly those (see `render::mirror`).
    pub(crate) check_inside: bool,
    /// The source's alpha bytes mean nothing: a window's frame, uploaded
    /// as it came (see `Renderer::upload_opaque`). Read as opaque.
    pub(crate) opaque: bool,
    pub(crate) blend: BlendMode,
}

/// Whether `blend` can be drawn on the GPU. All of them can, but replace
/// only where every pixel is fully covered: its partial cover (a rounded
/// corner) is a mix the blend stage has no factor for.
pub(crate) fn can_blend(blend: BlendMode, corner_radius: f64) -> bool {
    blend != BlendMode::Replace || corner_radius <= 0.0
}

/// The blend stage for one of the CPU's blend modes (`Surface::blend`), with
/// what the blit shader outputs for it:
///
/// - alpha: `s·a + d·(1 − a)`, and alpha `a + da·(1 − a)`;
/// - add, subtract: `d ± s·a`, alpha unchanged;
/// - multiply: `d · (s·a + 1 − a)` for colour and alpha alike, the shader
///   giving the factor;
/// - screen: `s·a + d·(1 − s·a)`, the shader giving `s·a`;
/// - replace: the source as it is, no blending.
fn blend_state(gl: &glow::Context, blend: BlendMode) {
    // SAFETY: GL calls on the current context.
    unsafe {
        if blend == BlendMode::Replace {
            gl.disable(glow::BLEND);
            return;
        }
        gl.enable(glow::BLEND);
        let (rgb_eq, rgb_src, rgb_dst, a_src, a_dst) = match blend {
            BlendMode::Alpha => (
                glow::FUNC_ADD,
                glow::SRC_ALPHA,
                glow::ONE_MINUS_SRC_ALPHA,
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
            ),
            BlendMode::Add => (glow::FUNC_ADD, glow::SRC_ALPHA, glow::ONE, glow::ZERO, glow::ONE),
            BlendMode::Subtract => (
                glow::FUNC_REVERSE_SUBTRACT,
                glow::SRC_ALPHA,
                glow::ONE,
                glow::ZERO,
                glow::ONE,
            ),
            BlendMode::Multiply => {
                (glow::FUNC_ADD, glow::ZERO, glow::SRC_COLOR, glow::ZERO, glow::SRC_ALPHA)
            }
            BlendMode::Screen => (
                glow::FUNC_ADD,
                glow::ONE,
                glow::ONE_MINUS_SRC_COLOR,
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
            ),
            BlendMode::Premultiplied => (
                glow::FUNC_ADD,
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
            ),
            BlendMode::Replace => unreachable!("handled above"),
        };
        gl.blend_equation_separate(rgb_eq, glow::FUNC_ADD);
        gl.blend_func_separate(rgb_src, rgb_dst, a_src, a_dst);
    }
}

/// The shader's number for a blend mode (see `BlitProgram`).
fn mode_number(blend: BlendMode) -> f32 {
    match blend {
        BlendMode::Alpha => 0.0,
        BlendMode::Add => 1.0,
        BlendMode::Subtract => 2.0,
        BlendMode::Multiply => 3.0,
        BlendMode::Screen => 4.0,
        BlendMode::Replace => 5.0,
        BlendMode::Premultiplied => 6.0,
    }
}

/// Draw `blit` of `src` onto `dst`, blending over what is there.
pub(crate) fn blit(dst: &Texture, src: &Texture, blit: &Blit) {
    let dev = &dst.dev;
    let gl = &dev.gl;
    let p = &dev.blit;
    let (sx, sy, sw, sh) = blit.sub;
    let t = &blit.xform;
    let radius = blit.corner_radius.min(sw / 2.0).min(sh / 2.0).max(0.0);
    dst.bind_target();
    blit.clip.apply(gl);
    blend_state(gl, blit.blend);
    // SAFETY: GL calls on the device's current context.
    unsafe {
        gl.use_program(Some(p.program));
        gl.uniform_2_f32(p.target.as_ref(), dst.w as f32, dst.h as f32);
        gl.uniform_2_f32(p.size.as_ref(), sw as f32, sh as f32);
        // A device pixel in region units, along the region's shorter-scaled
        // axis: what the quad is grown by (see `BlitProgram`).
        let scale = t.a.hypot(t.b).min(t.c.hypot(t.d));
        let pad = if scale > 1e-9 { (1.0 / scale).min(1e6) } else { 0.0 };
        gl.uniform_1_f32(p.pad.as_ref(), pad as f32 + 0.5);
        gl.uniform_3_f32(p.row0.as_ref(), t.a as f32, t.c as f32, t.tx as f32);
        gl.uniform_3_f32(p.row1.as_ref(), t.b as f32, t.d as f32, t.ty as f32);
        gl.uniform_2_f32(p.texture_size.as_ref(), src.w as f32, src.h as f32);
        gl.uniform_4_f32(p.sub.as_ref(), sx as f32, sy as f32, sw as f32, sh as f32);
        // Red and blue swapped, as the textures hold them (see `program`).
        let [r, g, b, a] = blit.tint;
        gl.uniform_4_f32(p.tint.as_ref(), b, g, r, a);
        gl.uniform_1_f32(p.radius.as_ref(), radius as f32);
        gl.uniform_1_f32(p.filtered.as_ref(), if blit.linear { 1.0 } else { 0.0 });
        gl.uniform_1_f32(p.check_inside.as_ref(), if blit.check_inside { 1.0 } else { 0.0 });
        gl.uniform_1_f32(p.opaque.as_ref(), if blit.opaque { 1.0 } else { 0.0 });
        gl.uniform_1_f32(p.mode.as_ref(), mode_number(blit.blend));
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(src.texture));
        dev.bind_quad();
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
    }
}

/// Where a shape's coverage comes from (see `ShapeProgram`).
pub(crate) enum Cover<'a> {
    /// An axis-aligned rectangle, already cut to the scissor, in device
    /// pixels: `(x0, y0, x1, y1)`.
    Rect(f64, f64, f64, f64),
    /// The CPU's coverage, one byte a pixel, for the box at `origin` of
    /// `size` pixels.
    Mask {
        origin: (usize, usize),
        size: (usize, usize),
        bytes: &'a [u8],
    },
    /// Coverage kept in the shape atlas, at `corner` there, for the box at
    /// `origin` (see [`keep_shape`]).
    Kept {
        origin: (usize, usize),
        corner: (usize, usize),
    },
}

/// Where the shape atlas holds `key`'s coverage, if it does.
pub(crate) fn kept_shape(dev: &Device, key: GlyphKey) -> Option<(usize, usize)> {
    dev.shapes.borrow().get(key)
}

/// Keep a shape's coverage (`w` × `h` bytes) in the shape atlas under
/// `key`; where it went, or `None` for one too large to keep.
pub(crate) fn keep_shape(
    dev: &Device,
    key: GlyphKey,
    mask: &[u8],
    w: usize,
    h: usize,
) -> Option<(usize, usize)> {
    // Shapes waiting in the batch point into the shape atlas: if it has to
    // be made anew for this one, they are drawn first. Otherwise this one
    // goes in beside them, and they wait on.
    if !dev.shapes.borrow().has_room(w, h) {
        batch::flush(dev);
    }
    dev.shapes
        .borrow_mut()
        .place(&dev.gl, key, mask, w, h)
        .map(|(at, _)| at)
}

/// A shape filled onto a target, as `raster::fill_paths_with` fills it.
pub(crate) struct Fill {
    /// Straight RGBA, when there is no gradient.
    pub(crate) colour: [f32; 4],
    pub(crate) gradient: Option<Gradient>,
    pub(crate) antialias: bool,
    pub(crate) blend: BlendMode,
    /// The pixels to fill: the shape's box, within the scissor.
    pub(crate) area: Clip,
}

/// Whether a fill in `blend` can be drawn on the GPU: every mode but
/// replace, whose partial cover is a mix the blend stage has no factor for.
pub(crate) fn can_fill(blend: BlendMode) -> bool {
    blend != BlendMode::Replace
}

/// Fill `cover` onto `dst` with `fill`'s paint. A rectangle or a kept
/// shape in a flat colour joins the batch (see `batch`); the rest are drawn
/// at once.
pub(crate) fn fill(dst: &Texture, fill: &Fill, cover: Cover) {
    if fill.gradient.is_none() {
        let Clip { x0, y0, x1, y1 } = fill.area;
        let area = (x0 as f32, y0 as f32, x1 as f32, y1 as f32);
        let quad = match &cover {
            Cover::Rect(rx0, ry0, rx1, ry1) => Some(batch::Quad {
                area,
                rect: [*rx0 as f32, *ry0 as f32, *rx1 as f32, *ry1 as f32],
                mask: None,
                colour: fill.colour,
                antialias: fill.antialias,
            }),
            Cover::Kept { origin, corner } => Some(batch::Quad {
                area,
                rect: [0.0; 4],
                mask: Some([origin.0 as f32, origin.1 as f32, corner.0 as f32, corner.1 as f32]),
                colour: fill.colour,
                antialias: fill.antialias,
            }),
            Cover::Mask { .. } => None,
        };
        if let Some(quad) = quad {
            batch::add(dst, fill.blend, &quad);
            return;
        }
    }
    let dev = &dst.dev;
    let gl = &dev.gl;
    let p = &dev.shape;
    dst.bind_target();
    fill.area.apply(gl);
    blend_state(gl, fill.blend);
    // SAFETY: GL calls on the device's current context.
    unsafe {
        gl.use_program(Some(p.program));
        gl.uniform_2_f32(p.target.as_ref(), dst.w as f32, dst.h as f32);
        let Clip { x0, y0, x1, y1 } = fill.area;
        gl.uniform_4_f32(p.area.as_ref(), x0 as f32, y0 as f32, x1 as f32, y1 as f32);
        gl.uniform_1_f32(p.antialias.as_ref(), if fill.antialias { 1.0 } else { 0.0 });
        gl.uniform_1_f32(p.mode.as_ref(), mode_number(fill.blend));
        let swap = |c: [f32; 4]| [c[2], c[1], c[0], c[3]];
        let [r, g, b, a] = swap(fill.colour);
        gl.uniform_4_f32(p.colour.as_ref(), r, g, b, a);
        match &fill.gradient {
            None => gl.uniform_1_f32(p.gradient.as_ref(), 0.0),
            Some(gradient) => {
                let (kind, geometry) = match gradient.kind {
                    GradientKind::Linear { x0, y0, x1, y1 } => (1.0, [x0, y0, x1, y1]),
                    GradientKind::Radial { cx, cy, radius } => (2.0, [cx, cy, radius, 0.0]),
                };
                gl.uniform_1_f32(p.gradient.as_ref(), kind);
                let [a, b, c, d] = geometry.map(|v| v as f32);
                gl.uniform_4_f32(p.geometry.as_ref(), a, b, c, d);
                let stops = gradient.stops();
                let mut colours = [0.0f32; 32];
                let mut at = [0.0f32; 8];
                for (i, stop) in stops.iter().enumerate().take(8) {
                    colours[i * 4..i * 4 + 4].copy_from_slice(&swap(stop.color));
                    at[i] = stop.at;
                }
                gl.uniform_4_f32_slice(p.stop_colour.as_ref(), &colours);
                gl.uniform_1_f32_slice(p.stop_at.as_ref(), &at);
                gl.uniform_1_f32(p.stop_count.as_ref(), stops.len() as f32);
            }
        }
        match cover {
            Cover::Rect(rx0, ry0, rx1, ry1) => {
                gl.uniform_1_f32(p.from_mask.as_ref(), 0.0);
                gl.uniform_4_f32(p.rect.as_ref(), rx0 as f32, ry0 as f32, rx1 as f32, ry1 as f32);
            }
            Cover::Mask {
                origin,
                size,
                bytes,
            } => {
                let (cw, ch) = upload_mask(dev, size, bytes);
                gl.uniform_1_f32(p.from_mask.as_ref(), 1.0);
                gl.uniform_2_f32(p.mask_origin.as_ref(), origin.0 as f32, origin.1 as f32);
                gl.uniform_2_f32(p.mask_corner.as_ref(), 0.0, 0.0);
                gl.uniform_2_f32(p.mask_scale.as_ref(), 1.0 / cw as f32, 1.0 / ch as f32);
            }
            Cover::Kept { origin, corner } => {
                let shapes = dev.shapes.borrow();
                gl.active_texture(glow::TEXTURE1);
                gl.bind_texture(glow::TEXTURE_2D, Some(shapes.texture));
                let size = shapes.size as f32;
                gl.uniform_1_f32(p.from_mask.as_ref(), 1.0);
                gl.uniform_2_f32(p.mask_origin.as_ref(), origin.0 as f32, origin.1 as f32);
                gl.uniform_2_f32(p.mask_corner.as_ref(), corner.0 as f32, corner.1 as f32);
                gl.uniform_2_f32(p.mask_scale.as_ref(), 1.0 / size, 1.0 / size);
            }
        }
        dev.bind_quad();
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.active_texture(glow::TEXTURE0);
    }
}

/// Put a coverage mask in the device's mask texture, at its corner, and
/// leave it bound on unit 1. Returns the texture's size, which only grows
/// (to the next power of two), so a frame of shapes reuses one texture.
fn upload_mask(dev: &Device, (w, h): (usize, usize), bytes: &[u8]) -> (usize, usize) {
    let gl = &dev.gl;
    // SAFETY: GL calls on the device's current context; `bytes` holds
    // `w * h` coverage values (`raster::coverage_mask` sized it).
    unsafe {
        gl.active_texture(glow::TEXTURE1);
        let (texture, cw, ch) = match dev.mask.get() {
            Some((texture, cw, ch)) if cw >= w && ch >= h => (texture, cw, ch),
            old => {
                if let Some((texture, _, _)) = old {
                    gl.delete_texture(texture);
                }
                let (cw, ch) = (
                    w.next_power_of_two().max(old.map_or(0, |o| o.1)),
                    h.next_power_of_two().max(old.map_or(0, |o| o.2)),
                );
                let texture = gl.create_texture().expect("a texture is always available");
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                for (name, value) in [
                    (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
                    (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
                    (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                    (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
                ] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32);
                }
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::LUMINANCE as i32,
                    cw as i32,
                    ch as i32,
                    0,
                    glow::LUMINANCE,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                dev.mask.set(Some((texture, cw, ch)));
                (texture, cw, ch)
            }
        };
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        // Fresh storage for every mask ("orphaning"): the last shape's draw
        // may not have run yet, and writing into the storage it reads would
        // make the driver wait for it. Given new storage, it doesn't.
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::LUMINANCE as i32,
            cw as i32,
            ch as i32,
            0,
            glow::LUMINANCE,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        // Rows of single bytes aren't four-aligned.
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        gl.tex_sub_image_2d(
            glow::TEXTURE_2D,
            0,
            0,
            0,
            w as i32,
            h as i32,
            glow::LUMINANCE,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(&bytes[..w * h])),
        );
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
        (cw, ch)
    }
}

/// One glyph to draw: which it is, its coverage (`w` × `h` bytes), and where
/// its pixels go on the target.
pub(crate) struct GlyphDraw<'a> {
    pub(crate) key: GlyphKey,
    pub(crate) mask: &'a [u8],
    pub(crate) w: usize,
    pub(crate) h: usize,
    pub(crate) xform: Transform,
}

/// How a run of text is painted.
pub(crate) struct TextPaint {
    /// Straight RGBA.
    pub(crate) colour: [f32; 4],
    pub(crate) blend: BlendMode,
    pub(crate) linear: bool,
    pub(crate) clip: Clip,
}

/// Draw `glyphs` onto `dst`, as `raster::blit_mask` draws each: those at a
/// whole-pixel place (nearly all text) in one draw, any scaled or turned
/// one on its own, kept to the pixels the CPU would visit. `false`, having
/// drawn nothing, when a glyph is too large for the atlas.
pub(crate) fn draw_glyphs(dst: &Texture, paint: &TextPaint, glyphs: &[GlyphDraw]) -> bool {
    let dev = &dst.dev;
    let gl = &dev.gl;

    // Every glyph's place first: making room can empty the atlas, and then
    // the places taken before are gone, so the run is placed again.
    let mut places = Vec::with_capacity(glyphs.len());
    'placing: for _ in 0..2 {
        places.clear();
        let mut atlas = dev.atlas.borrow_mut();
        for g in glyphs {
            match atlas.place(gl, g.key, g.mask, g.w, g.h) {
                None => return false,
                Some((_, true)) if !places.is_empty() => continue 'placing,
                Some((at, _)) => places.push(at),
            }
        }
        break;
    }
    if places.len() != glyphs.len() {
        return false;
    }
    let atlas_size = dev.atlas.borrow().size as f32;
    let atlas_texture = dev.atlas.borrow().texture;

    let p = &dev.glyph;
    dst.bind_target();
    blend_state(gl, paint.blend);
    // SAFETY: GL calls on the device's current context.
    unsafe {
        gl.use_program(Some(p.program));
        gl.uniform_2_f32(p.target.as_ref(), dst.w as f32, dst.h as f32);
        gl.uniform_1_f32(p.atlas_scale.as_ref(), 1.0 / atlas_size);
        let [r, g, b, a] = paint.colour;
        gl.uniform_4_f32(p.colour.as_ref(), b, g, r, a);
        gl.uniform_1_f32(p.mode.as_ref(), mode_number(paint.blend));
        gl.uniform_1_f32(p.filtered.as_ref(), if paint.linear { 1.0 } else { 0.0 });
        gl.active_texture(glow::TEXTURE1);
        gl.bind_texture(glow::TEXTURE_2D, Some(atlas_texture));
        gl.active_texture(glow::TEXTURE0);
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(dev.stream));
        for (location, size, offset) in [(0, 2, 0), (1, 2, 8), (2, 4, 16)] {
            gl.enable_vertex_attrib_array(location);
            gl.vertex_attrib_pointer_f32(location, size, glow::FLOAT, false, 32, offset);
        }
    }

    // Whole-pixel glyphs: two triangles each, all in one draw.
    let mut exact: Vec<f32> = Vec::new();
    let mut turned = Vec::new();
    for (g, &(ax, ay)) in glyphs.iter().zip(&places) {
        let cell = [ax as f32, ay as f32, g.w as f32, g.h as f32];
        match crate::raster::axis_aligned_offset(&g.xform) {
            Some((ox, oy)) => {
                let (x0, y0) = (ox as f32, oy as f32);
                let (x1, y1) = (x0 + g.w as f32, y0 + g.h as f32);
                push_quad(
                    &mut exact,
                    [(x0, y0), (x1, y0), (x0, y1), (x1, y1)],
                    [(0.0, 0.0), (g.w as f32, 0.0), (0.0, g.h as f32), (g.w as f32, g.h as f32)],
                    cell,
                );
            }
            None => turned.push((g, cell)),
        }
    }
    if !exact.is_empty() {
        paint.clip.apply(gl);
        // SAFETY: as above.
        unsafe {
            gl.uniform_1_f32(p.exact.as_ref(), 1.0);
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                device::bytes_of(&exact),
                glow::STREAM_DRAW,
            );
            gl.draw_arrays(glow::TRIANGLES, 0, (exact.len() / 8) as i32);
        }
    }

    // A scaled or turned glyph: the pixels the CPU visits are those of its
    // box on the target, so that is the scissor, and the square drawn is
    // grown past the glyph by a pixel so none of them is missed.
    for (g, cell) in turned {
        let t = &g.xform;
        let bounds = crate::raster::transformed_bounds(t, g.w as f64, g.h as f64);
        let (bx0, by0, bx1, by1) = bounds.pixel_bounds(dst.w, dst.h);
        let clip = Clip {
            x0: bx0.max(paint.clip.x0),
            y0: by0.max(paint.clip.y0),
            x1: bx1.min(paint.clip.x1),
            y1: by1.min(paint.clip.y1),
        };
        if clip.x0 >= clip.x1 || clip.y0 >= clip.y1 {
            continue;
        }
        let scale = t.a.hypot(t.b).min(t.c.hypot(t.d));
        let pad = if scale > 1e-9 { (1.0 / scale).min(1e6) } else { 0.0 } + 0.5;
        let (lx0, ly0, lx1, ly1) = (-pad, -pad, g.w as f64 + pad, g.h as f64 + pad);
        let corner = |x: f64, y: f64| {
            let (dx, dy) = t.apply(x, y);
            (dx as f32, dy as f32)
        };
        let mut quad = Vec::with_capacity(48);
        push_quad(
            &mut quad,
            [corner(lx0, ly0), corner(lx1, ly0), corner(lx0, ly1), corner(lx1, ly1)],
            [
                (lx0 as f32, ly0 as f32),
                (lx1 as f32, ly0 as f32),
                (lx0 as f32, ly1 as f32),
                (lx1 as f32, ly1 as f32),
            ],
            cell,
        );
        clip.apply(gl);
        // SAFETY: as above.
        unsafe {
            gl.uniform_1_f32(p.exact.as_ref(), 0.0);
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, device::bytes_of(&quad), glow::STREAM_DRAW);
            gl.draw_arrays(glow::TRIANGLES, 0, 6);
        }
    }
    // SAFETY: as above. Attributes 1 and 2 are the glyph program's alone.
    unsafe {
        gl.disable_vertex_attrib_array(1);
        gl.disable_vertex_attrib_array(2);
    }
    true
}

/// Two triangles of a quad, corners in the order top-left, top-right,
/// bottom-left, bottom-right, each vertex its place, its glyph place and
/// the glyph's cell.
fn push_quad(out: &mut Vec<f32>, at: [(f32, f32); 4], local: [(f32, f32); 4], cell: [f32; 4]) {
    for i in [0, 1, 2, 2, 1, 3] {
        out.extend_from_slice(&[at[i].0, at[i].1, local[i].0, local[i].1]);
        out.extend_from_slice(&cell);
    }
}

/// Overwrite `clip` of `dst` with a straight RGBA colour: `Graphics.clear`.
pub(crate) fn clear(dst: &Texture, color: [f32; 4], clip: Clip) {
    let gl = &dst.dev.gl;
    dst.bind_target();
    clip.apply(gl);
    // SAFETY: GL calls on the current context.
    unsafe {
        gl.disable(glow::BLEND);
        let [r, g, b, a] = color.map(|c| c.clamp(0.0, 1.0));
        gl.clear_color(b, g, r, a);
        gl.clear(glow::COLOR_BUFFER_BIT);
    }
}

/// Whether the GPU may be used at all: not when `SHINE_GPU=0`.
pub(crate) fn wanted() -> bool {
    std::env::var_os("SHINE_GPU").is_none_or(|v| v != "0")
}

/// Whether to say which way frames are presented.
pub(crate) fn tracing() -> bool {
    std::env::var_os("SHINE_TRACE_PRESENT").is_some()
}
