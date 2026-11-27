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
}

/// A surface by where it lives in the renderer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Which {
    Screen,
    Canvas(usize),
}

/// A shape whose coverage the shape atlas holds whole: under which key,
/// and its box, counted from the pixel the shape was asked at (as a
/// thing known by what was asked for) or from the target's corner (as
/// `gpu_fill_kept` reports the one it has just filled).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Known {
    pub(crate) key: (u64, char),
    pub(crate) dx: i64,
    pub(crate) dy: i64,
    pub(crate) w: usize,
    pub(crate) h: usize,
}

impl Renderer {
    /// Draw on `dev` from now on, where an operation can.
    pub(crate) fn attach_gpu(&mut self, dev: Rc<Device>) {
        if self.gpu.is_none() && gpu::drawing_wanted() {
            self.gpu = Some(GpuState {
                dev,
                screen: Mirror::default(),
                canvases: HashMap::new(),
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
            // A window's frame may be the corner of a larger shared buffer.
            texture.read_corner(&mut surface.buf, surface.w, surface.h);
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
    pub(super) fn sync_gpu(&mut self, which: Which, overwrite: bool) -> Result<(), String> {
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
    pub(super) fn touched_gpu(&mut self, which: Which) {
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

    pub(super) fn texture(&self, which: Which) -> Option<&Texture> {
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

    /// Canvas `to` takes over the shared buffers canvas `from` showed its
    /// frames from: a window's image is a new canvas when the window
    /// changes size, and the buffers the app shares, already made into
    /// textures here, are still the ones it draws in.
    #[cfg(target_os = "linux")]
    pub(crate) fn move_shared_frames(&mut self, from: usize, to: usize) {
        let Some(gpu) = self.gpu.as_mut() else { return };
        let Some(shared) = gpu.canvases.get_mut(&from).and_then(|m| m.shared.take()) else {
            return;
        };
        gpu.canvases.entry(to).or_default().shared = Some(shared);
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
    pub(super) fn prepare_target(&mut self, which: Which) {
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
        // A shape drawn before, at a place a whole number of pixels from
        // where it was: its coverage is in the shape atlas.
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
                crate::trace::covered(mw * mh);
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
            let fill = gpu::Fill {
                colour: paint.color,
                gradient: paint.gradient,
                antialias: paint.antialias,
                blend: paint.blend,
                area,
            };
            // A GPU that cannot run this fill's shader: the CPU's, then.
            // It is the same shader for every piece of the shape, so the
            // first one says, before anything is drawn.
            if !gpu::fill(texture, &fill, cover) {
                self.scratch.mask = mask;
                return false;
            }
        }
        self.scratch.mask = mask;
        self.touched_gpu(target);
        true
    }

    /// Fill the scratch paths from the shape atlas, keeping their coverage
    /// there first if it isn't yet: `None` for a shape that can't be kept
    /// (cut by a scissor that isn't on whole pixels, or too large for the
    /// atlas), which is filled the usual way.
    ///
    /// A shape is known by its outline relative to the pixel its box starts
    /// in, so the same button drawn a few pixels further down — or the same
    /// card every frame — is the same shape, and costs no coverage pass on
    /// the CPU and nothing uploaded.
    ///
    /// A shape the scissor cuts is kept too, one of two ways:
    ///
    /// - **whole**, worked out in a box of its own, when more than a sliver
    ///   of it shows: the cut is then the GPU's, by the area it draws. A list
    ///   being scrolled always has a card half out of view, cut at a
    ///   different row every frame, and covering that card afresh each
    ///   time — hundreds of thousands of pixels on the CPU, then uploaded —
    ///   was most of what scrolling cost;
    /// - **the piece that shows**, when that is a small part of it: a
    ///   window's outline is drawn as its four corners, each the whole
    ///   outline cut to a small box, and keeping the whole for those would
    ///   cover a window's worth of pixels to draw a few.
    fn gpu_fill_kept(&mut self, paint: &Paint, clip: Rect, target: Which) -> Option<bool> {
        use std::hash::{Hash, Hasher};

        // The outline on a grid of 256ths of a pixel, which is finer than
        // coverage can tell apart. The same card a few pixels further down
        // comes out of the layout's arithmetic a hair off (68.3 − 100 and
        // 68.3 − 101 differ in their last bit), and told apart by every bit
        // it would be a new shape at every step of a scroll.
        const GRID: f64 = 256.0;
        let on_grid = |v: f64| (v * GRID).round() / GRID;
        /// The longest side kept: the atlas's own limit, less its borders.
        const LONGEST: usize = 4000;

        let paths = self.scratch.paths.paths();
        let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
        let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(x, y) in paths.iter().flatten() {
            if !x.is_finite() || !y.is_finite() {
                return None;
            }
            let (x, y) = (on_grid(x), on_grid(y));
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        if x0 > x1 {
            return None;
        }
        let (w, h) = self.target_size();

        // The shape's box in whole pixels, wherever it is: it may start
        // left of the surface or end below it.
        let (bx0, by0) = (x0.floor(), y0.floor());
        let (bw, bh) = (x1.ceil() - bx0, y1.ceil() - by0);
        if bw < 1.0 || bh < 1.0 || bw > 1e6 || bh > 1e6 {
            return None;
        }
        let (bw, bh) = (bw as usize, bh as usize);

        // The pixels of it to draw: its box, within the scissor.
        let whole = x0 >= clip.x0 && y0 >= clip.y0 && x1 <= clip.x1 && y1 <= clip.y1;
        let on_pixels = |v: f64| v.fract() == 0.0;
        if !whole && !(on_pixels(clip.x0) && on_pixels(clip.y0) && on_pixels(clip.x1) && on_pixels(clip.y1)) {
            return None;
        }
        let seen = Rect {
            x0: bx0,
            y0: by0,
            x1: bx0 + bw as f64,
            y1: by0 + bh as f64,
        }
        .intersect(&clip);
        let (ax0, ay0, ax1, ay1) = seen.pixel_bounds(w, h);
        if ax0 >= ax1 || ay0 >= ay1 {
            return Some(true);
        }

        // In its own box: each point from the box's corner, in grid steps.
        let local = |(x, y): crate::geom::Point| (on_grid(x) - bx0, on_grid(y) - by0);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        paths.len().hash(&mut hasher);
        for path in paths {
            path.len().hash(&mut hasher);
            for &point in path {
                let (x, y) = local(point);
                ((x * GRID) as i64).hash(&mut hasher);
                ((y * GRID) as i64).hash(&mut hasher);
            }
        }
        let outline = hasher.finish();
        let whole_key = (outline, '\u{1}');

        // The piece that shows, from the box's corner; and whether the
        // whole is worth keeping instead: all of it shows, or enough that
        // it is on its way into view (a card scrolling in, of which more
        // shows every frame) rather than a corner cut from something big.
        let piece = (
            (ax0 as f64 - bx0) as usize,
            (ay0 as f64 - by0) as usize,
            (ax1 as f64 - bx0) as usize,
            (ay1 as f64 - by0) as usize,
        );
        let fits = bw <= LONGEST && bh <= LONGEST;
        let shows = (piece.2 - piece.0) * (piece.3 - piece.1);
        let keep_whole = fits && (whole || shows * 16 >= bw * bh);

        let dev = Rc::clone(&self.gpu.as_ref()?.dev);
        // Where in the atlas, and the target pixel its first texel is for.
        // For whoever asked for this shape by name (`draw_known`): where
        // its coverage is kept whole, if it is by the end of this.
        let as_whole = Known {
            key: whole_key,
            dx: bx0 as i64,
            dy: by0 as i64,
            w: bw,
            h: bh,
        };
        let (corner, origin) = if let Some(corner) = gpu::kept_shape(&dev, whole_key) {
            self.kept_whole = Some(as_whole);
            (corner, (bx0 as i64, by0 as i64))
        } else {
            let (key, cut) = if keep_whole {
                (whole_key, (0, 0, bw, bh))
            } else {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                (outline, piece).hash(&mut hasher);
                ((hasher.finish(), '\u{2}'), piece)
            };
            let origin = (bx0 as i64 + cut.0 as i64, by0 as i64 + cut.1 as i64);
            match gpu::kept_shape(&dev, key) {
                Some(corner) => (corner, origin),
                None => {
                    // On a surface just the box's size, the outline as it
                    // was told apart: so the coverage kept is the one for
                    // this key, whichever shape sharing it came first.
                    let local: Vec<Vec<crate::geom::Point>> = paths
                        .iter()
                        .map(|path| path.iter().map(|&point| local(point)).collect())
                        .collect();
                    let within = Rect {
                        x0: cut.0 as f64,
                        y0: cut.1 as f64,
                        x1: cut.2 as f64,
                        y1: cut.3 as f64,
                    };
                    let mut mask = std::mem::take(&mut self.scratch.mask);
                    let found = raster::coverage_mask(
                        &local,
                        within,
                        (bw, bh),
                        &mut self.scratch.fill,
                        &mut mask,
                    );
                    // Exactly the cut, or it isn't kept: where it is drawn
                    // from is reckoned from the cut's corner.
                    let (cw, ch) = (cut.2 - cut.0, cut.3 - cut.1);
                    let kept = match found {
                        Some(found) if found == (cut.0, cut.1, cw, ch) => {
                            crate::trace::covered(cw * ch);
                            gpu::keep_shape(&dev, key, &mask, cw, ch)
                        }
                        _ => None,
                    };
                    self.scratch.mask = mask;
                    match (found, kept) {
                        (None, _) => return Some(true),
                        (Some(_), None) => return None,
                        (Some(_), Some(corner)) => {
                            if keep_whole {
                                self.kept_whole = Some(as_whole);
                            }
                            (corner, origin)
                        }
                    }
                }
            }
        };
        if self.sync_gpu(target, false).is_err() {
            return Some(false);
        }
        let texture = self.texture(target)?;
        let fill = gpu::Fill {
            colour: paint.color,
            gradient: paint.gradient,
            antialias: paint.antialias,
            blend: paint.blend,
            area: gpu::Clip { x0: ax0, y0: ay0, x1: ax1, y1: ay1 },
        };
        if !gpu::fill(texture, &fill, gpu::Cover::Kept { origin, corner }) {
            return Some(false);
        }
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
        if let Some(drawn) = self.gpu_text_kept(paint, placed, target) {
            return drawn;
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

    /// Fill a shape the atlas holds whole, found by what was asked for
    /// (`known`, from `draw_known`) with no outline made at all: `anchor`
    /// is the pixel the shape was asked at, which its box is counted from.
    /// `None` when it is not held, or cannot be drawn so, and the caller
    /// makes the outline and fills that.
    ///
    /// A shape is otherwise known by its outline (`gpu_fill_kept`): made
    /// point by point, stroked if it is a line, moved to the screen and
    /// hashed, for every key cap and icon of every frame, to find coverage
    /// the atlas has had since the first. That was more than half of what
    /// a frame cost the processor once the chip had the drawing.
    pub(crate) fn gpu_fill_known(
        &mut self,
        known: u64,
        anchor: (i64, i64),
        paint: &Paint,
    ) -> Option<bool> {
        if !self.on_gpu() || !gpu::can_fill(paint.blend) || paint.gradient.is_some() {
            return None;
        }
        let Known { key, dx, dy, w: bw, h: bh } = *self.known.get(&known)?;
        let dev = Rc::clone(&self.gpu.as_ref()?.dev);
        // Gone when the atlas was made anew: kept again the long way.
        let corner = gpu::kept_shape(&dev, key)?;

        let (w, h) = self.target_size();
        let clip = paint.clip.intersect(&Rect::surface(w, h));
        if clip.is_empty() {
            return Some(true);
        }
        let origin = (anchor.0 + dx, anchor.1 + dy);
        let place = Rect {
            x0: origin.0 as f64,
            y0: origin.1 as f64,
            x1: (origin.0 + bw as i64) as f64,
            y1: (origin.1 + bh as i64) as f64,
        };
        // Cut by a scissor that is not on whole pixels: not this way.
        let whole =
            place.x0 >= clip.x0 && place.y0 >= clip.y0 && place.x1 <= clip.x1 && place.y1 <= clip.y1;
        let on_pixels = |v: f64| v.fract() == 0.0;
        if !whole && !(on_pixels(clip.x0) && on_pixels(clip.y0) && on_pixels(clip.x1) && on_pixels(clip.y1)) {
            return None;
        }
        let (ax0, ay0, ax1, ay1) = place.intersect(&clip).pixel_bounds(w, h);
        if ax0 >= ax1 || ay0 >= ay1 {
            return Some(true);
        }
        let target = self.which_target();
        self.prepare_target(target);
        if self.sync_gpu(target, false).is_err() {
            return None;
        }
        let texture = self.texture(target)?;
        let fill = gpu::Fill {
            colour: paint.color,
            gradient: None,
            antialias: paint.antialias,
            blend: paint.blend,
            area: gpu::Clip { x0: ax0, y0: ay0, x1: ax1, y1: ay1 },
        };
        if !gpu::fill(texture, &fill, gpu::Cover::Kept { origin, corner }) {
            return None;
        }
        self.touched_gpu(target);
        Some(true)
    }

    /// Draw a run of glyphs that all stand on whole pixels (nearly all text
    /// does) as one shape from the shape atlas: the run's coverage, put
    /// together from its glyphs' the first time that run is drawn, and a
    /// single square from then on, which joins the batch with the
    /// rectangles around it. `None` when the run is not such a one, or is
    /// too large to keep, and is drawn glyph by glyph.
    ///
    /// A glyph each was a square each, six corners, in a draw of its own
    /// for every line of text, which also ended the batch of shapes before
    /// it. On a chip that leaves the corners to the processor (Intel's up
    /// to the third generation: Mesa works each one out in an interpreter)
    /// the corners are what a frame costs, about twenty microseconds each
    /// on an Atom, and a page of text is a few thousand of them: a window
    /// drew slower on that chip than without it.
    ///
    /// A run is known by its font, its characters and where each stands
    /// from the run's corner, so the same words anywhere are the same
    /// shape. Where two glyphs overlap, their coverage is joined as two
    /// opaque layers would be: exact for opaque text, and a shade off in
    /// those few pixels for text drawn faded.
    ///
    /// It is kept the second time it is drawn. The first time each glyph
    /// goes into the batch by itself, from the same atlas, where a glyph
    /// is kept once however many runs it is in: text that is new every
    /// time it is drawn (a clock's seconds, a meter's numbers) would
    /// otherwise fill the atlas with runs nothing asks for again, and an
    /// atlas that is full is emptied, shapes and all.
    fn gpu_text_kept(
        &mut self,
        paint: &Paint,
        placed: &[(char, Transform)],
        target: Which,
    ) -> Option<bool> {
        /// The largest run kept, in pixels: a long line, or a paragraph.
        const MOST: usize = 128 * 1024;
        /// Marks a run's key among the shape atlas's (1 and 2 are shapes').
        const RUN: char = '\u{3}';
        /// Runs remembered as drawn once, before they are forgotten.
        const MOST_SEEN: usize = 8192;

        let font = self.fonts[self.st.font].value.as_ref()?;
        // Each glyph's place and size, and the box they fill together.
        let mut boxes: Vec<(char, i64, i64, usize, usize)> = Vec::with_capacity(placed.len());
        let (mut bx0, mut by0, mut bx1, mut by1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        for (ch, t) in placed {
            let (ox, oy) = raster::axis_aligned_offset(t)?;
            let Some(glyph) = font.glyph(*ch) else {
                continue;
            };
            let (gw, gh) = (glyph.mask.w, glyph.mask.h);
            if gw == 0 || gh == 0 {
                continue;
            }
            boxes.push((*ch, ox, oy, gw, gh));
            bx0 = bx0.min(ox);
            by0 = by0.min(oy);
            bx1 = bx1.max(ox + gw as i64);
            by1 = by1.max(oy + gh as i64);
        }
        if boxes.is_empty() {
            return Some(true);
        }
        let (bw, bh) = ((bx1 - bx0) as usize, (by1 - by0) as usize);
        if bw.checked_mul(bh).is_none_or(|area| area > MOST) {
            return None;
        }

        // The pixels of it to draw: its box, within the scissor.
        let (w, h) = self.target_size();
        let seen = Rect {
            x0: bx0 as f64,
            y0: by0 as f64,
            x1: bx1 as f64,
            y1: by1 as f64,
        }
        .intersect(&paint.clip);
        let (ax0, ay0, ax1, ay1) = seen.pixel_bounds(w, h);
        if ax0 >= ax1 || ay0 >= ay1 {
            return Some(true);
        }

        // Not the standard hasher: this is done for every line of text of
        // every frame, and nothing here is an adversary's to choose.
        let mix = |hash: u64, value: u64| (hash.rotate_left(5) ^ value).wrapping_mul(0x517c_c1b7_2722_0a95);
        let mut hash = mix(0x9e37_79b9_7f4a_7c15, self.font_key());
        for &(ch, ox, oy, _, _) in &boxes {
            hash = mix(hash, u64::from(ch));
            hash = mix(hash, ((ox - bx0) as u64) << 32 | (oy - by0) as u64);
        }
        let key = (hash, RUN);

        let dev = Rc::clone(&self.gpu.as_ref()?.dev);
        let paint_area = |x0, y0, x1, y1| gpu::Fill {
            colour: paint.color,
            gradient: None,
            antialias: true,
            blend: paint.blend,
            area: gpu::Clip { x0, y0, x1, y1 },
        };
        let corner = match gpu::kept_shape(&dev, key) {
            Some(corner) => corner,
            // The first time: glyph by glyph. A glyph is a shape by its
            // font and character; one whose character is a mark of the
            // atlas's own has no glyph worth the doubt.
            None if !self.runs_seen.contains(&hash) => {
                if boxes.iter().any(|&(ch, ..)| ch <= RUN) {
                    return None;
                }
                let font_key = self.font_key();
                let texture = self.texture(target)?;
                for &(ch, ox, oy, gw, gh) in &boxes {
                    let glyph = font.glyph(ch)?;
                    let one = (font_key, ch);
                    let corner = match gpu::kept_shape(&dev, one) {
                        Some(corner) => corner,
                        None => gpu::keep_shape(&dev, one, &glyph.mask.data, gw, gh)?,
                    };
                    let place = Rect {
                        x0: ox as f64,
                        y0: oy as f64,
                        x1: (ox + gw as i64) as f64,
                        y1: (oy + gh as i64) as f64,
                    }
                    .intersect(&paint.clip);
                    let (x0, y0, x1, y1) = place.pixel_bounds(w, h);
                    if x0 >= x1 || y0 >= y1 {
                        continue;
                    }
                    let cover = gpu::Cover::Kept { origin: (ox, oy), corner };
                    if !gpu::fill(texture, &paint_area(x0, y0, x1, y1), cover) {
                        return None;
                    }
                }
                if self.runs_seen.len() >= MOST_SEEN {
                    self.runs_seen.clear();
                }
                self.runs_seen.insert(hash);
                self.touched_gpu(target);
                return Some(true);
            }
            None => {
                self.runs_seen.remove(&hash);
                let mut mask = std::mem::take(&mut self.scratch.mask);
                mask.clear();
                mask.resize(bw * bh, 0);
                for &(ch, ox, oy, gw, gh) in &boxes {
                    let Some(glyph) = font.glyph(ch) else {
                        continue;
                    };
                    let (left, top) = ((ox - bx0) as usize, (oy - by0) as usize);
                    for row in 0..gh {
                        let from = &glyph.mask.data[row * gw..row * gw + gw];
                        let at = (top + row) * bw + left;
                        for (into, &cover) in mask[at..at + gw].iter_mut().zip(from) {
                            let (had, more) = (u32::from(*into), u32::from(cover));
                            *into = (had + more - raster::div255(had * more)) as u8;
                        }
                    }
                }
                let kept = gpu::keep_shape(&dev, key, &mask, bw, bh);
                self.scratch.mask = mask;
                kept?
            }
        };
        let fill = paint_area(ax0, ay0, ax1, ay1);
        let origin = (bx0, by0);
        let texture = self.texture(target)?;
        if !gpu::fill(texture, &fill, gpu::Cover::Kept { origin, corner }) {
            return None;
        }
        self.touched_gpu(target);
        Some(true)
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
        let drawn = gpu::blit(
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
        if drawn {
            self.touched_gpu(target);
        }
        drawn
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
