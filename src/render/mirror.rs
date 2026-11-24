//! Surfaces on the GPU: each surface's texture twin, which copy is current,
//! and the drawing operations that run there.
//!
//! A surface (the screen, a canvas, an image) is always on the CPU, and once
//! the GPU has drawn with it, on the GPU too. Whichever side writes it makes
//! the other copy stale; the other side brings it up to date before using
//! it — an upload before the GPU reads, a read-back before the CPU does.
//! So every operation can run where it can, and an operation the GPU has no
//! path for yet still sees exactly the pixels the GPU left. With no GPU (a
//! test, `SHINE_GPU_DRAW` unset, no context) nothing here does anything.

use std::collections::HashMap;
use std::rc::Rc;

use super::Renderer;
use crate::geom::Transform;
use crate::gpu::{self, Device, Texture};
use crate::raster::{self, Paint, Rect, Surface};

/// Which copy of a surface is current.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Fresh {
    /// The CPU's: the texture, if any, is stale.
    #[default]
    Cpu,
    /// The GPU's: the CPU's pixels are stale.
    Gpu,
    Both,
}

#[derive(Default)]
pub(crate) struct Mirror {
    pub(crate) texture: Option<Texture>,
    pub(crate) fresh: Fresh,
    /// The texture holds a window's frame as it came, its alpha bytes
    /// meaningless (see [`Renderer::upload_frame`]): read as opaque, and
    /// made opaque when read back.
    pub(crate) opaque: bool,
    /// Two textures over buffers an app shares with this process (see
    /// [`Renderer::show_shared_frame`]), and which of them holds the frame
    /// to show. While there are some, they are this surface's GPU copy.
    pub(crate) shared: Option<([Texture; 2], usize)>,
}

impl Mirror {
    /// The texture holding this surface's GPU copy.
    fn active(&self) -> Option<&Texture> {
        match &self.shared {
            Some((textures, current)) => Some(&textures[*current]),
            None => self.texture.as_ref(),
        }
    }
}

/// The GPU side of a renderer.
pub(crate) struct GpuState {
    pub(crate) dev: Rc<Device>,
    pub(crate) screen: Mirror,
    /// By canvas slot; a slot with no entry has never been on the GPU.
    pub(crate) canvases: HashMap<usize, Mirror>,
    /// What blurs work in, kept from one to the next (see `gpu::blur`).
    pub(crate) blur: Option<gpu::blur::BlurTextures>,
}

/// A surface by where it lives in the renderer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Which {
    Screen,
    Canvas(usize),
}

impl Renderer {
    /// Draw on `dev` from now on, where an operation can.
    pub(crate) fn attach_gpu(&mut self, dev: Rc<Device>) {
        if self.gpu.is_none() && gpu::drawing_wanted() {
            self.gpu = Some(GpuState {
                dev,
                screen: Mirror::default(),
                canvases: HashMap::new(),
                blur: None,
            });
        }
    }

    /// [`Renderer::attach_gpu`] whatever `SHINE_GPU_DRAW` says: the tests'.
    #[cfg(test)]
    pub(crate) fn attach_gpu_always(&mut self, dev: Rc<Device>) {
        self.gpu = Some(GpuState {
            dev,
            screen: Mirror::default(),
            canvases: HashMap::new(),
            blur: None,
        });
    }

    /// Stop using the GPU: every surface the GPU has newer pixels for gets
    /// them back first (unless `keep` is false, when the renderer is going
    /// away), then every texture is let go of.
    pub(crate) fn detach_gpu(&mut self, keep: bool) {
        if keep {
            self.sync_cpu(Which::Screen);
            let slots: Vec<usize> = self
                .gpu
                .as_ref()
                .map(|g| g.canvases.keys().copied().collect())
                .unwrap_or_default();
            for idx in slots {
                self.sync_cpu(Which::Canvas(idx));
            }
        }
        self.gpu = None;
    }

    /// Whether drawing goes to the GPU.
    pub(crate) fn on_gpu(&self) -> bool {
        self.gpu.is_some()
    }

    /// The surface being drawn on.
    pub(crate) fn which_target(&self) -> Which {
        match self.target {
            None => Which::Screen,
            Some(i) => Which::Canvas(i),
        }
    }

    /// The screen's texture, when the GPU holds the newest frame: what to
    /// put on the window without reading it back.
    pub(crate) fn screen_texture(&self) -> Option<&Texture> {
        let gpu = self.gpu.as_ref()?;
        (gpu.screen.fresh == Fresh::Gpu)
            .then_some(gpu.screen.texture.as_ref())
            .flatten()
    }

    /// A canvas slot is free again: its texture goes with it.
    pub(crate) fn forget_canvas(&mut self, idx: usize) {
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.canvases.remove(&idx);
        }
    }

    /// The screen was replaced (a resize): its texture is the old size.
    pub(crate) fn forget_screen(&mut self) {
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.screen = Mirror::default();
        }
    }

    /// Bring the CPU's copy of `which` up to date.
    pub(crate) fn sync_cpu(&mut self, which: Which) {
        let Some(gpu) = self.gpu.as_mut() else { return };
        let mirror = match which {
            Which::Screen => &mut gpu.screen,
            Which::Canvas(i) => match gpu.canvases.get_mut(&i) {
                Some(m) => m,
                None => return,
            },
        };
        if mirror.fresh != Fresh::Gpu {
            return;
        }
        let surface = match which {
            Which::Screen => Some(&mut self.screen),
            Which::Canvas(i) => self.canvases[i].value.as_mut(),
        };
        if let (Some(surface), Some(texture)) = (surface, mirror.active()) {
            crate::trace::copied(false);
            texture.read(&mut surface.buf);
            if mirror.opaque {
                for px in &mut surface.buf {
                    *px |= 0xFF00_0000;
                }
            }
            mirror.fresh = Fresh::Both;
        }
    }

    /// About to change `which` on the CPU: up to date there first, and its
    /// texture stale after.
    pub(crate) fn touch_cpu(&mut self, which: Which) {
        self.sync_cpu(which);
        let Some(gpu) = self.gpu.as_mut() else { return };
        let mirror = match which {
            Which::Screen => Some(&mut gpu.screen),
            Which::Canvas(i) => gpu.canvases.get_mut(&i),
        };
        if let Some(mirror) = mirror {
            mirror.fresh = Fresh::Cpu;
            mirror.opaque = false;
            mirror.shared = None;
        }
    }

    /// Bring the GPU's copy of `which` up to date, making its texture if it
    /// has none; `overwrite` when every pixel is about to be replaced, so
    /// uploading the old ones would be wasted.
    fn sync_gpu(&mut self, which: Which, overwrite: bool) -> Result<(), String> {
        let Some(gpu) = self.gpu.as_mut() else {
            return Err("no GPU".into());
        };
        let surface: &Surface = match which {
            Which::Screen => &self.screen,
            Which::Canvas(i) => self.canvases[i]
                .value
                .as_ref()
                .ok_or("the image is on loan to a draw call")?,
        };
        let dev = Rc::clone(&gpu.dev);
        let mirror = match which {
            Which::Screen => &mut gpu.screen,
            Which::Canvas(i) => gpu.canvases.entry(i).or_default(),
        };
        // Shared buffers are the newest copy there is, already on the GPU.
        if mirror.shared.is_some() {
            return Ok(());
        }
        let fits = mirror
            .texture
            .as_ref()
            .is_some_and(|t| (t.w, t.h) == (surface.w, surface.h));
        if !fits {
            mirror.texture = Some(Texture::new(&dev, surface.w, surface.h)?);
            mirror.fresh = Fresh::Cpu;
        }
        if mirror.fresh == Fresh::Cpu {
            if !overwrite {
                crate::trace::copied(true);
                mirror
                    .texture
                    .as_ref()
                    .expect("made above")
                    .upload(&surface.buf);
            }
            mirror.fresh = Fresh::Both;
        }
        Ok(())
    }

    /// The GPU drew on `which`: its CPU copy is stale.
    fn touched_gpu(&mut self, which: Which) {
        if let Some(gpu) = self.gpu.as_mut() {
            let mirror = match which {
                Which::Screen => Some(&mut gpu.screen),
                Which::Canvas(i) => gpu.canvases.get_mut(&i),
            };
            if let Some(mirror) = mirror {
                mirror.fresh = Fresh::Gpu;
            }
        }
        if which == Which::Screen {
            self.screen_dirty = true;
        }
    }

    fn texture(&self, which: Which) -> Option<&Texture> {
        let gpu = self.gpu.as_ref()?;
        match which {
            Which::Screen => gpu.screen.active(),
            Which::Canvas(i) => gpu.canvases.get(&i)?.active(),
        }
    }

    /// Show an app's frame from the buffers it shares with this process
    /// (see `gpu::dmabuf`): frame `index` of the two. `buffers` comes with
    /// the first frame after the app shared them (or shared new ones, at a
    /// new size), and is made into textures then; the frames after it only
    /// say which of the two is ready. No pixel is copied: image `idx` simply
    /// is that buffer from now on, until the app sends pixels again.
    #[cfg(target_os = "linux")]
    pub(crate) fn show_shared_frame(
        &mut self,
        idx: usize,
        buffers: Option<(&[gpu::dmabuf::DmaBuf; 2], usize, usize)>,
        index: usize,
    ) -> Result<(), String> {
        let gpu = self.gpu.as_mut().ok_or("not drawing on the GPU")?;
        let mirror = gpu.canvases.entry(idx).or_default();
        if let Some((buffers, w, h)) = buffers {
            let first = gpu::dmabuf::import(&gpu.dev, &buffers[0], w, h)?;
            let second = gpu::dmabuf::import(&gpu.dev, &buffers[1], w, h)?;
            mirror.shared = Some(([first, second], 0));
            if gpu::tracing() {
                eprintln!("shine: showing an app's {w}×{h} frames straight from its GPU buffers");
            }
        }
        let Some((_, current)) = mirror.shared.as_mut() else {
            return Err("a shared frame before any shared buffers".into());
        };
        *current = index.min(1);
        mirror.fresh = Fresh::Gpu;
        mirror.opaque = true;
        Ok(())
    }

    /// Whether `which` holds a window's frame with meaningless alpha.
    fn is_opaque_frame(&self, which: Which) -> bool {
        let Some(gpu) = self.gpu.as_ref() else {
            return false;
        };
        match which {
            Which::Screen => gpu.screen.opaque,
            Which::Canvas(i) => gpu.canvases.get(&i).is_some_and(|m| m.opaque),
        }
    }

    /// About to draw on `which` on the GPU: a window's frame (alpha bytes
    /// meaningless) is made a proper image first, through the CPU, so
    /// blending onto it reads real alpha. Rare: nothing draws onto a
    /// window's frame but a test.
    fn prepare_target(&mut self, which: Which) {
        if self.is_opaque_frame(which) {
            self.touch_cpu(which);
        }
    }

    /// Replace image `idx`'s pixels with a window's frame — little-endian
    /// `0x00RRGGBB`, its size — straight into its texture: what a display
    /// server does with every frame an app sends. The CPU's copy goes stale
    /// rather than being written, which saves a pass over every pixel; it is
    /// brought back (alpha made opaque) only if the CPU ever needs it.
    /// `false` when there is no GPU, for the CPU to take the frame.
    pub(crate) fn upload_frame(&mut self, idx: usize, frame: &[u8]) -> bool {
        // Pixels again: whatever buffers the app shared are behind it now.
        if let Some(mirror) = self.gpu.as_mut().and_then(|g| g.canvases.get_mut(&idx)) {
            mirror.shared = None;
        }
        if !self.on_gpu() || self.sync_gpu(Which::Canvas(idx), true).is_err() {
            return false;
        }
        let Some(gpu) = self.gpu.as_mut() else {
            return false;
        };
        let Some(mirror) = gpu.canvases.get_mut(&idx) else {
            return false;
        };
        let Some(texture) = mirror.texture.as_ref() else {
            return false;
        };
        texture.upload_bytes(frame);
        mirror.fresh = Fresh::Gpu;
        mirror.opaque = true;
        true
    }

    /// `Graphics.clear` on the GPU. `false` when it has to be done on the
    /// CPU instead.
    pub(crate) fn gpu_clear(&mut self, color: [f32; 4], clip: Rect) -> bool {
        if !self.on_gpu() {
            return false;
        }
        let which = self.which_target();
        self.prepare_target(which);
        let (w, h) = self.target_size();
        let (x0, y0, x1, y1) = clip.pixel_bounds(w, h);
        let whole = (x0, y0, x1, y1) == (0, 0, w, h);
        if self.sync_gpu(which, whole).is_err() {
            return false;
        }
        let Some(texture) = self.texture(which) else {
            return false;
        };
        gpu::clear(texture, color, gpu::Clip { x0, y0, x1, y1 });
        self.touched_gpu(which);
        if let Some(gpu) = self.gpu.as_mut() {
            // Cleared everywhere, a frame is a frame no longer.
            if let Which::Canvas(i) = which
                && let Some(m) = gpu.canvases.get_mut(&i)
                && whole
            {
                m.opaque = false;
            }
        }
        true
    }

    /// Blur `area` of the target on the GPU (see `raster::blur`). `false`
    /// when it has to be done on the CPU instead.
    pub(crate) fn gpu_blur(&mut self, area: &raster::BlurArea) -> bool {
        if !self.on_gpu() {
            return false;
        }
        let which = self.which_target();
        self.prepare_target(which);
        if self.sync_gpu(which, false).is_err() {
            return false;
        }
        let size = self.target_size();
        let Some(gpu) = self.gpu.as_mut() else {
            return false;
        };
        let kept = gpu.blur.take();
        match gpu::blur::BlurTextures::fit(&gpu.dev, kept, area, size) {
            Ok(textures) => gpu.blur = Some(textures),
            Err(_) => return false,
        }
        let (Some(texture), Some(textures)) = (self.texture(which), self.gpu.as_ref().and_then(|g| g.blur.as_ref()))
        else {
            return false;
        };
        gpu::blur::blur(texture, textures, area);
        self.touched_gpu(which);
        true
    }

    /// Fill the device-space paths in the scratch buffers with `paint` on
    /// the GPU. A lone axis-aligned rectangle (most of a UI) needs nothing
    /// from the CPU; any other shape's coverage is worked out by the CPU's
    /// scanline filler and goes up as a mask, so the edges are the CPU's to
    /// the pixel. `false` when it has to be done on the CPU instead: the
    /// replace blend mode (see `gpu::can_fill`).
    pub(crate) fn gpu_fill(&mut self, paint: &Paint) -> bool {
        if !self.on_gpu() || !gpu::can_fill(paint.blend) {
            return false;
        }
        let (w, h) = self.target_size();
        let clip = paint.clip.intersect(&Rect::surface(w, h));
        if clip.is_empty() {
            return true;
        }
        let target = self.which_target();
        self.prepare_target(target);

        let paths = self.scratch.paths.paths();
        let rect = if paint.gradient.is_none() {
            raster::axis_aligned_rect(paths)
        } else {
            None
        };
        // A shape drawn before, whole and at a place a whole number of
        // pixels from where it was: its coverage is in the shape atlas.
        if rect.is_none()
            && let Some(done) = self.gpu_fill_kept(paint, clip, target)
        {
            return done;
        }

        // What to fill, and how its coverage is found.
        let mut mask = std::mem::take(&mut self.scratch.mask);
        let cover = match rect {
            Some(rect) => {
                let r = rect.intersect(&clip);
                if r.is_empty() {
                    self.scratch.mask = mask;
                    return true;
                }
                let (x0, y0, x1, y1) = r.pixel_bounds(w, h);
                Some((gpu::Clip { x0, y0, x1, y1 }, Some(r)))
            }
            None => raster::coverage_mask(
                self.scratch.paths.paths(),
                clip,
                (w, h),
                &mut self.scratch.fill,
                &mut mask,
            )
            .map(|(x0, y0, mw, mh)| {
                (
                    gpu::Clip {
                        x0,
                        y0,
                        x1: x0 + mw,
                        y1: y0 + mh,
                    },
                    None,
                )
            }),
        };
        let Some((area, rect)) = cover else {
            self.scratch.mask = mask;
            return true;
        };
        if area.x0 >= area.x1 || area.y0 >= area.y1 || self.sync_gpu(target, false).is_err() {
            self.scratch.mask = mask;
            return area.x0 >= area.x1 || area.y0 >= area.y1;
        }
        if let Some(texture) = self.texture(target) {
            let cover = match rect {
                Some(r) => gpu::Cover::Rect(r.x0, r.y0, r.x1, r.y1),
                None => gpu::Cover::Mask {
                    origin: (area.x0, area.y0),
                    size: (area.x1 - area.x0, area.y1 - area.y0),
                    bytes: &mask,
                },
            };
            gpu::fill(
                texture,
                &gpu::Fill {
                    colour: paint.color,
                    gradient: paint.gradient,
                    antialias: paint.antialias,
                    blend: paint.blend,
                    area,
                },
                cover,
            );
        }
        self.scratch.mask = mask;
        self.touched_gpu(target);
        true
    }

    /// Fill the scratch paths from the shape atlas, keeping their coverage
    /// there first if it isn't yet: `None` for a shape that can't be kept
    /// (cut by the scissor, or too large), which is filled the usual way.
    ///
    /// A shape is known by its outline relative to the pixel its box starts
    /// in, so the same button drawn a few pixels further down — or the same
    /// card every frame — is the same shape, and costs no coverage pass on
    /// the CPU and nothing uploaded.
    fn gpu_fill_kept(&mut self, paint: &Paint, clip: Rect, target: Which) -> Option<bool> {
        use std::hash::{Hash, Hasher};

        let paths = self.scratch.paths.paths();
        let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
        let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(x, y) in paths.iter().flatten() {
            if !x.is_finite() || !y.is_finite() {
                return None;
            }
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let whole = x0 >= clip.x0 && y0 >= clip.y0 && x1 <= clip.x1 && y1 <= clip.y1;
        if !whole || x0 > x1 {
            return None;
        }
        let (ox, oy) = (x0.floor(), y0.floor());
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        paths.len().hash(&mut hasher);
        for path in paths {
            path.len().hash(&mut hasher);
            for &(x, y) in path {
                (x - ox).to_bits().hash(&mut hasher);
                (y - oy).to_bits().hash(&mut hasher);
            }
        }
        let key = (hasher.finish(), '\u{1}');

        let (w, h) = self.target_size();
        let dev = Rc::clone(&self.gpu.as_ref()?.dev);
        let corner = match gpu::kept_shape(&dev, key) {
            Some(corner) => corner,
            None => {
                let mut mask = std::mem::take(&mut self.scratch.mask);
                let found = raster::coverage_mask(
                    self.scratch.paths.paths(),
                    clip,
                    (w, h),
                    &mut self.scratch.fill,
                    &mut mask,
                );
                let kept = found.and_then(|(_, _, mw, mh)| gpu::keep_shape(&dev, key, &mask, mw, mh));
                self.scratch.mask = mask;
                match (found, kept) {
                    (None, _) => return Some(true),
                    (Some(_), None) => return None,
                    (Some(_), Some(corner)) => corner,
                }
            }
        };
        // The box is the shape's pixels, as `coverage_mask` found them.
        let (ax0, ay0, ax1, ay1) = Rect { x0, y0, x1, y1 }.pixel_bounds(w, h);
        if ax0 >= ax1 || ay0 >= ay1 || self.sync_gpu(target, false).is_err() {
            return Some(ax0 >= ax1 || ay0 >= ay1);
        }
        let texture = self.texture(target)?;
        gpu::fill(
            texture,
            &gpu::Fill {
                colour: paint.color,
                gradient: paint.gradient,
                antialias: paint.antialias,
                blend: paint.blend,
                area: gpu::Clip {
                    x0: ax0,
                    y0: ay0,
                    x1: ax1,
                    y1: ay1,
                },
            },
            gpu::Cover::Kept {
                origin: (ax0, ay0),
                corner,
            },
        );
        self.touched_gpu(target);
        Some(true)
    }

    /// Draw a run of laid-out glyphs (from the selected font) with `paint`
    /// on the GPU. `false` when it has to be done on the CPU instead: the
    /// replace blend mode, or a glyph too large for the atlas.
    pub(crate) fn gpu_text(&mut self, paint: &Paint, placed: &[(char, Transform)]) -> bool {
        if !self.on_gpu() || !gpu::can_fill(paint.blend) {
            return false;
        }
        let (w, h) = self.target_size();
        let (x0, y0, x1, y1) = paint.clip.pixel_bounds(w, h);
        if x0 >= x1 || y0 >= y1 || placed.is_empty() {
            return true;
        }
        let target = self.which_target();
        self.prepare_target(target);
        if self.sync_gpu(target, false).is_err() {
            return false;
        }
        let key = self.font_key();
        let Some(font) = self.fonts[self.st.font].value.as_ref() else {
            return false;
        };
        let glyphs: Vec<gpu::GlyphDraw> = placed
            .iter()
            .filter_map(|(ch, t)| {
                let glyph = font.glyph(*ch)?;
                Some(gpu::GlyphDraw {
                    key: (key, *ch),
                    mask: &glyph.mask.data,
                    w: glyph.mask.w,
                    h: glyph.mask.h,
                    xform: *t,
                })
            })
            .collect();
        let Some(texture) = self.texture(target) else {
            return false;
        };
        let drawn = gpu::draw_glyphs(
            texture,
            &gpu::TextPaint {
                colour: paint.color,
                blend: paint.blend,
                linear: paint.linear_filter,
                clip: gpu::Clip { x0, y0, x1, y1 },
            },
            &glyphs,
        );
        if drawn {
            self.touched_gpu(target);
        }
        drawn
    }

    /// Draw `frame` of canvas `idx` onto the target through `xform`, on the
    /// GPU. `false` when it has to be done on the CPU instead: the replace
    /// blend mode with rounded corners (see `gpu::can_blend`).
    pub(crate) fn gpu_blit(
        &mut self,
        idx: usize,
        frame: Rect,
        xform: &Transform,
        paint: &Paint,
    ) -> bool {
        if !self.on_gpu() || !gpu::can_blend(paint.blend, paint.corner_radius) {
            return false;
        }
        let Some((sw, sh)) = self.canvases[idx].value.as_ref().map(|s| (s.w, s.h)) else {
            return false;
        };
        // The region, kept to the image as the CPU keeps it.
        let sub = frame.intersect(&Rect::surface(sw, sh));
        let (rw, rh) = (sub.x1 - sub.x0, sub.y1 - sub.y0);
        if rw <= 0.0 || rh <= 0.0 {
            return true;
        }
        let (w, h) = self.target_size();
        let (mut x0, mut y0, mut x1, mut y1) = paint.clip.pixel_bounds(w, h);

        // Square to the target (scaled, flipped, moved, not turned): the
        // pixels inside are worked out here exactly as the CPU works them
        // out, and the scissor keeps to them. A pixel whose centre falls on
        // the region's edge is then in or out by the same arithmetic on
        // both sides, which no shader in single precision can promise.
        let square = xform.b.abs() < 1e-9 && xform.c.abs() < 1e-9;
        if square {
            let Some(inv) = xform.inverse() else {
                return true;
            };
            // Searched from just outside where the region lands, not from
            // the clip's edges, which can be the whole target away.
            let (ax, ay) = xform.apply(0.0, 0.0);
            let (bx, by) = xform.apply(rw, rh);
            let near = |a: f64, b: f64, lo: usize, hi: usize| {
                let from = (a.min(b).floor() - 1.0).max(lo as f64) as usize;
                let to = (a.max(b).ceil() + 1.0).max(0.0) as usize;
                (from.min(hi), to.clamp(lo, hi))
            };
            let (sx0, sx1) = near(ax, bx, x0, x1);
            let (sy0, sy1) = near(ay, by, y0, y1);
            (x0, x1) = inside_span(inv.a, inv.tx, rw, sx0, sx1);
            (y0, y1) = inside_span(inv.d, inv.ty, rh, sy0, sy1);
        }
        if x0 >= x1 || y0 >= y1 {
            return true;
        }

        let source = Which::Canvas(idx);
        let target = self.which_target();
        self.prepare_target(target);
        let opaque = self.is_opaque_frame(source);
        if self.sync_gpu(source, false).is_err() || self.sync_gpu(target, false).is_err() {
            return false;
        }
        let (Some(src), Some(dst)) = (self.texture(source), self.texture(target)) else {
            return false;
        };
        gpu::blit(
            dst,
            src,
            &gpu::Blit {
                sub: (sub.x0, sub.y0, rw, rh),
                xform: *xform,
                tint: paint.color,
                corner_radius: paint.corner_radius,
                linear: paint.linear_filter,
                clip: gpu::Clip { x0, y0, x1, y1 },
                check_inside: !square,
                opaque,
                blend: paint.blend,
            },
        );
        self.touched_gpu(target);
        true
    }
}

/// The pixels `lo..hi` along one axis whose centres map into `0..extent` of
/// the region, `u = scale * (p + 0.5) + offset` being the region coordinate
/// of pixel `p`: the CPU's own test (`raster::blit_scaled`). The map is
/// monotonic, so the pixels inside are one run, found from both ends.
fn inside_span(scale: f64, offset: f64, extent: f64, lo: usize, hi: usize) -> (usize, usize) {
    let inside = |p: usize| (0.0..extent).contains(&(scale * (p as f64 + 0.5) + offset));
    let Some(first) = (lo..hi).find(|&p| inside(p)) else {
        return (lo, lo);
    };
    let last = (first..hi).rev().find(|&p| inside(p)).unwrap_or(first);
    (first, last + 1)
}
