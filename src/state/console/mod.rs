//! The console backend: Shine on the bare screen of a Linux machine with no
//! desktop — Saule Linux, where the Saule program is the display.
//!
//! The "window" is the whole screen, at the screen's own resolution whatever
//! `Window.create` asked for; apps see the real size in `Window.getSize` and
//! a `Resized` event, as they would after a resize on the desktop. Frames go
//! to the display through DRM ([`display`]), input comes from evdev
//! ([`input`]), and the mouse cursor is the hardware's own where the driver
//! has one and drawn into the frames where not ([`cursor`]).

mod cursor;
mod display;
mod gpu;
mod input;
mod keymap;
mod touchpad;
mod vt;

use minifb::{CursorStyle, Key};

use super::backend::Backend;
use display::Display;
use input::Input;
use vt::Vt;

/// Whether `Window.create` should take the bare screen rather than open a
/// desktop window.
///
/// `SHINE_BACKEND=console` or `desktop` decides outright. Otherwise: yes when
/// there is no desktop session to open a window in (neither `DISPLAY` nor
/// `WAYLAND_DISPLAY`) and there is a DRM device to draw on.
pub(super) fn wanted() -> bool {
    match std::env::var("SHINE_BACKEND").as_deref() {
        Ok("console") => return true,
        Ok("desktop") => return false,
        _ => {}
    }
    let desktop = ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()));
    !desktop && std::path::Path::new("/dev/dri/card0").exists()
}

pub(super) struct Console {
    /// Drawing on the screen through the GPU, where there is one and the
    /// driver shows the cursor itself (see `gpu`). First, so it goes before
    /// the display whose device it draws on.
    gpu: Option<gpu::ConsoleGpu>,
    input: Input,
    display: Display,
    /// Dropped last: the terminal comes back once the screen is let go.
    _vt: Option<Vt>,
    cursor: CursorStyle,
    cursor_visible: bool,
    /// The driver shows the cursor itself.
    hardware_cursor: bool,
    /// Where the cursor was last shown, and as what: a cursor drawn into the
    /// frames needs a new frame when either changes.
    shown: Option<((i32, i32), CursorStyle)>,
}

impl Console {
    pub(super) fn open() -> Result<Console, String> {
        let vt = Vt::take();
        let display = Display::open().map_err(|e| format!("Window.create: {e}"))?;
        let input = Input::open(display.size());
        let mut console = Console {
            gpu: None,
            input,
            display,
            _vt: vt,
            cursor: CursorStyle::Arrow,
            cursor_visible: true,
            hardware_cursor: false,
            shown: None,
        };
        console.upload_cursor();
        // The GPU only with the hardware cursor: a cursor drawn into the
        // frames is drawn by the CPU, into the CPU's copy.
        if console.hardware_cursor {
            match gpu::ConsoleGpu::new(&console.display) {
                // OpenGL with no GPU behind it: the CPU draws faster by
                // Shine's own code than by Mesa's.
                Ok(gpu)
                    if !crate::gpu::drawing_forced()
                        && gpu
                            .device()
                            .is_some_and(|dev| crate::gpu::software_renderer(&dev.description)) =>
                {
                    let description = gpu.device().map(|dev| dev.description.clone());
                    crate::gpu::set_problem(&format!(
                        "no driver took the graphics chip, only Mesa's software OpenGL ({})",
                        description.unwrap_or_default()
                    ));
                    crate::gpu::children_draw_on_cpu();
                }
                Ok(gpu) => {
                    if let Some(dev) = gpu.device() {
                        crate::gpu::set_renderer(&dev.description);
                        if crate::gpu::tracing() {
                            eprintln!("shine: drawing on the GPU: {}", dev.description);
                        }
                    }
                    console.gpu = Some(gpu);
                }
                Err(e) => {
                    crate::gpu::set_problem(&e);
                    crate::gpu::children_draw_on_cpu();
                }
            }
        } else {
            crate::gpu::set_problem("the screen's controller has no cursor of its own");
        }
        Ok(console)
    }

    fn pointer(&self) -> (i32, i32) {
        (self.input.pos.0 as i32, self.input.pos.1 as i32)
    }

    /// Give the hardware cursor the current image, or find out there is no
    /// hardware cursor.
    fn upload_cursor(&mut self) {
        let shape = cursor::shape(self.cursor);
        self.hardware_cursor = self
            .display
            .set_cursor_image(&shape.argb(display::CURSOR_SIZE));
        if self.hardware_cursor {
            if self.cursor_visible {
                self.move_hardware_cursor();
            } else {
                self.display.hide_cursor();
            }
        }
        if std::env::var_os("SHINE_TRACE_PRESENT").is_some() {
            let how = if self.hardware_cursor {
                "hardware"
            } else {
                "drawn into the frames"
            };
            eprintln!("shine: mouse cursor: {how}");
        }
    }

    fn move_hardware_cursor(&mut self) {
        let (x, y) = self.pointer();
        let hot = cursor::shape(self.cursor).hotspot;
        self.display.move_cursor(x - hot.0, y - hot.1);
    }
}

impl Backend for Console {
    fn is_open(&self) -> bool {
        true
    }

    fn size(&self) -> (usize, usize) {
        self.display.size()
    }

    fn scale(&self) -> f64 {
        crate::window::screen_scale(self.display.size().1)
    }

    fn backing_scale(&self) -> f64 {
        1.0
    }

    fn pump(&mut self) {
        let before = self.pointer();
        self.input.pump();
        if self.hardware_cursor && self.cursor_visible && self.pointer() != before {
            self.move_hardware_cursor();
        }
    }

    fn present(&mut self, buf: &[u32], w: usize, h: usize) -> Result<(), String> {
        self.pump();
        let drawn = (!self.hardware_cursor && self.cursor_visible)
            .then(|| (self.pointer(), self.cursor));
        self.display.present(buf, w, h, |frame, pitch, sw, sh| {
            if let Some(((x, y), style)) = drawn {
                cursor::shape(style).draw(frame, pitch, sw, sh, x, y);
            }
        });
        self.shown = drawn;
        Ok(())
    }

    fn gpu_device(&self) -> Option<std::rc::Rc<crate::gpu::Device>> {
        self.gpu.as_ref().and_then(|g| g.device())
    }

    fn present_texture(&mut self, texture: &crate::gpu::Texture) -> Result<bool, String> {
        self.pump();
        let Some(gpu) = self.gpu.as_mut() else {
            return Ok(false);
        };
        if let Err(e) = gpu.present(&mut self.display, texture) {
            if crate::gpu::tracing() {
                eprintln!("shine: the GPU failed ({e}); drawing on the CPU");
            }
            self.gpu = None;
            return Ok(false);
        }
        Ok(true)
    }

    fn wants_present(&self) -> bool {
        let wanted = (!self.hardware_cursor && self.cursor_visible)
            .then(|| (self.pointer(), self.cursor));
        wanted != self.shown
    }

    fn keys_down(&self) -> Vec<Key> {
        self.input.held().to_vec()
    }

    fn is_key_down(&self, key: Key) -> bool {
        self.input.held().contains(&key)
    }

    fn mouse_pos(&self) -> Option<(f64, f64)> {
        Some(self.input.pos)
    }

    fn mouse_pos_clamped(&self) -> (f64, f64) {
        self.input.pos
    }

    fn mouse_buttons(&self) -> [bool; 3] {
        self.input.buttons.shown()
    }

    fn wheel(&self) -> (f64, f64) {
        self.input.wheel
    }

    fn is_active(&mut self) -> bool {
        true
    }

    // The screen has no title, no position and nothing to be on top of.
    fn set_title(&mut self, _title: &str) {}

    fn position(&self) -> (i64, i64) {
        (0, 0)
    }

    fn set_position(&mut self, _x: i64, _y: i64) {}

    fn set_topmost(&mut self, _topmost: bool) {}

    fn set_cursor(&mut self, cursor: CursorStyle) {
        if cursor != self.cursor {
            self.cursor = cursor;
            if self.hardware_cursor {
                self.upload_cursor();
            }
        }
    }

    fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
        if self.hardware_cursor {
            if visible {
                self.upload_cursor();
            } else {
                self.display.hide_cursor();
            }
        }
    }
}
