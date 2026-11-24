//! The shader programs, written once for OpenGL ES 2.0 and desktop OpenGL
//! 2.1: the same GLSL with each dialect's first lines.
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
fn header(es: bool, fragment: bool) -> &'static str {
    match (es, fragment) {
        (true, true) => {
            "#version 100\n\
             #ifdef GL_FRAGMENT_PRECISION_HIGH\nprecision highp float;\n\
             #else\nprecision mediump float;\n#endif\n"
        }
        (true, false) => "#version 100\nprecision highp float;\n",
        (false, _) => "#version 120\n",
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
    unsafe {
        let program = gl.create_program().map_err(|e| format!("GL program: {e}"))?;
        let mut shaders = Vec::new();
        for (kind, fragment_stage, source) in [
            (glow::VERTEX_SHADER, false, vertex),
            (glow::FRAGMENT_SHADER, true, fragment),
        ] {
            let shader = gl.create_shader(kind).map_err(|e| format!("GL shader: {e}"))?;
            gl.shader_source(shader, &format!("{}{source}", header(es, fragment_stage)));
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

unsafe fn uniform(gl: &glow::Context, program: glow::Program, name: &str) -> Option<glow::UniformLocation> {
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
                gl_FragColor = vec4(texture2D(frame, uv).bgr, 1.0);
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
    pub(crate) filtered: Option<glow::UniformLocation>,
    pub(crate) check_inside: Option<glow::UniformLocation>,
    pub(crate) opaque: Option<glow::UniformLocation>,
    pub(crate) mode: Option<glow::UniformLocation>,
}

impl BlitProgram {
    pub(crate) unsafe fn new(gl: &glow::Context, es: bool) -> Result<BlitProgram, String> {
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
            uniform vec2 textureSize;
            uniform vec4 sub;
            uniform vec4 tint;
            uniform float radius;
            uniform float filtered;
            uniform float checkInside;
            uniform float opaque;
            uniform float mode;
            varying vec2 local;

            float inside(float value, float extent) {
                if (extent <= 1.0) {
                    return extent * 0.5;
                }
                return clamp(value, 0.5, extent - 0.5);
            }

            float corner(vec2 p, vec2 extent, float r) {
                float cx;
                if (p.x < r) {
                    cx = r;
                } else if (p.x > extent.x - r) {
                    cx = extent.x - r;
                } else {
                    return 1.0;
                }
                float cy;
                if (p.y < r) {
                    cy = r;
                } else if (p.y > extent.y - r) {
                    cy = extent.y - r;
                } else {
                    return 1.0;
                }
                return clamp(r - distance(p, vec2(cx, cy)) + 0.5, 0.0, 1.0);
            }

            void main() {
                vec2 p = local;
                if (checkInside > 0.5
                    && (p.x < 0.0 || p.y < 0.0 || p.x >= sub.z || p.y >= sub.w)) {
                    discard;
                }
                float cover = 1.0;
                if (radius > 0.0) {
                    cover = corner(p, sub.zw, radius);
                    if (cover <= 0.0) {
                        discard;
                    }
                }
                vec2 at = sub.xy + vec2(inside(p.x, sub.z), inside(p.y, sub.w));
                if (filtered < 0.5) {
                    at = floor(at) + 0.5;
                }
                vec4 sampled = texture2D(image, at / textureSize);
                if (opaque > 0.5) {
                    sampled.a = 1.0;
                }
                // A see-through pixel leaves the target as it is, in every
                // mode, as on the CPU.
                if (sampled.a <= 0.0) {
                    discard;
                }
                vec4 colour = sampled * tint;
                float sa = colour.a * cover;
                // What the blend stage needs for each of the CPU's modes
                // (see `gpu::blend_state`).
                if (mode < 0.5 || (mode > 0.5 && mode < 2.5)) {
                    // alpha, add, subtract: the colour, weighed by `sa`.
                    gl_FragColor = vec4(colour.rgb, sa);
                } else if (mode < 3.5) {
                    // multiply: the factor the target is multiplied by.
                    gl_FragColor = vec4(colour.rgb * sa + 1.0 - sa, colour.a * sa + 1.0 - sa);
                } else if (mode < 4.5) {
                    // screen: the colour already weighed.
                    gl_FragColor = vec4(colour.rgb * sa, sa);
                } else if (mode < 5.5) {
                    // replace: as it is (only drawn with no partial cover).
                    gl_FragColor = colour;
                } else {
                    // premultiplied: the image's colours are weighed
                    // already; the tint's alpha and the cover weigh them
                    // further.
                    gl_FragColor = vec4(sampled.rgb * tint.rgb * tint.a * cover, sa);
                }
            }";
        // SAFETY: the caller's context is current.
        unsafe {
            let program = link(gl, es, vertex, fragment)?;
            gl.use_program(Some(program));
            gl.uniform_1_i32(uniform(gl, program, "image").as_ref(), 0);
            Ok(BlitProgram {
                target: uniform(gl, program, "target"),
                size: uniform(gl, program, "size"),
                pad: uniform(gl, program, "pad"),
                row0: uniform(gl, program, "row0"),
                row1: uniform(gl, program, "row1"),
                texture_size: uniform(gl, program, "textureSize"),
                sub: uniform(gl, program, "sub"),
                tint: uniform(gl, program, "tint"),
                radius: uniform(gl, program, "radius"),
                filtered: uniform(gl, program, "filtered"),
                check_inside: uniform(gl, program, "checkInside"),
                opaque: uniform(gl, program, "opaque"),
                mode: uniform(gl, program, "mode"),
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
    pub(crate) from_mask: Option<glow::UniformLocation>,
    pub(crate) rect: Option<glow::UniformLocation>,
    pub(crate) mask_origin: Option<glow::UniformLocation>,
    pub(crate) mask_corner: Option<glow::UniformLocation>,
    pub(crate) mask_scale: Option<glow::UniformLocation>,
    pub(crate) antialias: Option<glow::UniformLocation>,
    pub(crate) colour: Option<glow::UniformLocation>,
    pub(crate) gradient: Option<glow::UniformLocation>,
    pub(crate) geometry: Option<glow::UniformLocation>,
    pub(crate) stop_colour: Option<glow::UniformLocation>,
    pub(crate) stop_at: Option<glow::UniformLocation>,
    pub(crate) stop_count: Option<glow::UniformLocation>,
    pub(crate) mode: Option<glow::UniformLocation>,
}

impl ShapeProgram {
    pub(crate) unsafe fn new(gl: &glow::Context, es: bool) -> Result<ShapeProgram, String> {
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
            uniform float fromMask;
            uniform vec4 rect;
            uniform sampler2D mask;
            uniform vec2 maskOrigin;
            uniform vec2 maskCorner;
            uniform vec2 maskScale;
            uniform float antialias;
            uniform vec4 colour;
            uniform float gradient;
            uniform vec4 geometry;
            uniform vec4 stopColour[8];
            uniform float stopAt[8];
            uniform float stopCount;
            uniform float mode;

            float coverage(vec2 pixel) {
                if (fromMask > 0.5) {
                    return texture2D(mask, (maskCorner + pixel - maskOrigin + 0.5) * maskScale).r;
                }
                float inside = 0.0;
                for (int s = 0; s < 4; s++) {
                    float sy = pixel.y + (float(s) + 0.5) / 4.0;
                    if (sy >= rect.y && sy < rect.w) {
                        inside += 1.0;
                    }
                }
                float across = clamp(min(pixel.x + 1.0, rect.z) - max(pixel.x, rect.x), 0.0, 1.0);
                return inside / 4.0 * across;
            }

            // The CPU's `Gradient::sample`, at the pixel's centre.
            vec4 paint(vec2 at) {
                if (gradient < 0.5) {
                    return colour;
                }
                float t;
                if (gradient < 1.5) {
                    vec2 axis = geometry.zw - geometry.xy;
                    float length2 = dot(axis, axis);
                    t = length2 <= 1e-12 ? 0.0 : dot(at - geometry.xy, axis) / length2;
                } else {
                    t = abs(geometry.z) <= 1e-12 ? 1.0 : length(at - geometry.xy) / geometry.z;
                }
                t = clamp(t, 0.0, 1.0);
                if (t <= stopAt[0]) {
                    return stopColour[0];
                }
                vec4 last = stopColour[0];
                float lastAt = stopAt[0];
                for (int i = 1; i < 8; i++) {
                    if (float(i) >= stopCount) {
                        break;
                    }
                    if (t <= stopAt[i]) {
                        float span = stopAt[i] - lastAt;
                        float local = span <= 1.1920929e-7 ? 0.0 : (t - lastAt) / span;
                        return last + (stopColour[i] - last) * local;
                    }
                    last = stopColour[i];
                    lastAt = stopAt[i];
                }
                return last;
            }

            void main() {
                vec2 pixel = floor(gl_FragCoord.xy);
                float c = coverage(pixel);
                if (antialias < 0.5) {
                    c = c >= 0.5 ? 1.0 : 0.0;
                }
                if (c <= 0.0) {
                    discard;
                }
                vec4 source = paint(pixel + 0.5);
                float sa = source.a * c;
                if (mode < 0.5 || (mode > 0.5 && mode < 2.5)) {
                    gl_FragColor = vec4(source.rgb, sa);
                } else if (mode < 3.5) {
                    gl_FragColor = vec4(source.rgb * sa + 1.0 - sa, source.a * sa + 1.0 - sa);
                } else {
                    gl_FragColor = vec4(source.rgb * sa, sa);
                }
            }";
        // SAFETY: the caller's context is current.
        unsafe {
            let program = link(gl, es, vertex, fragment)?;
            gl.use_program(Some(program));
            gl.uniform_1_i32(uniform(gl, program, "mask").as_ref(), 1);
            Ok(ShapeProgram {
                target: uniform(gl, program, "target"),
                area: uniform(gl, program, "area"),
                from_mask: uniform(gl, program, "fromMask"),
                rect: uniform(gl, program, "rect"),
                mask_origin: uniform(gl, program, "maskOrigin"),
                mask_corner: uniform(gl, program, "maskCorner"),
                mask_scale: uniform(gl, program, "maskScale"),
                antialias: uniform(gl, program, "antialias"),
                colour: uniform(gl, program, "colour"),
                gradient: uniform(gl, program, "gradient"),
                geometry: uniform(gl, program, "geometry"),
                stop_colour: uniform(gl, program, "stopColour"),
                stop_at: uniform(gl, program, "stopAt"),
                stop_count: uniform(gl, program, "stopCount"),
                mode: uniform(gl, program, "mode"),
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
    pub(crate) mode: Option<glow::UniformLocation>,
    pub(crate) exact: Option<glow::UniformLocation>,
    pub(crate) filtered: Option<glow::UniformLocation>,
}

impl GlyphProgram {
    pub(crate) unsafe fn new(gl: &glow::Context, es: bool) -> Result<GlyphProgram, String> {
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
            uniform float mode;
            uniform float exact;
            uniform float filtered;
            varying vec2 local;
            varying vec4 glyph;

            void main() {
                float a;
                if (exact > 0.5) {
                    a = texture2D(atlas, (glyph.xy + floor(local) + 0.5) * atlasScale).r;
                } else if (filtered > 0.5) {
                    // Past half a texel outside, every tap the CPU takes is
                    // outside the glyph; nearer, the atlas's empty border
                    // gives the zeroes it reads.
                    if (local.x < -0.5 || local.y < -0.5
                        || local.x > glyph.z + 0.5 || local.y > glyph.w + 0.5) {
                        discard;
                    }
                    a = texture2D(atlas, (glyph.xy + local) * atlasScale).r;
                } else {
                    vec2 p = floor(local);
                    if (p.x < 0.0 || p.y < 0.0 || p.x >= glyph.z || p.y >= glyph.w) {
                        discard;
                    }
                    a = texture2D(atlas, (glyph.xy + p + 0.5) * atlasScale).r;
                }
                if (a <= 0.0) {
                    discard;
                }
                float sa = colour.a * a;
                if (mode < 0.5 || (mode > 0.5 && mode < 2.5)) {
                    gl_FragColor = vec4(colour.rgb, sa);
                } else if (mode < 3.5) {
                    gl_FragColor = vec4(colour.rgb * sa + 1.0 - sa, colour.a * sa + 1.0 - sa);
                } else {
                    gl_FragColor = vec4(colour.rgb * sa, sa);
                }
            }";
        // SAFETY: the caller's context is current.
        unsafe {
            let program = link_with(gl, es, vertex, fragment, &["position", "place", "cell"])?;
            gl.use_program(Some(program));
            gl.uniform_1_i32(uniform(gl, program, "atlas").as_ref(), 1);
            Ok(GlyphProgram {
                target: uniform(gl, program, "target"),
                atlas_scale: uniform(gl, program, "atlasScale"),
                colour: uniform(gl, program, "colour"),
                mode: uniform(gl, program, "mode"),
                exact: uniform(gl, program, "exact"),
                filtered: uniform(gl, program, "filtered"),
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
                    gl_FragColor = vec4(colour.rgb, sa);
                } else if (mode < 3.5) {
                    gl_FragColor = vec4(colour.rgb * sa + 1.0 - sa, colour.a * sa + 1.0 - sa);
                } else {
                    gl_FragColor = vec4(colour.rgb * sa, sa);
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

/// A Gaussian blur, one pass of it (see `gpu::blur`, and `raster::blur`,
/// whose numbers it repeats): across, from a copy of the target into a
/// texture between the passes; then down from that one, mixed into the
/// target by a rounded rectangle's coverage. Every tap is read at a texel's
/// centre, so filtering adds nothing; the reach is at most
/// `raster::blur::MOST` either way, the loop's fixed bound.
pub(crate) struct BlurProgram {
    pub(crate) program: glow::Program,
    pub(crate) down: Option<glow::UniformLocation>,
    pub(crate) reach: Option<glow::UniformLocation>,
    pub(crate) sigma: Option<glow::UniformLocation>,
    pub(crate) surface: Option<glow::UniformLocation>,
    pub(crate) here: Option<glow::UniformLocation>,
    pub(crate) source_origin: Option<glow::UniformLocation>,
    pub(crate) source_size: Option<glow::UniformLocation>,
    pub(crate) original_origin: Option<glow::UniformLocation>,
    pub(crate) original_size: Option<glow::UniformLocation>,
    pub(crate) shape: Option<glow::UniformLocation>,
    pub(crate) corner: Option<glow::UniformLocation>,
    pub(crate) opacity: Option<glow::UniformLocation>,
    pub(crate) fade: Option<glow::UniformLocation>,
}

impl BlurProgram {
    pub(crate) unsafe fn new(gl: &glow::Context, es: bool) -> Result<BlurProgram, String> {
        let vertex = "
            attribute vec2 corner;
            void main() {
                gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
            }";
        let fragment = "
            uniform sampler2D source;
            uniform sampler2D original;
            uniform float down;
            uniform float reach;
            uniform float sigma;
            // The target surface's size; where the pixel drawn into is on
            // it (what the render target's corner is); where the source's
            // and the original's corners are on it, and their sizes.
            uniform vec2 surface;
            uniform vec2 here;
            uniform vec2 sourceOrigin;
            uniform vec2 sourceSize;
            uniform vec2 originalOrigin;
            uniform vec2 originalSize;
            uniform vec4 shape;
            uniform float corner;
            uniform float opacity;
            uniform float fade;

            float coverage(vec2 p) {
                float r = min(corner, min((shape.z - shape.x), (shape.w - shape.y)) / 2.0);
                vec2 q = abs(p - (shape.xy + shape.zw) / 2.0) - ((shape.zw - shape.xy) / 2.0 - r);
                float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
                float fading = fade > 0.0 ? clamp((shape.w - p.y) / fade, 0.0, 1.0) : 1.0;
                return clamp(0.5 - d, 0.0, 1.0) * opacity * fading;
            }

            void main() {
                vec2 pixel = floor(gl_FragCoord.xy) + here;
                vec4 sum = vec4(0.0);
                float total = 0.0;
                for (int i = -64; i <= 64; i++) {
                    float fi = float(i);
                    if (abs(fi) > reach) {
                        continue;
                    }
                    float weight = exp(-(fi * fi) / (2.0 * sigma * sigma));
                    vec2 at = pixel;
                    if (down > 0.5) {
                        at.y = clamp(pixel.y + fi, 0.0, surface.y - 1.0);
                    } else {
                        at.x = clamp(pixel.x + fi, 0.0, surface.x - 1.0);
                    }
                    sum += weight * texture2D(source, (at - sourceOrigin + 0.5) / sourceSize);
                    total += weight;
                }
                vec4 blurred = sum / total;
                if (down < 0.5) {
                    gl_FragColor = blurred;
                    return;
                }
                vec4 was = texture2D(original, (pixel - originalOrigin + 0.5) / originalSize);
                gl_FragColor = was + (blurred - was) * coverage(pixel + 0.5);
            }";
        // SAFETY: the caller's context is current.
        unsafe {
            let program = link(gl, es, vertex, fragment)?;
            gl.use_program(Some(program));
            gl.uniform_1_i32(uniform(gl, program, "source").as_ref(), 0);
            gl.uniform_1_i32(uniform(gl, program, "original").as_ref(), 1);
            Ok(BlurProgram {
                down: uniform(gl, program, "down"),
                reach: uniform(gl, program, "reach"),
                sigma: uniform(gl, program, "sigma"),
                surface: uniform(gl, program, "surface"),
                here: uniform(gl, program, "here"),
                source_origin: uniform(gl, program, "sourceOrigin"),
                source_size: uniform(gl, program, "sourceSize"),
                original_origin: uniform(gl, program, "originalOrigin"),
                original_size: uniform(gl, program, "originalSize"),
                shape: uniform(gl, program, "shape"),
                corner: uniform(gl, program, "corner"),
                opacity: uniform(gl, program, "opacity"),
                fade: uniform(gl, program, "fade"),
                program,
            })
        }
    }
}
