//! Drawing on the bare screen through the GPU: OpenGL on GBM.
//!
//! The renderer draws the frame into the screen's texture on the GPU (see
//! `render::mirror`); here it is drawn once more onto a GBM surface — the
//! window, on a machine with no windows — whose buffers the display
//! controller can show. After the buffers swap, the one just finished is
//! locked, given a DRM framebuffer (once per buffer) and flipped to, the way
//! the dumb buffers are flipped when the CPU draws. No pixel of the frame
//! passes through the CPU.
//!
//! GBM is Mesa's, loaded at run time like OpenGL itself: a system without it
//! (an image with no driver pack) never gets here and draws on the CPU.

use std::collections::HashMap;
use std::ffi::c_void;
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use std::rc::Rc;

use libloading::Library;

use super::display::Display;
use crate::gpu::{self, Device, Texture};

/// `GBM_FORMAT_XRGB8888`: the screen's own format, as the dumb buffers use.
const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
/// `GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING`.
const SCANOUT_AND_RENDERING: u32 = 1 | (1 << 2);

/// The few GBM entry points used, looked up in `libgbm.so.1`.
struct Gbm {
    create_device: unsafe extern "C" fn(i32) -> *mut c_void,
    device_destroy: unsafe extern "C" fn(*mut c_void),
    surface_create: unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32) -> *mut c_void,
    surface_destroy: unsafe extern "C" fn(*mut c_void),
    lock_front_buffer: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    release_buffer: unsafe extern "C" fn(*mut c_void, *mut c_void),
    bo_get_handle: unsafe extern "C" fn(*mut c_void) -> u64,
    bo_get_stride: unsafe extern "C" fn(*mut c_void) -> u32,
    _library: Library,
}

impl Gbm {
    fn load() -> Result<Gbm, String> {
        // SAFETY: loading Mesa's GBM, whose initialisers have no
        // preconditions; each symbol is GBM's documented C signature.
        unsafe {
            let library = Library::new("libgbm.so.1").map_err(|e| format!("no GBM: {e}"))?;
            macro_rules! sym {
                ($name:literal) => {
                    *library
                        .get($name)
                        .map_err(|e| format!("GBM lacks {}: {e}", String::from_utf8_lossy($name)))?
                };
            }
            Ok(Gbm {
                create_device: sym!(b"gbm_create_device"),
                device_destroy: sym!(b"gbm_device_destroy"),
                surface_create: sym!(b"gbm_surface_create"),
                surface_destroy: sym!(b"gbm_surface_destroy"),
                lock_front_buffer: sym!(b"gbm_surface_lock_front_buffer"),
                release_buffer: sym!(b"gbm_surface_release_buffer"),
                bo_get_handle: sym!(b"gbm_bo_get_handle"),
                bo_get_stride: sym!(b"gbm_bo_get_stride"),
                _library: library,
            })
        }
    }
}

pub(super) struct ConsoleGpu {
    // The device first, so the context goes before the GBM objects it was
    // made on.
    dev: Option<Rc<Device>>,
    gbm: Gbm,
    device: NonNull<c_void>,
    surface: NonNull<c_void>,
    width: usize,
    height: usize,
    /// A DRM framebuffer for each GBM buffer shown so far, by its handle:
    /// a surface has only a few buffers, which come round again.
    framebuffers: HashMap<u32, drm::control::framebuffer::Handle>,
    /// The buffer on screen, and the one waiting to be (flipped to, its
    /// flip not yet landed): each released back to GBM once the next is
    /// showing.
    shown: Option<NonNull<c_void>>,
    pending: Option<NonNull<c_void>>,
}

impl ConsoleGpu {
    /// OpenGL on `display`'s DRM device, at its size.
    pub(super) fn new(display: &Display) -> Result<ConsoleGpu, String> {
        if let Some(why) = gpu::turned_off() {
            return Err(why.into());
        }
        let gbm = Gbm::load()?;
        let (width, height) = display.size();
        // SAFETY: GBM calls with the card's open descriptor, which outlives
        // the GBM device (the console drops this before the display).
        unsafe {
            let device = NonNull::new((gbm.create_device)(display.card().as_raw_fd()))
                .ok_or("GBM can't use the screen's device")?;
            let surface = (gbm.surface_create)(
                device.as_ptr(),
                width as u32,
                height as u32,
                XRGB8888,
                SCANOUT_AND_RENDERING,
            );
            let Some(surface) = NonNull::new(surface) else {
                (gbm.device_destroy)(device.as_ptr());
                return Err("GBM can't make a screen-sized surface".into());
            };
            match Device::on_gbm(device, surface, width, height) {
                Ok(dev) => Ok(ConsoleGpu {
                    dev: Some(Rc::new(dev)),
                    gbm,
                    device,
                    surface,
                    width,
                    height,
                    framebuffers: HashMap::new(),
                    shown: None,
                    pending: None,
                }),
                Err(e) => {
                    (gbm.surface_destroy)(surface.as_ptr());
                    (gbm.device_destroy)(device.as_ptr());
                    Err(e)
                }
            }
        }
    }

    /// The GPU, for the renderer to draw on.
    pub(super) fn device(&self) -> Option<Rc<Device>> {
        self.dev.clone()
    }

    /// Put `texture` (the screen the renderer drew) on the screen.
    pub(super) fn present(&mut self, display: &mut Display, texture: &Texture) -> Result<(), String> {
        let dev = self.dev.as_ref().ok_or("the GPU is gone")?;
        gpu::show(dev, texture, self.width, self.height)?;

        // SAFETY: GBM calls on this console's surface, after the swap gave
        // it a finished buffer.
        let buffer = unsafe { (self.gbm.lock_front_buffer)(self.surface.as_ptr()) };
        let buffer = NonNull::new(buffer).ok_or("GBM gave no finished buffer")?;
        // SAFETY: `buffer` is a live GBM buffer.
        let (handle, stride) = unsafe {
            (
                (self.gbm.bo_get_handle)(buffer.as_ptr()) as u32,
                (self.gbm.bo_get_stride)(buffer.as_ptr()),
            )
        };
        let framebuffer = match self.framebuffers.get(&handle) {
            Some(&fb) => fb,
            None => {
                let fb = display
                    .add_framebuffer(handle, stride, self.width as u32, self.height as u32)
                    .map_err(|e| format!("the screen won't show the GPU's buffer: {e}"))?;
                self.framebuffers.insert(handle, fb);
                fb
            }
        };

        // The last flip landed (the display waits for it), so the buffer
        // shown before it is free again.
        display.flip_to(framebuffer);
        if let Some(old) = self.shown.take() {
            // SAFETY: a buffer this surface gave out, no longer on screen.
            unsafe { (self.gbm.release_buffer)(self.surface.as_ptr(), old.as_ptr()) };
        }
        self.shown = self.pending.take();
        self.pending = Some(buffer);
        Ok(())
    }
}

impl Drop for ConsoleGpu {
    fn drop(&mut self) {
        // SAFETY: the buffers came from this surface, which came from this
        // device; nothing uses them after this.
        unsafe {
            for buffer in [self.shown.take(), self.pending.take()].into_iter().flatten() {
                (self.gbm.release_buffer)(self.surface.as_ptr(), buffer.as_ptr());
            }
        }
        // The context goes before the surface it draws on. The renderer's
        // textures, which share it, are gone by now (the engine lets go of
        // them first), so this is the last of it.
        self.dev = None;
        // SAFETY: the GBM objects this console made, destroyed last.
        unsafe {
            (self.gbm.surface_destroy)(self.surface.as_ptr());
            (self.gbm.device_destroy)(self.device.as_ptr());
        }
    }
}
