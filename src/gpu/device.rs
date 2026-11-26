//! The GPU itself: one OpenGL context, the window surface it shows on, and
//! the shader programs Shine draws with. Shared (`Rc`) by the presenter and
//! by every texture, so the context outlives everything made on it.

use std::cell::{Cell, RefCell};
use std::num::NonZeroU32;

use glow::HasContext;
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{
    ContextApi, ContextAttributesBuilder, GlContext, NotCurrentGlContext, PossiblyCurrentContext,
    Version,
};
use glutin::display::{Display, DisplayApiPreference, GlDisplay};
use glutin::surface::{GlSurface, Surface, SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use super::atlas::Atlas;
use super::program::{
    BlitKey, BlitProgram, FillBatchProgram, TextKey, GlyphProgram, GlyphSampling,
    PresentProgram, ShapeKey, ShapeProgram, Variants,
};

pub(crate) struct Device {
    pub(crate) gl: glow::Context,
    pub(crate) present: PresentProgram,
    /// The shaders that come in variants, each made when a draw first
    /// needs it (see `program::link_variant`).
    blit_programs: Variants<BlitKey, BlitProgram>,
    shape_programs: Variants<ShapeKey, ShapeProgram>,
    glyph_programs: Variants<TextKey, GlyphProgram>,
    pub(crate) fill_batch: FillBatchProgram,
    /// OpenGL ES, not desktop OpenGL: which header the shaders get.
    es: bool,
    /// Shapes gathered to be drawn together (see `gpu::batch`).
    pub(crate) batch: super::batch::Pending,
    /// Every glyph drawn so far (see `gpu::atlas`).
    pub(crate) atlas: RefCell<Atlas>,
    /// The coverage of shapes drawn before, kept the same way: a UI draws
    /// the same buttons and cards every frame (see `render::mirror`).
    pub(crate) shapes: RefCell<Atlas>,
    /// Vertices made per draw (a line of glyphs) go up through this.
    pub(crate) stream: glow::Buffer,
    /// Sharing textures with other processes, where EGL can (see
    /// `gpu::dmabuf`).
    #[cfg(target_os = "linux")]
    pub(crate) egl: Option<super::dmabuf::Egl>,
    /// Coverage masks for shapes go up into this one texture, grown when a
    /// shape needs more room (see `gpu::fill`).
    pub(crate) mask: Cell<Option<(glow::Texture, usize, usize)>>,
    /// A unit square (0,0)–(1,1) as a triangle strip: every quad drawn is
    /// this, placed by a vertex shader.
    pub(crate) quad: glow::Buffer,
    /// The window surface's size in pixels, as it was last told.
    surface_size: Cell<(u32, u32)>,
    // Last, so they are dropped after the GL objects above are deleted.
    context: PossiblyCurrentContext,
    /// The window drawn on; none for a device that only draws into
    /// textures (the tests').
    surface: Option<Surface<WindowSurface>>,
    _display: Display,
    /// "OpenGL ES 3.1 Mesa … · D3D12 (AMD Radeon RX 6700 XT)".
    pub(crate) description: String,
}

/// The plainest variant of each shader: an image as it is, a rectangle in
/// one colour, text at a whole-pixel place, all blended over what is there.
const PLAIN_BLIT: BlitKey =
    BlitKey { mode: 0, check_inside: false, rounded: false, nearest: false, opaque: false };
const PLAIN_SHAPE: ShapeKey = ShapeKey { mode: 0, from_mask: false, hard: false, gradient: 0 };
const PLAIN_GLYPH: TextKey = TextKey { mode: 0, sampling: GlyphSampling::Exact };

impl Device {
    /// A context on `window`, `width` × `height` drawable pixels; an error
    /// names what is missing when there is no usable GPU.
    pub(crate) fn new(
        window: &(impl HasWindowHandle + HasDisplayHandle),
        width: usize,
        height: usize,
    ) -> Result<Device, String> {
        let raw_display = window
            .display_handle()
            .map_err(|e| format!("no display handle: {e}"))?
            .as_raw();
        let raw_window = window
            .window_handle()
            .map_err(|e| format!("no window handle: {e}"))?
            .as_raw();
        prefer_host_gpu();
        // SAFETY: both handles come from minifb's live window, which
        // outlives the device (the engine lets go of the GPU first).
        unsafe { Device::on(raw_display, raw_window, preference(raw_window), true, width, height) }
    }

    /// A context drawing on the bare screen through GBM: `gbm_device` and
    /// `gbm_surface` are the GBM objects the console backend made on the
    /// DRM device (see `state::console::gpu`), `width` × `height` pixels.
    ///
    /// # Safety
    ///
    /// Both pointers are live GBM objects, and outlive the device.
    #[cfg(target_os = "linux")]
    pub(crate) unsafe fn on_gbm(
        gbm_device: std::ptr::NonNull<std::ffi::c_void>,
        gbm_surface: std::ptr::NonNull<std::ffi::c_void>,
        width: usize,
        height: usize,
    ) -> Result<Device, String> {
        use raw_window_handle::{GbmDisplayHandle, GbmWindowHandle, RawDisplayHandle, RawWindowHandle};
        let display = RawDisplayHandle::Gbm(GbmDisplayHandle::new(gbm_device));
        let window = RawWindowHandle::Gbm(GbmWindowHandle::new(gbm_surface));
        // SAFETY: the caller's.
        unsafe { Device::on(display, window, DisplayApiPreference::Egl, false, width, height) }
    }

    /// A context on a native window, its surface `width` × `height`. The
    /// framebuffer has an alpha channel only when `alpha`: a GBM surface's
    /// format (XRGB) has none, and its configuration must match it.
    unsafe fn on(
        raw_display: raw_window_handle::RawDisplayHandle,
        raw_window: raw_window_handle::RawWindowHandle,
        preference: DisplayApiPreference,
        alpha: bool,
        width: usize,
        height: usize,
    ) -> Result<Device, String> {
        // SAFETY: the caller's: both handles are live.
        let display = unsafe { Display::new(raw_display, preference) }
            .map_err(|e| format!("no OpenGL display: {e}"))?;

        let mut template = ConfigTemplateBuilder::new().compatible_with_native_window(raw_window);
        if !alpha {
            template = template.with_alpha_size(0);
        }
        let template = template.build();
        // SAFETY: the template names the live window.
        let config = unsafe { display.find_configs(template) }
            .map_err(|e| format!("no OpenGL configuration: {e}"))?
            // On GBM the configuration's pixel format must be the surface's
            // (XRGB8888), or making the surface fails.
            .filter(|config| alpha || matches_xrgb(config))
            .reduce(|best, next| {
                // Fewest samples: Shine antialiases itself, so multisampling
                // would only cost memory.
                if next.num_samples() < best.num_samples() {
                    next
                } else {
                    best
                }
            })
            .ok_or("no OpenGL configuration for this window")?;

        // ES 2.0 first; desktop OpenGL 2.1 where ES isn't offered.
        let mut last_error = String::new();
        let mut not_current = None;
        for api in [
            ContextApi::Gles(Some(Version::new(2, 0))),
            ContextApi::OpenGl(Some(Version::new(2, 1))),
        ] {
            let attributes = ContextAttributesBuilder::new()
                .with_context_api(api)
                .build(Some(raw_window));
            // SAFETY: the configuration came from this display.
            match unsafe { display.create_context(&config, &attributes) } {
                Ok(context) => {
                    not_current = Some(context);
                    break;
                }
                Err(e) => last_error = e.to_string(),
            }
        }
        let not_current =
            not_current.ok_or_else(|| format!("no OpenGL context: {last_error}"))?;

        let (w, h) = nonzero(width, height);
        let attributes =
            SurfaceAttributesBuilder::<WindowSurface>::new().build(raw_window, w, h);
        // SAFETY: the window is live, and the configuration is compatible
        // with it (the template asked for that).
        let surface = unsafe { display.create_window_surface(&config, &attributes) }
            .map_err(|e| format!("no OpenGL surface: {e}"))?;
        let context = not_current
            .make_current(&surface)
            .map_err(|e| format!("cannot use the OpenGL context: {e}"))?;
        // Shine paces its own frames; waiting for the display here as well
        // would halve the rate of a loop that just misses one.
        let _ = surface.set_swap_interval(&context, SwapInterval::DontWait);
        Device::finish(display, context, Some(surface), (w.get(), h.get()))
    }

    /// A device with no window, drawing only into textures: the first EGL
    /// device the system lists that will start (the GPU, or Mesa's software
    /// renderer where there is none), with a context current on no surface.
    ///
    /// What an app inside a Saule display server draws with: it has no
    /// window of its own, only frames it hands over (see `state::client`).
    /// The tests use it too, to compare the GPU's pixels with the CPU's.
    #[cfg(target_os = "linux")]
    pub(crate) fn headless() -> Result<Device, String> {
        use glutin::api::egl;
        use glutin::config::ConfigSurfaceTypes;

        prefer_host_gpu();

        // The first device that will start: some listed ones (a GPU the
        // driver can't open in this environment) refuse to.
        let mut errors = Vec::new();
        let mut display = None;
        for device in egl::device::Device::query_devices()
            .map_err(|e| format!("no EGL devices: {e}"))?
        {
            // SAFETY: a device EGL listed, and no native display.
            match unsafe { egl::display::Display::with_device(&device, None) } {
                Ok(found) => {
                    display = Some(found);
                    break;
                }
                Err(e) => errors.push(format!("{:?}: {e}", device.drm_device_node_path())),
            }
        }
        let display =
            display.ok_or_else(|| format!("no EGL device starts ({})", errors.join("; ")))?;
        let template = ConfigTemplateBuilder::new()
            .with_surface_type(ConfigSurfaceTypes::empty())
            .build();
        // SAFETY: the template asks for nothing native.
        let config = unsafe { display.find_configs(template) }
            .map_err(|e| format!("no EGL configuration: {e}"))?
            .next()
            .ok_or("no EGL configuration")?;
        let attributes = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(Version::new(2, 0))))
            .build(None);
        // SAFETY: the configuration came from this display.
        let context = unsafe { display.create_context(&config, &attributes) }
            .map_err(|e| format!("no EGL context: {e}"))?
            .make_current_surfaceless()
            .map_err(|e| format!("cannot use the EGL context: {e}"))?;
        Device::finish(
            Display::Egl(display),
            PossiblyCurrentContext::Egl(context),
            None,
            (1, 1),
        )
    }

    /// OpenGL ES, not desktop OpenGL: which header a shader gets.
    pub(crate) fn es(&self) -> bool {
        self.es
    }

    /// The blit shader for `key`: none where the GPU cannot run that
    /// one, and the draw is the CPU's.
    pub(crate) fn blit_program(&self, key: BlitKey) -> Option<std::rc::Rc<BlitProgram>> {
        // SAFETY: GL calls on this device's current context.
        self.blit_programs.get(key, || unsafe { BlitProgram::new(&self.gl, self.es, key) })
    }

    /// The shape shader for `key`, likewise.
    pub(crate) fn shape_program(&self, key: ShapeKey) -> Option<std::rc::Rc<ShapeProgram>> {
        // SAFETY: as above.
        self.shape_programs.get(key, || unsafe { ShapeProgram::new(&self.gl, self.es, key) })
    }

    /// The glyph shader for `key`, likewise.
    pub(crate) fn glyph_program(&self, key: TextKey) -> Option<std::rc::Rc<GlyphProgram>> {
        // SAFETY: as above.
        self.glyph_programs.get(key, || unsafe { GlyphProgram::new(&self.gl, self.es, key) })
    }

    /// The programs and buffers every device has, on a current context.
    fn finish(
        display: Display,
        context: PossiblyCurrentContext,
        surface: Option<Surface<WindowSurface>>,
        size: (u32, u32),
    ) -> Result<Device, String> {
        // SAFETY: the context is current on this thread, and the loader
        // returns its entry points.
        let gl = unsafe {
            glow::Context::from_loader_function_cstr(|name| display.get_proc_address(name))
        };
        let es = matches!(context.context_api(), ContextApi::Gles(_));
        #[cfg(target_os = "linux")]
        let egl = super::dmabuf::Egl::load(&display, &context);
        #[cfg(target_os = "linux")]
        keep_driver_loaded();

        // SAFETY: GL calls on the current context.
        unsafe {
            // Every program is tried before any failure is reported: a
            // chip that cannot run one usually cannot run several, and
            // which ones, and how far over its limits each is (the driver
            // says), is what there is to go on when no such chip is at
            // hand.
            //
            // The three that come in variants are tried in their plainest
            // one: a GPU that cannot run that cannot draw a window. The
            // rest are made as draws need them.
            let present = PresentProgram::new(&gl, es);
            let blit = BlitProgram::new(&gl, es, PLAIN_BLIT);
            let shape = ShapeProgram::new(&gl, es, PLAIN_SHAPE);
            let glyph = GlyphProgram::new(&gl, es, PLAIN_GLYPH);
            let fill_batch = FillBatchProgram::new(&gl, es);
            let failed: Vec<String> = [
                ("present", present.as_ref().err()),
                ("blit", blit.as_ref().err()),
                ("shape", shape.as_ref().err()),
                ("glyph", glyph.as_ref().err()),
                ("fill", fill_batch.as_ref().err()),
            ]
            .into_iter()
            .filter_map(|(name, error)| error.map(|e| format!("{name}: {}", e.trim())))
            .collect();
            if !failed.is_empty() {
                return Err(format!(
                    "{} cannot run {} of Shine's 5 shaders ({})",
                    gl.get_parameter_string(glow::RENDERER),
                    failed.len(),
                    failed.join("; ")
                ));
            }
            let (present, fill_batch) = (present?, fill_batch?);
            let blit_programs = Variants::new();
            let shape_programs = Variants::new();
            let glyph_programs = Variants::new();
            blit_programs.get(PLAIN_BLIT, || blit);
            shape_programs.get(PLAIN_SHAPE, || shape);
            glyph_programs.get(PLAIN_GLYPH, || glyph);
            let atlas = RefCell::new(Atlas::new(&gl)?);
            let shapes = RefCell::new(Atlas::new(&gl)?);
            let stream = gl.create_buffer().map_err(|e| format!("GL buffer: {e}"))?;
            let quad = gl.create_buffer().map_err(|e| format!("GL buffer: {e}"))?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(quad));
            let corners: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes_of(&corners), glow::STATIC_DRAW);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            let description = format!(
                "{} · {}",
                gl.get_parameter_string(glow::VERSION),
                gl.get_parameter_string(glow::RENDERER)
            );
            Ok(Device {
                gl,
                present,
                blit_programs,
                shape_programs,
                glyph_programs,
                fill_batch,
                es,
                batch: Default::default(),
                atlas,
                shapes,
                stream,
                #[cfg(target_os = "linux")]
                egl,
                mask: Cell::new(None),
                quad,
                surface_size: Cell::new(size),
                context,
                surface,
                _display: display,
                description,
            })
        }
    }

    /// Draw into the window from here on: the default framebuffer, sized
    /// `win_w` × `win_h` pixels.
    pub(crate) fn bind_window(&self, win_w: usize, win_h: usize) {
        super::batch::flush(self);
        let (w, h) = nonzero(win_w, win_h);
        if let Some(surface) = &self.surface
            && (w.get(), h.get()) != self.surface_size.get()
        {
            surface.resize(&self.context, w, h);
            self.surface_size.set((w.get(), h.get()));
        }
        // The window's buffers as they really are: while a window is being
        // resized the system may have grown them before the size the window
        // reports catches up, and a viewport of the old size would leave the
        // frame in a corner of the new buffer, with black beside it. Over
        // the whole buffer, it is stretched for that frame instead.
        let (vw, vh) = self
            .surface
            .as_ref()
            .and_then(|s| s.width().zip(s.height()))
            .filter(|&(sw, sh)| sw > 0 && sh > 0)
            .unwrap_or((w.get(), h.get()));
        // SAFETY: GL calls on this device's current context.
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.gl.viewport(0, 0, vw as i32, vh as i32);
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.disable(glow::BLEND);
        }
    }

    /// Show what has been drawn into the window.
    pub(crate) fn swap(&self) -> Result<(), String> {
        // SAFETY: a GL query on the current context.
        let error = unsafe { self.gl.get_error() };
        if error != glow::NO_ERROR {
            return Err(format!("OpenGL error 0x{error:x}"));
        }
        match &self.surface {
            Some(surface) => surface
                .swap_buffers(&self.context)
                .map_err(|e| format!("cannot show the frame: {e}")),
            None => Ok(()),
        }
    }

    /// Bind the unit square to attribute 0, for a program to draw.
    pub(crate) fn bind_quad(&self) {
        // SAFETY: GL calls on the current context.
        unsafe {
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.quad));
            self.gl.enable_vertex_attrib_array(0);
            self.gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the objects were made on this context, still current.
        unsafe {
            self.gl.delete_buffer(self.quad);
            self.gl.delete_program(self.present.program);
            for program in self.blit_programs.programs() {
                self.gl.delete_program(program.program);
            }
            for program in self.shape_programs.programs() {
                self.gl.delete_program(program.program);
            }
            for program in self.glyph_programs.programs() {
                self.gl.delete_program(program.program);
            }
            self.gl.delete_program(self.fill_batch.program);
            self.gl.delete_buffer(self.stream);
            self.atlas.borrow().delete(&self.gl);
            self.shapes.borrow().delete(&self.gl);
            if let Some((mask, _, _)) = self.mask.take() {
                self.gl.delete_texture(mask);
            }
        }
    }
}

/// Under WSL the GPU is Windows', reached through Mesa's D3D12 driver
/// (`/dev/dxg`), but Mesa doesn't pick that driver by itself on WSLg's X
/// server and draws on the CPU instead (llvmpipe), which is slower than not
/// using OpenGL at all. Asking for it by name, before Mesa loads, gets the
/// real card. A `GALLIUM_DRIVER` already set is left as it is.
fn prefer_host_gpu() {
    #[cfg(target_os = "linux")]
    if std::path::Path::new("/dev/dxg").exists() && std::env::var_os("GALLIUM_DRIVER").is_none() {
        // SAFETY: set once, on the thread that draws, before any GL library
        // is loaded; nothing else in Shine reads or writes the environment
        // while a window opens.
        unsafe { std::env::set_var("GALLIUM_DRIVER", "d3d12") };
    }
}

/// Which window-system interfaces to try, best first, for this platform.
#[allow(unused_variables)]
fn preference(window: raw_window_handle::RawWindowHandle) -> DisplayApiPreference {
    #[cfg(target_os = "macos")]
    return DisplayApiPreference::Cgl;
    #[cfg(target_os = "windows")]
    return DisplayApiPreference::WglThenEgl(Some(window));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    // GLX first: under WSLg, EGL on X11 can't reach the GPU ("DRI3 error")
    // where GLX can. GLX reports errors through Xlib's process-wide
    // handler; glutin's is only needed for GLX calls that fail, which a
    // failed context creation reports through its result anyway.
    return DisplayApiPreference::GlxThenEgl(Box::new(|_| ()));
}

/// A size for glutin, which has no zero-sized surfaces.
pub(crate) fn nonzero(w: usize, h: usize) -> (NonZeroU32, NonZeroU32) {
    let clamp = |v: usize| NonZeroU32::new(v.clamp(1, u32::MAX as usize) as u32).unwrap();
    (clamp(w), clamp(h))
}

/// A slice of plain numbers as its bytes.
pub(crate) fn bytes_of<T: Copy>(items: &[T]) -> &[u8] {
    // SAFETY: `T` is a plain number type here (u32, f32): every byte of it
    // is initialised, and u8 has no alignment requirement.
    unsafe { std::slice::from_raw_parts(items.as_ptr().cast::<u8>(), std::mem::size_of_val(items)) }
}

/// The same, writable: what `glReadPixels` fills.
pub(crate) fn bytes_of_mut(items: &mut [u32]) -> &mut [u8] {
    // SAFETY: every bit pattern is a valid u32, so writing bytes into one is
    // sound, and u8 has no alignment requirement.
    unsafe {
        std::slice::from_raw_parts_mut(items.as_mut_ptr().cast::<u8>(), std::mem::size_of_val(items))
    }
}

/// Whether a configuration's pixels are XRGB8888, the format of the GBM
/// surface the console draws on.
fn matches_xrgb(config: &glutin::config::Config) -> bool {
    #[cfg(target_os = "linux")]
    if let glutin::config::Config::Egl(config) = config {
        return config.native_visual() == u32::from_le_bytes(*b"XR24");
    }
    let _ = config;
    true
}

/// Keep every library loaded so far — the driver the context just brought
/// in among them — loaded until the process ends.
///
/// Some drivers unload part of themselves when their last context goes, and
/// leave behind a destructor for each thread they ran on: the thread that
/// drew then crashes as it ends, inside code no longer there. WSL's D3D12
/// driver does this (`libd3d12core.so` and the GPU maker's own library), so
/// a Saule program drawing on the GPU under WSL died at its end with a
/// segmentation fault. Pinned, the libraries stay, and their destructors
/// find their code. They would be in memory to the end anyway, nearly
/// always; this only takes away the unloading.
#[cfg(target_os = "linux")]
fn keep_driver_loaded() {
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return;
    };
    let mut seen = std::collections::HashSet::new();
    for path in maps.lines().filter_map(|line| line.split_whitespace().nth(5)) {
        if !path.starts_with('/') || !path.contains(".so") || !seen.insert(path) {
            continue;
        }
        let Ok(name) = std::ffi::CString::new(path) else {
            continue;
        };
        // SAFETY: RTLD_NOLOAD only finds a library already loaded (nothing
        // new runs), and RTLD_NODELETE marks it never to be unloaded; the
        // handle is dropped at once, the mark stays.
        unsafe {
            let handle = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD | libc::RTLD_NODELETE);
            if !handle.is_null() {
                libc::dlclose(handle);
            }
        }
    }
}
