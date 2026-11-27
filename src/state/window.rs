//! The window and the frame loop: sizing, presenting, and pacing. What the
//! frames are shown on is the [`super::backend::Backend`].

mod input;

use minifb::Key;
use std::time::{Duration, Instant};

use super::*;

/// How long an unchanged frame may go without being re-sent to the window.
/// See [`Engine::present`].
const FORCE_BLIT_EVERY: Duration = Duration::from_millis(500);

/// Opt the process into per-monitor DPI awareness before any window exists.
///
/// Without this Windows lies: `GetDpiForWindow` reports 96 and the OS scales
/// the window's pixels up for us, which on a scaled display means a blurry UI
/// we cannot see the real resolution of. Declaring awareness hands us physical
/// pixels and an honest DPI, which is what the toolkit wants — it does its own
/// scaling.
#[cfg(windows)]
pub(crate) fn declare_dpi_aware() {
    use std::sync::Once;

    static ONCE: Once = Once::new();

    ONCE.call_once(|| {
        // Safety: no arguments to get wrong, and a failure (already set by a
        // manifest, or an older Windows) is reported by the return value we
        // deliberately ignore.
        unsafe {
            windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );
        }
    });
}

#[cfg(not(windows))]
pub(crate) fn declare_dpi_aware() {}

/// Frame budget a fresh window starts with — 60 FPS.
pub(crate) const DEFAULT_FRAME_DUR: Duration = Duration::from_nanos(1_000_000_000 / 60);

impl Engine {
    /// Draw on the GPU the backend presents through, once it has one; and
    /// stop, keeping every pixel, if the backend lost it.
    pub(crate) fn follow_gpu(&mut self) {
        match self.backend.gpu_device() {
            Some(dev) if !self.r.on_gpu() => self.r.attach_gpu(dev),
            None if self.r.on_gpu() => self.r.detach_gpu(true),
            _ => {}
        }
    }

    /// `true` while the OS window is open.
    ///
    /// Escape only ends the loop when `Window.setQuitOnEscape(true)` asked for
    /// it. Love2D quits on Escape by default; an application toolkit cannot
    /// live with that, because Escape is also how a modal, a menu, or an
    /// autocomplete popup is dismissed — and the app has no way to decline the
    /// engine's quit. The `Closed` event covers the real close.
    pub fn is_open(&self) -> bool {
        self.backend.is_open()
            && !(self.quit_on_escape && self.backend.is_key_down(Key::Escape))
    }

    pub fn set_quit_on_escape(&mut self, enable: bool) {
        self.quit_on_escape = enable;
    }

    pub fn quit_on_escape(&self) -> bool {
        self.quit_on_escape
    }

    /// The window's framebuffer dimensions as `(width, height)`.
    pub fn size(&self) -> (usize, usize) {
        (self.r.screen.w, self.r.screen.h)
    }

    /// The DPI scale factor, refreshed every `pollEvents`.
    pub fn dpi_scale(&self) -> f64 {
        self.scale
    }

    /// Cap the loop at `fps` frames per second; `0` removes the cap.
    ///
    /// The pacing sleep in [`Engine::present`] is the only thing throttling a
    /// Saule game loop, and it used to be a hardcoded 60 with no way to ask
    /// for less (a mostly idle UI on a laptop battery) or more (a 120 Hz
    /// display).
    pub fn set_target_fps(&mut self, fps: i64) -> Result<(), String> {
        if fps < 0 {
            return Err("Window.setTargetFPS: the rate cannot be negative".into());
        }
        if fps > 1000 {
            return Err("Window.setTargetFPS: the rate may not exceed 1000".into());
        }
        self.frame_dur = if fps == 0 {
            None
        } else {
            Some(Duration::from_secs_f64(1.0 / fps as f64))
        };
        self.next_frame = Instant::now() + self.frame_dur.unwrap_or_default();
        Ok(())
    }

    pub fn target_fps(&self) -> i64 {
        match self.frame_dur {
            None => 0,
            Some(d) => (1.0 / d.as_secs_f64()).round() as i64,
        }
    }

    /// How many framebuffer pixels there are per unit of the size and pointer
    /// coordinates the backend reports (see [`Backend::backing_scale`]).
    pub(crate) fn backing_scale(&self) -> f64 {
        self.backend.backing_scale()
    }

    /// Match the framebuffer to the window, and refresh the scale factor.
    ///
    /// A resized window needs a new buffer before anything is drawn into it —
    /// `update_with_buffer` rejects a mismatch, and drawing into the old one
    /// would clip to the previous size. The scale is re-read here too, so
    /// dragging a window onto a monitor with different DPI is picked up.
    /// Returns the new size when the framebuffer was actually replaced, which
    /// is what a `Resized` event reports.
    pub(crate) fn sync_surface(&mut self) -> Option<(usize, usize)> {
        // Read the scale first: the framebuffer size depends on it, so a window
        // dragged onto a denser monitor has to be re-sized in the same pass
        // that notices the density changed.
        self.scale = self.backend.scale();

        let (points_w, points_h) = self.backend.size();
        let backing = self.backing_scale();
        let w = (points_w as f64 * backing).round() as usize;
        let h = (points_h as f64 * backing).round() as usize;

        let mut resized = None;

        if w > 0 && h > 0 && (w != self.r.screen.w || h != self.r.screen.h) {
            self.r.resize_screen(w, h);
            resized = Some((w, h));
        }

        resized
    }

    pub fn set_title(&mut self, title: &str) {
        self.backend.set_title(title);
    }

    pub fn position(&self) -> (i64, i64) {
        self.backend.position()
    }

    pub fn set_position(&mut self, x: i64, y: i64) {
        self.backend.set_position(x, y);
    }

    pub fn set_topmost(&mut self, topmost: bool) {
        self.backend.set_topmost(topmost);
    }

    /// Whether the window has keyboard focus. A background app should usually
    /// stop animating.
    pub fn is_focused(&mut self) -> bool {
        self.backend.is_active()
    }

    /// In place of [`Engine::present`] on a frame that drew nothing and
    /// has nothing to do: wait until input arrives, `timeout` at the
    /// longest, where the backend can tell when it has. Where it cannot,
    /// or there is a frame to show after all, this is `present`.
    pub fn rest(&mut self, timeout: Duration) -> Result<(), String> {
        let stale = self.last_blit.elapsed() >= FORCE_BLIT_EVERY;
        if self.r.screen_dirty || stale || self.backend.wants_present() {
            return self.present();
        }
        let started = Instant::now();
        if !self.backend.rest(timeout) {
            return self.present();
        }
        self.follow_gpu();
        crate::timer::mark_frame();
        self.mouse
            .observe(self.backend.mouse_buttons(), self.backend.wheel());
        crate::trace::frame(Duration::ZERO, started.elapsed());
        // The next frame is paced from now, not from before the wait.
        self.next_frame = Instant::now() + self.frame_dur.unwrap_or_default();
        Ok(())
    }

    /// Present the framebuffer to the window, then sleep until the next frame
    /// deadline. This is the loop's single pacing point.
    pub fn present(&mut self) -> Result<(), String> {
        // Copy the framebuffer to the window only when something was drawn
        // to it. That copy is every pixel, every time — an `XPutImage` of the
        // whole window, which under WSLg also crosses XWayland and the RDP
        // compositor — and an app that skips repainting idle frames still
        // presents each one to pace its loop. Re-sending an unchanged frame
        // at 60 Hz is what made a large or maximised window crawl.
        //
        // An unchanged frame still pumps the event queue, which is the other
        // half of what presenting is for.
        let stale = self.last_blit.elapsed() >= FORCE_BLIT_EVERY;
        let started = Instant::now();

        if self.r.screen_dirty || stale || self.backend.wants_present() {
            let Engine { backend, r, .. } = self;
            // A frame the GPU drew goes straight to the window; one the CPU
            // drew (or the GPU could not show) goes as its pixels.
            let shown = match r.screen_texture() {
                Some(texture) => backend.present_texture(texture)?,
                None => false,
            };
            if !shown {
                r.sync_cpu(crate::render::Which::Screen);
                backend.present(&r.screen.buf, r.screen.w, r.screen.h)?;
            }
            self.r.screen_dirty = false;
            self.last_blit = Instant::now();
        } else {
            self.backend.pump();
        }
        self.follow_gpu();

        crate::timer::mark_frame();

        // Presenting pumps the OS queue a second time, and minifb clears its
        // scroll delta at the start of every pump. Sampling here is what stops
        // a wheel notch that arrived mid-frame from being wiped before
        // `pollEvents` gets to read it. Keys need no equivalent: their edges
        // are latched in the backend's callback as the messages arrive.
        self.mouse
            .observe(self.backend.mouse_buttons(), self.backend.wheel());

        let presented = started.elapsed();
        let Some(budget) = self.frame_dur else {
            crate::trace::frame(presented, Duration::ZERO);
            return Ok(()); // uncapped: present and go straight back round
        };

        // Pace the loop: sleep off whatever time is left in this frame.
        let now = Instant::now();
        let mut idle = Duration::ZERO;
        if now < self.next_frame {
            idle = self.next_frame - now;
            std::thread::sleep(idle);
        }
        crate::trace::frame(presented, idle);
        // Schedule the next deadline; if we fell behind (a slow frame), resync
        // from now so we don't try to "catch up" with a burst of zero-sleep
        // frames.
        self.next_frame += budget;
        let now = Instant::now();
        if self.next_frame < now {
            self.next_frame = now + budget;
        }
        Ok(())
    }
}
