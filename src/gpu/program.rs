//! The shader programs, written once for OpenGL ES 2.0, desktop OpenGL 2.1
//! and a core profile (macOS's): the same GLSL with each dialect's first
//! lines (see [`header`]). They write their colour to `shineColour`, which
//! each dialect's first lines say where to put.
//!
//! ## Colours on the GPU
//!
//! Shine's pixels are `0xAARRGGBB` words, which in memory are the bytes B,
//! G, R, A. Textures keep those bytes exactly as they are (uploaded and read
//! back as RGBA, which ES 2.0 always has, where BGRA it may not), so on the
//! GPU red and blue trade places: the "r" channel holds blue. Blending
//! treats the three colour channels alike and alpha is where it belongs, so
//! drawing needs no swizzle at all; colours given to a shader (a tint, a
//! clear colour) are passed with red and blue swapped, and only putting a
//! frame on the window swaps them back.

use glow::HasContext;

/// The first lines of every shader, for the context's dialect. A fragment
/// shader asks for high precision where ES has it: positions up to 16384
/// pixels need more than ES's medium precision.
///
/// A core profile (`core`, see `gpu::core_profile`) takes GLSL 1.50 and not
/// 1.20, where what 1.20 called `attribute` and `varying` are a stage's
/// `in` and `out`, `texture2D` is `texture`, and a fragment shader's colour
/// goes to an output it declares. The words of 1.20 are made to mean those,
/// so one source serves every dialect, programs' own shaders included.
fn header(es: bool, core: bool, fragment: bool) -> &'static str {
    match (es, core, fragment) {
        (true, _, true) => {
            "#version 100\n\
             #ifdef GL_FRAGMENT_PRECISION_HIGH\nprecision highp float;\n\
             #else\nprecision mediump float;\n#endif\n\
             #define shineColour gl_FragColor\n"
        }
        (true, _, false) => "#version 100\nprecision highp float;\n",
        (false, true, true) => {
            "#version 150\n\
             #define varying in\n\
             #define texture2D texture\n\
             out vec4 shineColour;\n"
        }
        (false, true, false) => {
            "#version 150\n\
             #define attribute in\n\
             #define varying out\n\
             #define texture2D texture\n"
        }
        (false, false, true) => "#version 120\n#define shineColour gl_FragColor\n",
        (false, false, false) => "#version 120\n",
    }
}

/// Compile and link a program from its two shaders, with the unit square at
/// attribute 0 (`corner`).
unsafe fn link(gl: &glow::Context, es: bool, vertex: &str, fragment: &str) -> Result<glow::Program, String> {
    // SAFETY: the caller's context is current.
    unsafe { link_with(gl, es, vertex, fragment, &["corner"]) }
}

/// [`link`] with the vertex attributes named, numbered from 0 in order.
unsafe fn link_with(
    gl: &glow::Context,
    es: bool,
    vertex: &str,
    fragment: &str,
    attributes: &[&str],
) -> Result<glow::Program, String> {
    // SAFETY: the caller's context is current.
    unsafe { link_variant(gl, es, "", vertex, fragment, attributes) }
}

/// A program whose fragment shader is one variant of `fragment`: the
/// `#define` lines in `defines` choose, as it is compiled, what the
/// shader would otherwise have to choose for every pixel.
///
/// That is the difference between a shader that runs everywhere and one
/// that does not. Intel's chips up to the GMA 3150 have no branches, 64
/// instructions and four rounds of texture reads to a shader: one that
/// carries every blend mode and every kind of sampling, picked by
/// uniforms, is several times that, and the same shader cut down to the
/// one mode and the one kind a draw needs fits with room to spare.
pub(super) unsafe fn link_variant(
    gl: &glow::Context,
    es: bool,
    defines: &str,
    vertex: &str,
    fragment: &str,
    attributes: &[&str],
) -> Result<glow::Program, String> {
    // SAFETY: the caller's context is current.
    unsafe {
        let program = gl.create_program().map_err(|e| format!("GL program: {e}"))?;
        let core = super::core_profile(gl);
        let mut shaders = Vec::new();
        for (kind, fragment_stage, source) in [
            (glow::VERTEX_SHADER, false, vertex),
            (glow::FRAGMENT_SHADER, true, fragment),
        ] {
            let shader = gl.create_shader(kind).map_err(|e| format!("GL shader: {e}"))?;
            let chosen = if fragment_stage { defines } else { "" };
            gl.shader_source(shader, &format!("{}{chosen}{source}", header(es, core, fragment_stage)));
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                return Err(format!(
                    "GL shader does not compile: {}",
                    gl.get_shader_info_log(shader)
                ));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        for (i, name) in attributes.iter().enumerate() {
            gl.bind_attrib_location(program, i as u32, name);
        }
        gl.link_program(program);
        for shader in shaders {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }
        if !gl.get_program_link_status(program) {
            return Err(format!(
                "GL program does not link: {}",
                gl.get_program_info_log(program)
            ));
        }
        Ok(program)
    }
}

/// The number a shader variant is compiled with for a blend mode: what it
/// has to hand the blend stage (see `gpu::blend_state`). Alpha, add and
/// subtract are given the same thing, the colour and how much of it.
pub(crate) fn shader_mode(blend: crate::raster::BlendMode) -> u8 {
    use crate::raster::BlendMode;
    match blend {
        BlendMode::Alpha | BlendMode::Add | BlendMode::Subtract => 0,
        BlendMode::Multiply => 3,
        BlendMode::Screen => 4,
        BlendMode::Replace => 5,
        BlendMode::Premultiplied => 6,
    }
}

/// The variants of one shader made so far, each when a draw first needs
/// it: a program for a key, or nothing for one the GPU turned down, which
/// is then drawn on the CPU and not tried again.
pub(crate) struct Variants<K, P> {
    made: std::cell::RefCell<std::collections::HashMap<K, Option<std::rc::Rc<P>>>>,
}

impl<K: Copy + Eq + std::hash::Hash + std::fmt::Debug, P> Variants<K, P> {
    pub(crate) fn new() -> Self {
        Variants { made: Default::default() }
    }

    /// The program for `key`, made with `make` if this is the first time.
    pub(crate) fn get(
        &self,
        key: K,
        make: impl FnOnce() -> Result<P, String>,
    ) -> Option<std::rc::Rc<P>> {
        if let Some(known) = self.made.borrow().get(&key) {
            return known.clone();
        }
        let made = match make() {
            Ok(program) => Some(std::rc::Rc::new(program)),
            Err(why) => {
                eprintln!("shine: the GPU cannot run shader {key:?} ({}); drawn on the CPU", why.trim());
                None
            }
        };
        self.made.borrow_mut().insert(key, made.clone());
        made
    }

    /// Every program made.
    pub(crate) fn programs(&self) -> Vec<std::rc::Rc<P>> {
        self.made.borrow().values().flatten().cloned().collect()
    }
}

pub(super) unsafe fn uniform(
    gl: &glow::Context,
    program: glow::Program,
    name: &str,
) -> Option<glow::UniformLocation> {
    // SAFETY: the caller's context is current.
    unsafe { gl.get_uniform_location(program, name) }
}

/// A texture over the whole window, red and blue swapped back: how a frame
/// is shown.
pub(crate) struct PresentProgram {
    pub(crate) program: glow::Program,
}

impl PresentProgram {
    pub(crate) unsafe fn new(gl: &glow::Context, es: bool) -> Result<PresentProgram, String> {
        let vertex = "
            attribute vec2 corner;
            varying vec2 uv;
            void main() {
                // The frame's first row is the window's top.
                uv = corner;
                gl_Position = vec4(corner.x * 2.0 - 1.0, 1.0 - corner.y * 2.0, 0.0, 1.0);
            }";
        let fragment = "
            uniform sampler2D frame;
            varying vec2 uv;
            void main() {
                shineColour = vec4(texture2D(frame, uv).bgr, 1.0);
            }";
        // SAFETY: the caller's context is current.
        unsafe {
            let program = link(gl, es, vertex, fragment)?;
            gl.use_program(Some(program));
            gl.uniform_1_i32(uniform(gl, program, "frame").as_ref(), 0);
            Ok(PresentProgram { program })
        }
    }
}

/// One image (or a region of one) drawn through an affine transform onto a
/// render target, as the CPU's `raster::blit_surface_sub` draws it: the
/// same pixels inside (decided by the scissor when the image is square to
/// the target, by the shader's own test when it is turned), the same hard
/// edges, rounded corners with the same
/// coverage, bilinear taps kept inside the region, tinted by the paint
/// colour. Blended by the fixed-function stage (see `gpu::Gpu::blit`).
pub(crate) struct BlitProgram {
    pub(crate) program: glow::Program,
    pub(crate) target: Option<glow::UniformLocation>,
    pub(crate) size: Option<glow::UniformLocation>,
    pub(crate) pad: Option<glow::UniformLocation>,
    pub(crate) row0: Option<glow::UniformLocation>,
    pub(crate) row1: Option<glow::UniformLocation>,
    pub(crate) texture_size: Option<glow::UniformLocation>,
    pub(crate) sub: Option<glow::UniformLocation>,
    pub(crate) tint: Option<glow::UniformLocation>,
    pub(crate) radius: Option<glow::UniformLocation>,
}

/// Which blit shader a draw needs (see [`link_variant`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BlitKey {
    /// What the blend stage is given (see [`shader_mode`]).
    pub(crate) mode: u8,
    /// The shader decides which pixels are inside the region: a turned
    /// image, whose pixels no scissor holds.
    pub(crate) check_inside: bool,
    pub(crate) rounded: bool,
    /// The nearest pixel of the image, not a mix of the four round it.
    pub(crate) nearest: bool,
    /// The image's alpha bytes mean nothing: read as opaque.
    pub(crate) opaque: bool,
}

impl BlitProgram {
    pub(crate) unsafe fn new(gl: &glow::Context, es: bool, key: BlitKey) -> Result<BlitProgram, String> {
        // The square becomes the region (`size`), grown by `pad` all round,
        // the transform's rows place it on the target, and the target's
        // pixels map onto clip space without a flip: row 0 of a target
        // texture is row 0 of the surface it mirrors.
        //
        // The pad is a pixel's width or more: the fragment shader, not the
        // rasterizer, decides which pixels are inside, by the same test
        // the CPU makes. The rasterizer's own rule for a pixel whose centre
        // is on an edge would otherwise disagree with it now and then.
        let vertex = "
            attribute vec2 corner;
            uniform vec2 target;
            uniform vec2 size;
            uniform float pad;
            uniform vec3 row0;
            uniform vec3 row1;
            varying vec2 local;
            void main() {
                local = corner * (size + 2.0 * pad) - pad;
                vec2 device = vec2(dot(row0, vec3(local, 1.0)), dot(row1, vec3(local, 1.0)));
                gl_Position = vec4(device / target * 2.0 - 1.0, 0.0, 1.0);
            }";
        let fragment = "
            uniform sampler2D image;
            uniform vec2 shineTextureSize;
            uniform vec4 sub;
            uniform vec4 tint;
            uniform float radius;
            varying vec2 local;

            void main() {
                vec2 p = local;
                // Whether the pixel is drawn at all: nothing, where any of these is.
                float keep = 1.0;
            #ifdef CHECK_INSIDE
                vec2 from = step(0.0, p);
                vec2 before = 1.0 - step(sub.zw, p);
                keep = from.x * from.y * before.x * before.y;
            #endif
                float cover = 1.0;
            #ifdef ROUNDED
                // The middle of the corner's curve, where the pixel is in a corner:
                // off the straight part of both edges at once.
                vec2 off = p - clamp(p, vec2(radius), sub.zw - radius);
                float cornered = sign(abs(off.x)) * sign(abs(off.y));
                cover = mix(1.0, clamp(radius - length(off) + 0.5, 0.0, 1.0), cornered);
            #endif
                // Half a pixel inside the region, or its middle where it is that thin.
                vec2 middle = sub.zw * 0.5;
                vec2 at = sub.xy + clamp(p, min(vec2(0.5), middle), max(sub.zw - 0.5, middle));
            #ifdef NEAREST
                at = floor(at) + 0.5;
            #endif
                vec4 sampled = texture2D(image, at / shineTextureSize);
            #ifdef OPAQUE
                sampled.a = 1.0;
            #endif
                // A see-through pixel leaves the target as it is, in every mode, as
                // on the CPU. One test for all of it: this chip's oldest relatives
                // count each `discard` as they count a texture read.
                if (keep * cover * sampled.a <= 0.0) {
                    discard;
                }
                vec4 colour = sampled * tint;
                float sa = colour.a * cover;
            #if MODE == 0
                // alpha, add, subtract: the colour, weighed by `sa`.
                shineColour = vec4(colour.rgb, sa);
            #elif MODE == 3
                // multiply: the factor the target is multiplied by.
                shineColour = vec4(colour.rgb * sa + 1.0 - sa, colour.a * sa + 1.0 - sa);
            #elif MODE == 4
                // screen: the colour already weighed.
                shineColour = vec4(colour.rgb * sa, sa);
            #elif MODE == 5
                // replace: as it is (only drawn with no partial cover).
                shineColour = colour;
            #else
                // premultiplied: the image's colours are weighed already; the tint's
                // alpha and the cover weigh them further.
                shineColour = vec4(sampled.rgb * tint.rgb * tint.a * cover, sa);
            #endif
            }";
        let mut defines = format!("#define MODE {}\n", key.mode);
        for (on, name) in [
            (key.check_inside, "CHECK_INSIDE"),
            (key.rounded, "ROUNDED"),
            (key.nearest, "NEAREST"),
            (key.opaque, "OPAQUE"),
        ] {
            if on {
                defines.push_str(&format!("#define {name}\n"));
            }
        }
        // SAFETY: the caller's context is current.
        unsafe {
            let program = link_variant(gl, es, &defines, vertex, fragment, &[])?;
            gl.use_program(Some(program));
            gl.uniform_1_i32(uniform(gl, program, "image").as_ref(), 0);
            Ok(BlitProgram {
                target: uniform(gl, program, "target"),
                size: uniform(gl, program, "size"),
                pad: uniform(gl, program, "pad"),
                row0: uniform(gl, program, "row0"),
                row1: uniform(gl, program, "row1"),
                texture_size: uniform(gl, program, "shineTextureSize"),
                sub: uniform(gl, program, "sub"),
                tint: uniform(gl, program, "tint"),
                radius: uniform(gl, program, "radius"),
                program,
            })
        }
    }
}

/// A shape filled with a colour or a gradient, as the CPU's
/// `raster::fill_paths_with` fills it. The CPU's coverage comes either from
/// an axis-aligned rectangle, worked out here exactly as
/// `fill_axis_aligned_rect` works it out (four sub-rows down, exact overlap
/// across), or from a mask of the CPU's own scanline coverage
/// (`raster::coverage_mask`) for every other shape. The paint's
/// antialiasing, colour or gradient and blend mode are applied here.
pub(crate) struct ShapeProgram {
    pub(crate) program: glow::Program,
    pub(crate) target: Option<glow::UniformLocation>,
    pub(crate) area: Option<glow::UniformLocation>,
    pub(crate) rect: Option<glow::UniformLocation>,
    pub(crate) mask_origin: Option<glow::UniformLocation>,
    pub(crate) mask_corner: Option<glow::UniformLocation>,
    pub(crate) mask_scale: Option<glow::UniformLocation>,
    pub(crate) colour: Option<glow::UniformLocation>,
    pub(crate) ramp: Option<glow::UniformLocation>,
    pub(crate) stop_colour: Option<glow::UniformLocation>,
    pub(crate) stop_at: Option<glow::UniformLocation>,
    pub(crate) stop_scale: Option<glow::UniformLocation>,
}

/// Which shape shader a fill needs (see [`link_variant`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ShapeKey {
    /// What the blend stage is given (see [`shader_mode`]).
    pub(crate) mode: u8,
    /// Coverage from a mask of the CPU's, not from a rectangle.
    pub(crate) from_mask: bool,
    /// No antialiasing: a pixel is in or out.
    pub(crate) hard: bool,
    /// 0 a flat colour, 1 a linear gradient, 2 a radial one.
    pub(crate) gradient: u8,
}

impl ShapeProgram {
    pub(crate) unsafe fn new(gl: &glow::Context, es: bool, key: ShapeKey) -> Result<ShapeProgram, String> {
        // The square becomes the box of pixels to fill (`area`).
        let vertex = "
            attribute vec2 corner;
            uniform vec2 target;
            uniform vec4 area;
            void main() {
                vec2 device = mix(area.xy, area.zw, corner);
                gl_Position = vec4(device / target * 2.0 - 1.0, 0.0, 1.0);
            }";
        let fragment = "
            uniform vec4 rect;
            uniform sampler2D mask;
            uniform vec2 maskOrigin;
            uniform vec2 maskCorner;
            uniform vec2 maskScale;
            uniform vec4 colour;

            float coverage(vec2 pixel) {
            #ifdef FROM_MASK
                return texture2D(mask, (maskCorner + pixel - maskOrigin + 0.5) * maskScale).r;
            #else
                float inside = 0.0;
                for (int s = 0; s < 4; s++) {
                    float sy = pixel.y + (float(s) + 0.5) / 4.0;
                    if (sy >= rect.y && sy < rect.w) {
                        inside += 1.0;
                    }
                }
                float across = clamp(min(pixel.x + 1.0, rect.z) - max(pixel.x, rect.x), 0.0, 1.0);
                return inside / 4.0 * across;
            #endif
            }

            #if GRADIENT != 0
            // Where along the gradient a pixel is: `dot(at, ramp.xy) + ramp.z` for a
            // linear one, `length(at - ramp.xy) * ramp.z + ramp.w` for a radial one,
            // the division done once, by whoever sets it.
            uniform vec4 ramp;
            uniform vec4 stopColour[8];
            uniform float stopAt[8];
            // One over the distance from the stop before, 0 for a stop there is not.
            uniform float stopScale[8];

            // The CPU's `Gradient::sample`, at the pixel's centre: from the first
            // stop's colour, toward each next one by how far past the stop before it
            // the pixel is. No branches: a chip that has none runs this too.
            vec4 paint(vec2 at) {
            #if GRADIENT == 1
                float t = dot(at, ramp.xy) + ramp.z;
            #else
                float t = length(at - ramp.xy) * ramp.z + ramp.w;
            #endif
                t = clamp(t, 0.0, 1.0);
                vec4 shade = stopColour[0];
                for (int i = 1; i < 8; i++) {
                    float along = clamp((t - stopAt[i - 1]) * stopScale[i], 0.0, 1.0);
                    shade += (stopColour[i] - shade) * along;
                }
                return shade;
            }
            #endif

            void main() {
                vec2 pixel = floor(gl_FragCoord.xy);
                float c = coverage(pixel);
            #ifdef HARD
                c = c >= 0.5 ? 1.0 : 0.0;
            #endif
                if (c <= 0.0) {
                    discard;
                }
            #if GRADIENT != 0
                vec4 source = paint(pixel + 0.5);
            #else
                vec4 source = colour;
            #endif
                float sa = source.a * c;
            #if MODE == 0
                shineColour = vec4(source.rgb, sa);
            #elif MODE == 3
                shineColour = vec4(source.rgb * sa + 1.0 - sa, source.a * sa + 1.0 - sa);
            #else
                shineColour = vec4(source.rgb * sa, sa);
            #endif
            }";
        let mut defines = format!("#define MODE {}\n#define GRADIENT {}\n", key.mode, key.gradient);
        for (on, name) in [(key.from_mask, "FROM_MASK"), (key.hard, "HARD")] {
            if on {
                defines.push_str(&format!("#define {name}\n"));
            }
        }
        // SAFETY: the caller's context is current.
        unsafe {
            let program = link_variant(gl, es, &defines, vertex, fragment, &[])?;
            gl.use_program(Some(program));
            gl.uniform_1_i32(uniform(gl, program, "mask").as_ref(), 1);
            Ok(ShapeProgram {
                target: uniform(gl, program, "target"),
                area: uniform(gl, program, "area"),
                rect: uniform(gl, program, "rect"),
                mask_origin: uniform(gl, program, "maskOrigin"),
                mask_corner: uniform(gl, program, "maskCorner"),
                mask_scale: uniform(gl, program, "maskScale"),
                colour: uniform(gl, program, "colour"),
                ramp: uniform(gl, program, "ramp"),
                stop_colour: uniform(gl, program, "stopColour"),
                stop_at: uniform(gl, program, "stopAt"),
                stop_scale: uniform(gl, program, "stopScale"),
                program,
            })
        }
    }
}

/// Glyphs from the atlas (see `gpu::atlas`), coloured and blended as the
/// CPU's `raster::blit_mask` draws them: a glyph at a whole-pixel place is
/// its coverage bytes exactly, one texel a pixel; a scaled or turned one is
/// sampled, bilinear or nearest, with nothing outside its edges. Each
/// vertex carries its place on the target, its place in the glyph, and
/// where the glyph is in the atlas with its size.
pub(crate) struct GlyphProgram {
    pub(crate) program: glow::Program,
    pub(crate) target: Option<glow::UniformLocation>,
    pub(crate) atlas_scale: Option<glow::UniformLocation>,
    pub(crate) colour: Option<glow::UniformLocation>,
}

/// How a glyph's coverage is read from the atlas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum GlyphSampling {
    /// At a whole-pixel place: a texel a pixel, exactly.
    Exact,
    /// Scaled or turned, mixed from the texels round each pixel.
    Filtered,
    /// Scaled or turned, the nearest texel.
    Nearest,
}

/// Which glyph shader a run of text needs (see [`link_variant`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TextKey {
    /// What the blend stage is given (see [`shader_mode`]).
    pub(crate) mode: u8,
    pub(crate) sampling: GlyphSampling,
}

impl GlyphProgram {
    pub(crate) unsafe fn new(gl: &glow::Context, es: bool, key: TextKey) -> Result<GlyphProgram, String> {
        let vertex = "
            attribute vec2 position;
            attribute vec2 place;
            attribute vec4 cell;
            uniform vec2 target;
            varying vec2 local;
            varying vec4 glyph;
            void main() {
                local = place;
                glyph = cell;
                gl_Position = vec4(position / target * 2.0 - 1.0, 0.0, 1.0);
            }";
        let fragment = "
            uniform sampler2D atlas;
            uniform float atlasScale;
            uniform vec4 colour;
            varying vec2 local;
            varying vec4 glyph;

            void main() {
            #ifdef EXACT
                float keep = 1.0;
                float a = texture2D(atlas, (glyph.xy + floor(local) + 0.5) * atlasScale).r;
            #else
            #ifdef FILTERED
                // Past half a texel outside, every tap the CPU takes is outside the
                // glyph; nearer, the atlas's empty border gives the zeroes it reads.
                vec2 from = step(-0.5, local);
                vec2 upTo = step(local, glyph.zw + 0.5);
                float keep = from.x * from.y * upTo.x * upTo.y;
                float a = texture2D(atlas, (glyph.xy + local) * atlasScale).r;
            #else
                vec2 p = floor(local);
                vec2 from = step(0.0, p);
                vec2 before = 1.0 - step(glyph.zw, p);
                float keep = from.x * from.y * before.x * before.y;
                float a = texture2D(atlas, (glyph.xy + p + 0.5) * atlasScale).r;
            #endif
            #endif
                if (keep * a <= 0.0) {
                    discard;
                }
                float sa = colour.a * a;
            #if MODE == 0
                shineColour = vec4(colour.rgb, sa);
            #elif MODE == 3
                shineColour = vec4(colour.rgb * sa + 1.0 - sa, colour.a * sa + 1.0 - sa);
            #else
                shineColour = vec4(colour.rgb * sa, sa);
            #endif
            }";
        let sampling = match key.sampling {
            GlyphSampling::Exact => "#define EXACT\n",
            GlyphSampling::Filtered => "#define FILTERED\n",
            GlyphSampling::Nearest => "",
        };
        let defines = format!("#define MODE {}\n{sampling}", key.mode);
        // SAFETY: the caller's context is current.
        unsafe {
            let program =
                link_variant(gl, es, &defines, vertex, fragment, &["position", "place", "cell"])?;
            gl.use_program(Some(program));
            gl.uniform_1_i32(uniform(gl, program, "atlas").as_ref(), 1);
            Ok(GlyphProgram {
                target: uniform(gl, program, "target"),
                atlas_scale: uniform(gl, program, "atlasScale"),
                colour: uniform(gl, program, "colour"),
                program,
            })
        }
    }
}

/// Shapes in a batch (see `gpu::batch`): the same coverage, colour and blend
/// as [`ShapeProgram`] without a gradient, but everything a shape needs
/// carried by its vertices instead of uniforms, so a run of rectangles and
/// kept shapes is one draw.
pub(crate) struct FillBatchProgram {
    pub(crate) program: glow::Program,
    pub(crate) target: Option<glow::UniformLocation>,
    pub(crate) mask_scale: Option<glow::UniformLocation>,
    pub(crate) mode: Option<glow::UniformLocation>,
}

impl FillBatchProgram {
    pub(crate) unsafe fn new(gl: &glow::Context, es: bool) -> Result<FillBatchProgram, String> {
        let vertex = "
            attribute vec2 position;
            attribute vec4 rectIn;
            attribute vec4 colourIn;
            attribute vec4 maskIn;
            attribute vec2 flagsIn;
            uniform vec2 target;
            varying vec4 rect;
            varying vec4 colour;
            varying vec4 mask;
            varying vec2 flags;
            void main() {
                rect = rectIn;
                colour = colourIn;
                mask = maskIn;
                flags = flagsIn;
                gl_Position = vec4(position / target * 2.0 - 1.0, 0.0, 1.0);
            }";
        let fragment = "
            uniform sampler2D shapes;
            uniform float maskScale;
            uniform float mode;
            varying vec4 rect;
            varying vec4 colour;
            varying vec4 mask;
            varying vec2 flags;

            void main() {
                vec2 pixel = floor(gl_FragCoord.xy);
                float c;
                if (flags.x > 0.5) {
                    c = texture2D(shapes, (mask.zw + pixel - mask.xy + 0.5) * maskScale).r;
                } else {
                    float inside = 0.0;
                    for (int s = 0; s < 4; s++) {
                        float sy = pixel.y + (float(s) + 0.5) / 4.0;
                        if (sy >= rect.y && sy < rect.w) {
                            inside += 1.0;
                        }
                    }
                    float across = clamp(min(pixel.x + 1.0, rect.z) - max(pixel.x, rect.x), 0.0, 1.0);
                    c = inside / 4.0 * across;
                }
                if (flags.y < 0.5) {
                    c = c >= 0.5 ? 1.0 : 0.0;
                }
                if (c <= 0.0) {
                    discard;
                }
                float sa = colour.a * c;
                if (mode < 0.5 || (mode > 0.5 && mode < 2.5)) {
                    shineColour = vec4(colour.rgb, sa);
                } else if (mode < 3.5) {
                    shineColour = vec4(colour.rgb * sa + 1.0 - sa, colour.a * sa + 1.0 - sa);
                } else {
                    shineColour = vec4(colour.rgb * sa, sa);
                }
            }";
        // SAFETY: the caller's context is current.
        unsafe {
            let program = link_with(
                gl,
                es,
                vertex,
                fragment,
                &["position", "rectIn", "colourIn", "maskIn", "flagsIn"],
            )?;
            gl.use_program(Some(program));
            gl.uniform_1_i32(uniform(gl, program, "shapes").as_ref(), 1);
            Ok(FillBatchProgram {
                target: uniform(gl, program, "target"),
                mask_scale: uniform(gl, program, "maskScale"),
                mode: uniform(gl, program, "mode"),
                program,
            })
        }
    }
}
