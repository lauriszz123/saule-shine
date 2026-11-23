//! Where a new desktop window opens: in the middle of one monitor, on every
//! system.
//!
//! "One monitor" is the one the mouse pointer is on — where the person is
//! looking — or else the primary one. What each system needs for that:
//!
//! - **Linux (X11)**: minifb centres a new window on the X *screen*, and with
//!   several monitors that screen is all of them side by side (a 1920 and a
//!   2560 pixel monitor make one 4480-pixel screen), so the window straddled
//!   the edge between two. The monitors come from XRandR instead
//!   (`libXrandr`, loaded at run time like the rest of X11).
//! - **Windows**: minifb lets Windows choose (`CW_USEDEFAULT`), which
//!   cascades from the primary monitor's top-left corner. The monitor under
//!   the pointer and its work area (the screen minus the taskbar) come from
//!   Win32.
//! - **macOS**: minifb already calls `[NSWindow center]`, which centres on
//!   the screen with the keyboard focus, so nothing is done there.
//!
//! When anything can't be found out, the window stays where minifb put it.

/// A monitor's usable area, in the desktop's pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Monitor {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub primary: bool,
}

/// Where the mouse pointer is on the desktop, when it can be found.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
type Pointer = Option<(i64, i64)>;

impl Monitor {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn contains(&self, (px, py): (i64, i64)) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }
}

/// The monitor to open on: the pointer's, else the primary, else the first.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn pick(monitors: &[Monitor], pointer: Pointer) -> Option<Monitor> {
    pointer
        .and_then(|p| monitors.iter().find(|m| m.contains(p)))
        .or_else(|| monitors.iter().find(|m| m.primary))
        .or_else(|| monitors.first())
        .copied()
}

/// The top-left corner that centres a `width × height` window on `m`. A
/// window larger than the monitor starts at its corner rather than off its
/// edge.
fn centre_on(m: Monitor, width: i64, height: i64) -> (i64, i64) {
    (
        m.x + ((m.width - width) / 2).max(0),
        m.y + ((m.height - height) / 2).max(0),
    )
}

/// Where to move a new window so it is centred on one monitor, or `None`
/// to leave it where minifb put it.
#[cfg(target_os = "linux")]
pub(super) fn centred_position(
    _window: &minifb::Window,
    width: usize,
    height: usize,
) -> Option<(i64, i64)> {
    let (monitors, pointer) = x11::query()?;
    let monitor = pick(&monitors, pointer)?;
    Some(centre_on(monitor, width as i64, height as i64))
}

#[cfg(windows)]
pub(super) fn centred_position(
    window: &minifb::Window,
    _width: usize,
    _height: usize,
) -> Option<(i64, i64)> {
    let (monitor, (width, height)) = win32::query(window)?;
    Some(centre_on(monitor, width, height))
}

#[cfg(not(any(target_os = "linux", windows)))]
pub(super) fn centred_position(
    _window: &minifb::Window,
    _width: usize,
    _height: usize,
) -> Option<(i64, i64)> {
    None
}

#[cfg(target_os = "linux")]
mod x11 {
    use std::os::raw::{c_int, c_uint, c_ulong};
    use std::ptr;

    use x11_dl::{xlib, xrandr};

    use super::{Monitor, Pointer};

    /// The active monitors and where the pointer is, from a short-lived
    /// connection of our own.
    pub(super) fn query() -> Option<(Vec<Monitor>, Pointer)> {
        let xlib = xlib::Xlib::open().ok()?;
        let xrandr = xrandr::Xrandr::open().ok()?;

        // Safety: every call below is on the display opened here, which is
        // closed before returning; the monitor list is freed with the call
        // meant for it, after being copied out.
        unsafe {
            let display = (xlib.XOpenDisplay)(ptr::null());
            if display.is_null() {
                return None;
            }
            let root = (xlib.XDefaultRootWindow)(display);

            let mut count: c_int = 0;
            let list = (xrandr.XRRGetMonitors)(display, root, xlib::True, &mut count);
            let mut monitors = Vec::new();
            if !list.is_null() {
                for i in 0..count.max(0) as usize {
                    let m = *list.add(i);
                    monitors.push(Monitor {
                        x: i64::from(m.x),
                        y: i64::from(m.y),
                        width: i64::from(m.width),
                        height: i64::from(m.height),
                        primary: m.primary != 0,
                    });
                }
                (xrandr.XRRFreeMonitors)(list);
            }

            let (mut root_ret, mut child): (c_ulong, c_ulong) = (0, 0);
            let (mut px, mut py, mut wx, mut wy): (c_int, c_int, c_int, c_int) = (0, 0, 0, 0);
            let mut mask: c_uint = 0;
            let on_screen = (xlib.XQueryPointer)(
                display,
                root,
                &mut root_ret,
                &mut child,
                &mut px,
                &mut py,
                &mut wx,
                &mut wy,
                &mut mask,
            ) != 0;

            (xlib.XCloseDisplay)(display);

            let pointer = on_screen.then(|| (i64::from(px), i64::from(py)));
            Some((monitors, pointer))
        }
    }
}

#[cfg(windows)]
mod win32 {
    use windows_sys::Win32::Foundation::{POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect};

    use super::Monitor;

    /// The work area of the monitor under the pointer (the primary one when
    /// the pointer can't be found), and the window's outer size — its frame
    /// included, since that is what a move positions.
    pub(super) fn query(window: &minifb::Window) -> Option<(Monitor, (i64, i64))> {
        let hwnd = window.get_window_handle();
        if hwnd.is_null() {
            return None;
        }

        // Safety: plain Win32 queries into locals; `hwnd` is minifb's live
        // window.
        unsafe {
            let mut frame: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd as _, &mut frame) == 0 {
                return None;
            }

            let mut pointer = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pointer);
            let handle = MonitorFromPoint(pointer, MONITOR_DEFAULTTOPRIMARY);

            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(handle, &mut info) == 0 {
                return None;
            }

            let work = info.rcWork;
            Some((
                Monitor {
                    x: i64::from(work.left),
                    y: i64::from(work.top),
                    width: i64::from(work.right - work.left),
                    height: i64::from(work.bottom - work.top),
                    primary: true,
                },
                (
                    i64::from(frame.right - frame.left),
                    i64::from(frame.bottom - frame.top),
                ),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(x: i64, y: i64, width: i64, height: i64) -> Monitor {
        Monitor {
            x,
            y,
            width,
            height,
            primary: false,
        }
    }

    /// A 2560×1440 monitor on the right of a 1920×1080 one, neither primary:
    /// the setup this was written on.
    fn two() -> Vec<Monitor> {
        vec![monitor(1920, 0, 2560, 1440), monitor(0, 139, 1920, 1080)]
    }

    #[test]
    fn opens_on_the_monitor_with_the_pointer() {
        let m = pick(&two(), Some((100, 500))).unwrap();
        assert_eq!(centre_on(m, 1024, 600), (448, 139 + 240));
        let m = pick(&two(), Some((3000, 10))).unwrap();
        assert_eq!(centre_on(m, 1024, 600), (1920 + 768, 420));
    }

    #[test]
    fn without_a_pointer_the_primary_or_the_first() {
        assert_eq!(pick(&two(), None).unwrap().x, 1920);
        let mut monitors = two();
        monitors[1].primary = true;
        assert_eq!(pick(&monitors, None).unwrap().x, 0);
    }

    #[test]
    fn one_monitor_is_centred_on_too() {
        let m = pick(&two()[..1], None).unwrap();
        assert_eq!(centre_on(m, 1024, 600), (1920 + 768, 420));
    }

    #[test]
    fn no_monitors_leaves_the_window_alone() {
        assert_eq!(pick(&[], Some((10, 10))), None);
    }

    #[test]
    fn a_window_bigger_than_the_monitor_starts_at_its_corner() {
        assert_eq!(centre_on(two()[1], 4000, 2000), (0, 139));
    }
}
