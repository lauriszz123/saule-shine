//! A surface's twin on the GPU: a texture holding its pixels, and the
//! framebuffer that lets the GPU draw into it.

use std::cell::Cell;
use std::rc::Rc;

use glow::HasContext;

use super::device::{bytes_of, bytes_of_mut, Device};

pub(crate) struct Texture {
    pub(crate) dev: Rc<Device>,
    pub(crate) texture: glow::Texture,
    /// Made the first time something is drawn into the texture.
    framebuffer: Cell<Option<glow::Framebuffer>>,
    pub(crate) w: usize,
    pub(crate) h: usize,
    /// The EGLImage the texture is bound to, for one made over another
    /// process's buffer (`gpu::dmabuf::import`): destroyed with it.
    image: Option<*const std::ffi::c_void>,
}

impl Texture {
    /// A `w` × `h` texture, its pixels undefined until written.
    pub(crate) fn new(dev: &Rc<Device>, w: usize, h: usize) -> Result<Texture, String> {
        let gl = &dev.gl;
        // SAFETY: GL calls on the device's current context.
        unsafe {
            let texture = gl.create_texture().map_err(|e| format!("GL texture: {e}"))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            // Linear always: the blit shader picks texel centres itself
            // where Shine samples nearest, and those come back exact.
            // Clamping is what lets a texture of any size work on ES 2.0.
            for (name, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32);
            }
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                w as i32,
                h as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            Ok(Texture::wrap(dev, texture, w, h, None))
        }
    }

    /// A texture already made on `dev`, `w` × `h`, owned from here on.
    pub(crate) fn wrap(
        dev: &Rc<Device>,
        texture: glow::Texture,
        w: usize,
        h: usize,
        image: Option<*const std::ffi::c_void>,
    ) -> Texture {
        Texture {
            dev: Rc::clone(dev),
            texture,
            framebuffer: Cell::new(None),
            w,
            h,
            image,
        }
    }

    /// Copy every pixel of `src`, which is this texture's size, into this
    /// one, on the GPU.
    pub(crate) fn copy_from(&self, src: &Texture) {
        super::batch::flush(&self.dev);
        src.bind_target();
        let gl = &self.dev.gl;
        // SAFETY: GL calls on the current context, `src`'s framebuffer
        // bound to read from.
        unsafe {
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.copy_tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                0,
                0,
                self.w.min(src.w) as i32,
                self.h.min(src.h) as i32,
            );
        }
    }

    /// Copy `w` × `h` pixels of `src` from (`sx`, `sy`) to this texture's
    /// corner, on the GPU.
    pub(crate) fn copy_region_from(&self, src: &Texture, sx: usize, sy: usize, w: usize, h: usize) {
        src.bind_target();
        let gl = &self.dev.gl;
        // SAFETY: GL calls on the current context, `src`'s framebuffer
        // bound to read from; the region is inside both (the caller's).
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.copy_tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, sx as i32, sy as i32, w as i32, h as i32);
        }
    }

    /// Replace every pixel with `buf`'s (`w * h` of them, `0xAARRGGBB`).
    pub(crate) fn upload(&self, buf: &[u32]) {
        super::batch::flush(&self.dev);
        let gl = &self.dev.gl;
        // SAFETY: GL calls on the current context; `buf` is the texture's
        // size (the caller's surface is).
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                self.w as i32,
                self.h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(bytes_of(&buf[..self.w * self.h]))),
            );
        }
    }

    /// Replace every pixel with `bytes`, four to a pixel in memory order
    /// (B, G, R, A), at least `w * h * 4` of them.
    pub(crate) fn upload_bytes(&self, bytes: &[u8]) {
        super::batch::flush(&self.dev);
        let gl = &self.dev.gl;
        // SAFETY: GL calls on the current context; the caller passes a
        // frame of the texture's size, checked here.
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                self.w as i32,
                self.h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&bytes[..self.w * self.h * 4])),
            );
        }
    }

    /// Copy every pixel back into `buf`, which is the texture's size.
    pub(crate) fn read(&self, buf: &mut [u32]) {
        super::batch::flush(&self.dev);
        self.bind_target();
        let gl = &self.dev.gl;
        // SAFETY: GL calls on the current context, with this texture's
        // framebuffer bound; `buf` holds `w * h` pixels.
        unsafe {
            gl.read_pixels(
                0,
                0,
                self.w as i32,
                self.h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(bytes_of_mut(&mut buf[..self.w * self.h]))),
            );
        }
    }

    /// Copy the `w` × `h` pixels at the texture's corner back into `buf`:
    /// all of it, for a texture that size, or the frame in a buffer made
    /// larger than its window.
    pub(crate) fn read_corner(&self, buf: &mut [u32], w: usize, h: usize) {
        let (w, h) = (w.min(self.w), h.min(self.h));
        if (w, h) == (self.w, self.h) {
            return self.read(buf);
        }
        super::batch::flush(&self.dev);
        self.bind_target();
        let gl = &self.dev.gl;
        // SAFETY: GL calls on the current context, with this texture's
        // framebuffer bound; the region is inside it and `buf` holds
        // `w * h` pixels (sliced here).
        unsafe {
            gl.read_pixels(
                0,
                0,
                w as i32,
                h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(bytes_of_mut(&mut buf[..w * h]))),
            );
        }
    }

    /// Copy every pixel back into `bytes`, four to a pixel in memory order,
    /// at least `w * h * 4` of them: a frame an app hands over.
    pub(crate) fn read_bytes(&self, bytes: &mut [u8]) {
        super::batch::flush(&self.dev);
        self.bind_target();
        let gl = &self.dev.gl;
        // SAFETY: GL calls on the current context, with this texture's
        // framebuffer bound; `bytes` is large enough (sliced here).
        unsafe {
            gl.read_pixels(
                0,
                0,
                self.w as i32,
                self.h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut bytes[..self.w * self.h * 4])),
            );
        }
    }

    /// This texture's framebuffer, made the first time it is asked for.
    pub(crate) fn framebuffer(&self) -> glow::Framebuffer {
        if let Some(framebuffer) = self.framebuffer.get() {
            return framebuffer;
        }
        let gl = &self.dev.gl;
        // SAFETY: GL calls on the current context.
        unsafe {
            let framebuffer = gl
                .create_framebuffer()
                .expect("a framebuffer object is always available");
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(self.texture),
                0,
            );
            self.framebuffer.set(Some(framebuffer));
            framebuffer
        }
    }

    /// Draw into this texture from here on, its pixels for the viewport.
    pub(crate) fn bind_target(&self) {
        super::batch::flush(&self.dev);
        let framebuffer = self.framebuffer();
        let gl = &self.dev.gl;
        // SAFETY: GL calls on the current context.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.viewport(0, 0, self.w as i32, self.h as i32);
        }
    }
}

impl Drop for Texture {
    fn drop(&mut self) {
        // Shapes gathered for this texture are drawn before it goes.
        super::batch::flush(&self.dev);
        let gl = &self.dev.gl;
        // SAFETY: made on this device's context, which the `Rc` keeps alive.
        unsafe {
            if let Some(framebuffer) = self.framebuffer.take() {
                gl.delete_framebuffer(framebuffer);
            }
            gl.delete_texture(self.texture);
        }
        #[cfg(target_os = "linux")]
        if let (Some(image), Some(egl)) = (self.image, self.dev.egl.as_ref()) {
            egl.destroy(image);
        }
    }
}
