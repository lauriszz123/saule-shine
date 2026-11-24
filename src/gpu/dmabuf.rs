//! Sharing textures between processes, as Linux dma-bufs: how an app inside
//! a Saule display server hands its frames over without a copy (see
//! `state::client` and `compositor`).
//!
//! The app makes a texture into an EGLImage and exports it
//! (`EGL_MESA_image_dma_buf_export`): a file descriptor, and how its pixels
//! are laid out. The server imports that (`EGL_EXT_image_dma_buf_import`)
//! as an EGLImage again and binds a texture to it
//! (`GL_OES_EGL_image`). Both textures are then the same memory on the GPU:
//! what the app draws, the server samples.
//!
//! EGL only (a context made through EGL, as every Linux one here is), and
//! only where the driver offers both extensions; [`Egl::load`] says when
//! not, and frames go through shared memory as before.

use std::ffi::c_void;
use std::os::fd::{FromRawFd, OwnedFd};

use glow::HasContext;
use glutin::context::AsRawContext;
use glutin::display::{AsRawDisplay, GlDisplay};

use super::device::Device;
use super::texture::Texture;

/// One shared buffer, as it crosses between processes.
pub(crate) struct DmaBuf {
    pub(crate) fd: OwnedFd,
    pub(crate) fourcc: u32,
    pub(crate) stride: u32,
    pub(crate) offset: u32,
    pub(crate) modifier: u64,
}

type Ptr = *const c_void;

/// The EGL calls sharing needs, looked up on a device's display.
pub(crate) struct Egl {
    display: Ptr,
    context: Ptr,
    create_image: unsafe extern "C" fn(Ptr, Ptr, u32, Ptr, *const i32) -> Ptr,
    destroy_image: unsafe extern "C" fn(Ptr, Ptr) -> u32,
    export_query: Option<unsafe extern "C" fn(Ptr, Ptr, *mut i32, *mut i32, *mut u64) -> u32>,
    export: Option<unsafe extern "C" fn(Ptr, Ptr, *mut i32, *mut i32, *mut i32) -> u32>,
    target_texture: unsafe extern "C" fn(u32, Ptr),
}

const EGL_NO_CONTEXT: Ptr = std::ptr::null();
const EGL_GL_TEXTURE_2D: u32 = 0x30B1;
const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: i32 = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: i32 = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: i32 = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: i32 = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: i32 = 0x3444;
const EGL_NONE: i32 = 0x3038;
/// `DRM_FORMAT_MOD_INVALID`: no modifier said, the driver's own layout.
const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

impl Egl {
    /// The calls, if the device's display is EGL and has them. Exporting
    /// may still be missing (`can_export`): a server only needs to import.
    pub(crate) fn load(
        display: &glutin::display::Display,
        context: &glutin::context::PossiblyCurrentContext,
    ) -> Option<Egl> {
        let glutin::display::RawDisplay::Egl(raw_display) = display.raw_display() else {
            return None;
        };
        let glutin::context::RawContext::Egl(raw_context) = context.raw_context() else {
            return None;
        };
        let lookup = |name: &std::ffi::CStr| {
            let f = display.get_proc_address(name);
            (!f.is_null()).then_some(f)
        };
        // SAFETY: each pointer is the named EGL/GL entry point, transmuted
        // to its documented signature.
        unsafe {
            Some(Egl {
                display: raw_display,
                context: raw_context,
                create_image: std::mem::transmute::<Ptr, _>(
                    lookup(c"eglCreateImageKHR").or_else(|| lookup(c"eglCreateImage"))?,
                ),
                destroy_image: std::mem::transmute::<Ptr, _>(
                    lookup(c"eglDestroyImageKHR").or_else(|| lookup(c"eglDestroyImage"))?,
                ),
                export_query: lookup(c"eglExportDMABUFImageQueryMESA").map(|f| std::mem::transmute(f)),
                export: lookup(c"eglExportDMABUFImageMESA").map(|f| std::mem::transmute(f)),
                target_texture: std::mem::transmute::<Ptr, _>(
                    lookup(c"glEGLImageTargetTexture2DOES")?,
                ),
            })
        }
    }

    pub(crate) fn can_export(&self) -> bool {
        self.export_query.is_some() && self.export.is_some()
    }

    pub(crate) fn destroy(&self, image: Ptr) {
        // SAFETY: an image made on this display.
        unsafe { (self.destroy_image)(self.display, image) };
    }
}

/// Share `texture` (made on `dev`) as a dma-buf: `None` where the driver
/// can't.
pub(crate) fn export(dev: &Device, texture: &Texture) -> Option<DmaBuf> {
    let egl = dev.egl.as_ref()?;
    let (query, export) = (egl.export_query?, egl.export?);
    // SAFETY: EGL calls on this device's display and current context; the
    // texture is one of this context's, named by its GL name.
    unsafe {
        let name = texture.texture.0.get() as usize as Ptr;
        let none = [EGL_NONE];
        let image = (egl.create_image)(egl.display, egl.context, EGL_GL_TEXTURE_2D, name, none.as_ptr());
        if image.is_null() {
            return None;
        }
        let (mut fourcc, mut planes, mut modifier) = (0i32, 0i32, 0u64);
        let (mut fd, mut stride, mut offset) = (-1i32, 0i32, 0i32);
        let ok = query(egl.display, image, &mut fourcc, &mut planes, &mut modifier) != 0
            && planes == 1
            && export(egl.display, image, &mut fd, &mut stride, &mut offset) != 0
            && fd >= 0;
        // The buffer outlives the image: the fd holds it.
        egl.destroy(image);
        if !ok {
            if fd >= 0 {
                drop(OwnedFd::from_raw_fd(fd));
            }
            return None;
        }
        Some(DmaBuf {
            fd: OwnedFd::from_raw_fd(fd),
            fourcc: fourcc as u32,
            stride: stride as u32,
            offset: offset as u32,
            modifier,
        })
    }
}

/// A texture on `dev` over a buffer another process shared, `w` × `h`.
pub(crate) fn import(dev: &std::rc::Rc<Device>, buf: &DmaBuf, w: usize, h: usize) -> Result<Texture, String> {
    use std::os::fd::AsRawFd;
    let egl = dev.egl.as_ref().ok_or("no EGL to import with")?;
    let mut attributes = vec![
        EGL_WIDTH,
        w as i32,
        EGL_HEIGHT,
        h as i32,
        EGL_LINUX_DRM_FOURCC_EXT,
        buf.fourcc as i32,
        EGL_DMA_BUF_PLANE0_FD_EXT,
        buf.fd.as_raw_fd(),
        EGL_DMA_BUF_PLANE0_OFFSET_EXT,
        buf.offset as i32,
        EGL_DMA_BUF_PLANE0_PITCH_EXT,
        buf.stride as i32,
    ];
    if buf.modifier != MOD_INVALID {
        attributes.extend_from_slice(&[
            EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
            buf.modifier as u32 as i32,
            EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
            (buf.modifier >> 32) as u32 as i32,
        ]);
    }
    attributes.push(EGL_NONE);
    // SAFETY: EGL and GL calls on this device's display and current context;
    // the attribute list ends with EGL_NONE.
    unsafe {
        let image = (egl.create_image)(
            egl.display,
            EGL_NO_CONTEXT,
            EGL_LINUX_DMA_BUF_EXT,
            std::ptr::null(),
            attributes.as_ptr(),
        );
        if image.is_null() {
            return Err("the GPU won't take the app's buffer".into());
        }
        let gl = &dev.gl;
        let texture = gl.create_texture().map_err(|e| format!("GL texture: {e}"))?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        for (name, value) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32);
        }
        (egl.target_texture)(glow::TEXTURE_2D, image);
        if gl.get_error() != glow::NO_ERROR {
            gl.delete_texture(texture);
            egl.destroy(image);
            return Err("the GPU won't make a texture of the app's buffer".into());
        }
        Ok(Texture::wrap(dev, texture, w, h, Some(image)))
    }
}
