//! Shaders a program brings: `Graphics.newShader`.
//!
//! Shine's own shaders draw what `Graphics` promises (an image, a shape,
//! text) and nothing else. An effect — a blur, a ripple, a colour wash —
//! is not Shine's to decide, so a program writes it: one GLSL function,
//!
//! ```glsl
//! vec4 effect(vec4 colour, sampler2D image, vec2 uv, vec2 pixel)
//! ```
//!
//! called for every pixel an image is drawn to while the shader is set
//! (`Graphics.setShader`). `colour` is the paint colour, `image` the image
//! being drawn, `uv` where in it this pixel reads (0 to 1 each way), and
//! `pixel` the same place in the image's own pixels. What it returns is
//! blended as the image's pixel would have been. `Texel(image, uv)` reads
//! the image; `texel` is the size of one of its pixels in `uv`, for
//! reading a neighbour; and the program's own `uniform`s are set with
//! `Shader.send`.
//!
//! A shader may have a part that runs once a corner instead of once a
//! pixel:
//!
//! ```glsl
//! varying vec2 beside;
//! #ifdef VERTEX
//! void vertex(vec2 uv, vec2 pixel) { beside = uv + vec2(texel.x, 0.0); }
//! #endif
//! #ifdef PIXEL
//! vec4 effect(vec4 colour, sampler2D image, vec2 uv, vec2 pixel) {
//!     return (Texel(image, uv) + Texel(image, beside)) * 0.5;
//! }
//! #endif
//! ```
//!
//! What `vertex` leaves in the shader's own `varying`s, `effect` finds
//! there, worked out between the corners. A source with a `vertex` is
//! compiled for both parts, `VERTEX` defined for the one and `PIXEL` for
//! the other.
//!
//! GLSL ES 1.00, the oldest there is, and what is written for the weakest
//! GPU runs on every one. Intel's chips up to the GMA 3150 take no loop
//! that isn't a fixed count, no branch that isn't a choice between two
//! values, 64 instructions, and four rounds of texture reads: a read at a
//! place worked out in `effect` starts a new round, a read at `uv` or at
//! a `varying` as it came does not. Which is what `vertex` is for: an
//! effect that reads a pixel's neighbours works out where they are there.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use glow::HasContext;

use super::device::Device;
use super::texture::Texture;
use super::{Clip, blend_state, program};
use crate::geom::Transform;
use crate::raster::BlendMode;

/// What goes before the program's source in each part: what it is given.
/// Textures hold red and blue the other way round (see `program`), which
/// `Texel` and the fragment part's last line hide. `#line` has the GPU's
/// complaints count lines as the program wrote them.
const VERTEX_BEFORE: &str = "
#define VERTEX
attribute vec2 corner;
uniform vec2 target;
uniform vec2 size;
uniform vec3 row0;
uniform vec3 row1;
uniform vec4 sub;
uniform vec2 shineTextureSize;
uniform vec2 texel;
varying vec2 shinePixel;
varying vec2 shineUv;
#line 1
";

/// The square becomes the region drawn (`size`), placed by the
/// transform's rows; where each pixel reads the image is worked out here,
/// between the corners, so the fragment part has it for nothing. `CALL`
/// is the program's `vertex`, where it has one.
const VERTEX_AFTER: &str = "
void main() {
    shinePixel = corner * size;
    shineUv = (sub.xy + shinePixel) / shineTextureSize;
    CALL
    vec2 device = vec2(dot(row0, vec3(shinePixel, 1.0)), dot(row1, vec3(shinePixel, 1.0)));
    gl_Position = vec4(device / target * 2.0 - 1.0, 0.0, 1.0);
}
";

const BEFORE: &str = "
#define PIXEL
uniform sampler2D shineImage;
uniform vec4 shineTint;
uniform vec2 texel;
varying vec2 shinePixel;
varying vec2 shineUv;
#define Texel(image, uv) texture2D(image, uv).bgra
#line 1
";

const AFTER: &str = "
void main() {
    shineColour = effect(shineTint, shineImage, shineUv, shinePixel).bgra;
}
";

pub(crate) struct UserShader {
    dev: Rc<Device>,
    program: glow::Program,
    target: Option<glow::UniformLocation>,
    size: Option<glow::UniformLocation>,
    row0: Option<glow::UniformLocation>,
    row1: Option<glow::UniformLocation>,
    sub: Option<glow::UniformLocation>,
    texture_size: Option<glow::UniformLocation>,
    tint: Option<glow::UniformLocation>,
    texel: Option<glow::UniformLocation>,
    /// Where the program's own uniforms are, found as they are first sent.
    named: RefCell<HashMap<String, Option<glow::UniformLocation>>>,
}

impl UserShader {
    /// Compile `effect`. The error is the GPU's own, with the program's
    /// line numbers: a mistake in the source, or more than this GPU runs.
    pub(crate) fn new(dev: &Rc<Device>, effect: &str) -> Result<UserShader, String> {
        let gl = &dev.gl;
        // A source with a part for the corners is compiled into both;
        // one without is the fragment part's alone.
        let vertex = if effect.contains("void vertex(") {
            let after = VERTEX_AFTER.replace("CALL", "vertex(shineUv, shinePixel);");
            format!("{VERTEX_BEFORE}{effect}{after}")
        } else {
            format!("{VERTEX_BEFORE}{}", VERTEX_AFTER.replace("CALL", ""))
        };
        let fragment = format!("{BEFORE}{effect}{AFTER}");
        // SAFETY: GL calls on the device's current context.
        unsafe {
            let program = program::link_variant(gl, dev.es(), "", &vertex, &fragment, &["corner"])?;
            gl.use_program(Some(program));
            gl.uniform_1_i32(program::uniform(gl, program, "shineImage").as_ref(), 0);
            let at = |name: &str| program::uniform(gl, program, name);
            Ok(UserShader {
                dev: Rc::clone(dev),
                target: at("target"),
                size: at("size"),
                row0: at("row0"),
                row1: at("row1"),
                sub: at("sub"),
                texture_size: at("shineTextureSize"),
                tint: at("shineTint"),
                texel: at("texel"),
                named: RefCell::new(HashMap::new()),
                program,
            })
        }
    }
}

impl Drop for UserShader {
    fn drop(&mut self) {
        // SAFETY: made on this context, which the device keeps current.
        unsafe { self.dev.gl.delete_program(self.program) };
    }
}

/// One image region drawn through a shader.
pub(crate) struct ShaderDraw {
    /// The region of the source: its corner and its size, in pixels.
    pub(crate) sub: (f64, f64, f64, f64),
    /// Region space to the target's pixels.
    pub(crate) xform: Transform,
    /// The paint colour, straight RGBA: the effect's `colour`.
    pub(crate) tint: [f32; 4],
    pub(crate) clip: Clip,
    pub(crate) blend: BlendMode,
}

/// Draw `draw` of `src` onto `dst` through `shader`, with `values` for its
/// uniforms (a name and one to four numbers each).
pub(crate) fn draw(
    dst: &Texture,
    src: &Texture,
    shader: &UserShader,
    values: &[(String, Vec<f32>)],
    draw: &ShaderDraw,
) {
    let dev = &dst.dev;
    let gl = &dev.gl;
    let (sx, sy, sw, sh) = draw.sub;
    let t = &draw.xform;
    dst.bind_target();
    draw.clip.apply(gl);
    blend_state(gl, draw.blend);
    // SAFETY: GL calls on the device's current context.
    unsafe {
        gl.use_program(Some(shader.program));
        gl.uniform_2_f32(shader.target.as_ref(), dst.w as f32, dst.h as f32);
        gl.uniform_2_f32(shader.size.as_ref(), sw as f32, sh as f32);
        gl.uniform_3_f32(shader.row0.as_ref(), t.a as f32, t.c as f32, t.tx as f32);
        gl.uniform_3_f32(shader.row1.as_ref(), t.b as f32, t.d as f32, t.ty as f32);
        gl.uniform_4_f32(shader.sub.as_ref(), sx as f32, sy as f32, sw as f32, sh as f32);
        gl.uniform_2_f32(shader.texture_size.as_ref(), src.w as f32, src.h as f32);
        let [r, g, b, a] = draw.tint;
        gl.uniform_4_f32(shader.tint.as_ref(), r, g, b, a);
        gl.uniform_2_f32(shader.texel.as_ref(), 1.0 / src.w as f32, 1.0 / src.h as f32);
        let mut named = shader.named.borrow_mut();
        for (name, value) in values {
            let at = named
                .entry(name.clone())
                .or_insert_with(|| program::uniform(gl, shader.program, name));
            match value.as_slice() {
                [x] => gl.uniform_1_f32(at.as_ref(), *x),
                [x, y] => gl.uniform_2_f32(at.as_ref(), *x, *y),
                [x, y, z] => gl.uniform_3_f32(at.as_ref(), *x, *y, *z),
                [x, y, z, w] => gl.uniform_4_f32(at.as_ref(), *x, *y, *z, *w),
                _ => {}
            }
        }
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(src.texture));
        dev.bind_quad();
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
    }
}
