//! Putting a frame on the window through the GPU: one drawn on the CPU,
//! uploaded first, or one the GPU drew itself (the screen's texture).

use std::rc::Rc;

use glow::HasContext;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use super::device::Device;
use super::texture::Texture;

pub(crate) struct Presenter {
    /// Where CPU-drawn frames are uploaded; made at the first one, and
    /// again when the frame changes size.
    frame: Option<Texture>,
    pub(crate) dev: Rc<Device>,
}

impl Presenter {
    /// A context on `window`, `width` × `height` drawable pixels.
    pub(crate) fn new(
        window: &(impl HasWindowHandle + HasDisplayHandle),
        width: usize,
        height: usize,
    ) -> Result<Presenter, String> {
        Ok(Presenter {
            frame: None,
            dev: Rc::new(Device::new(window, width, height)?),
        })
    }

    /// Put `buf` (`w` × `h` pixels, `0xAARRGGBB`) over the whole window,
    /// which is `win_w` × `win_h` drawable pixels.
    pub(crate) fn present(
        &mut self,
        buf: &[u32],
        w: usize,
        h: usize,
        win_w: usize,
        win_h: usize,
    ) -> Result<(), String> {
        if w == 0 || h == 0 || buf.len() < w * h {
            return Ok(());
        }
        let fits = self.frame.as_ref().is_some_and(|t| (t.w, t.h) == (w, h));
        if !fits {
            self.frame = Some(Texture::new(&self.dev, w, h)?);
        }
        let frame = self.frame.as_ref().expect("made above");
        frame.upload(buf);
        show(&self.dev, frame, win_w, win_h)
    }

    /// Put a texture the GPU drew over the whole window.
    pub(crate) fn present_texture(
        &mut self,
        texture: &Texture,
        win_w: usize,
        win_h: usize,
    ) -> Result<(), String> {
        show(&self.dev, texture, win_w, win_h)
    }
}

pub(crate) fn show(dev: &Device, texture: &Texture, win_w: usize, win_h: usize) -> Result<(), String> {
    dev.bind_window(win_w, win_h);
    let gl = &dev.gl;
    // SAFETY: GL calls on the device's current context.
    unsafe {
        gl.use_program(Some(dev.present.program));
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture.texture));
        dev.bind_quad();
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
    }
    dev.swap()
}
