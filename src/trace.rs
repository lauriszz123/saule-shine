//! `SHINE_TRACE_FRAMES=1`: where a program's frames spend their time.
//!
//! Every two seconds, on stderr:
//!
//! ```text
//! shine: 31 fps · frame 32.1 ms = draw 11.0 + present 2.4 + program 18.7 (+ idle 0.0)
//! ```
//!
//! followed, when an average frame drew for a fifth of a millisecond or
//! more, by where that drawing went (the `Graphics` functions, the time
//! in each a frame and the calls of each a frame):
//!
//! ```text
//! shine 71:   drawn a frame · graphics.rs:116 9.80 ms in 59 · graphics.rs:212 1.10 ms in 36
//! ```
//!
//! - **draw**: inside Shine's drawing calls (`Graphics.*` on the renderer);
//! - **present**: putting the frame on screen, or sending it to the display
//!   server;
//! - **program**: everything else between two presents: the program's own
//!   Saule code, and the native calls that aren't drawing;
//! - **idle**: the pacing sleep that holds the loop to its target rate.
//!
//! An average hides the one frame that froze the screen — a picture decoded
//! or a shader compiled the first time it was needed — so any frame whose
//! work took longer than a tenth of a second is also reported on its own,
//! as it happens:
//!
//! ```text
//! shine 71: slow call 1694 ms at src/api/graphics.rs:412
//! shine 71: slow frame 1840 ms = draw 1702 + present 21 + program 117 · objects.rs:344 1694 ms in 1
//! ```
//!
//! with the `Graphics` functions most of its drawing went to (the time in
//! each and the number of calls), and how many shapes had their coverage
//! worked out on the CPU. So is the one drawing call that took most of it (30 ms or more),
//! by where in Shine's `Graphics` it is. The GPU draws shapes in batches,
//! so the call named may be the one that sent a batch off rather than the
//! one that filled it.
//!
//! `SHINE_TRACE_FRAMES=/some/file` appends there instead, for a program
//! whose stderr isn't a terminal: an app a display server started.
//!
//! Off unless the variable is set; when off, each hook is one cached check.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Where the report goes: `None` when tracing is off, `Some(None)` for
/// stderr, `Some(Some(path))` for a file.
fn target() -> &'static Option<Option<std::path::PathBuf>> {
    static TARGET: OnceLock<Option<Option<std::path::PathBuf>>> = OnceLock::new();
    TARGET.get_or_init(|| {
        let value = std::env::var_os("SHINE_TRACE_FRAMES")?;
        Some((value != "1").then(|| value.into()))
    })
}

fn enabled() -> bool {
    target().is_some()
}

fn report(line: &str) {
    match target() {
        Some(Some(path)) => {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{line}");
            }
        }
        _ => eprintln!("{line}"),
    }
}

#[derive(Clone, Copy)]
struct Totals {
    frames: u32,
    draw: Duration,
    present: Duration,
    idle: Duration,
    /// Surfaces copied up to the GPU and read back from it (`render::mirror`).
    uploads: u32,
    readbacks: u32,
    /// Batched draws, and the shapes they drew (`gpu::batch`).
    batches: u32,
    batched: u32,
}

const ZERO: Totals = Totals {
    frames: 0,
    uploads: 0,
    readbacks: 0,
    batches: 0,
    batched: 0,
    draw: Duration::ZERO,
    present: Duration::ZERO,
    idle: Duration::ZERO,
};

/// Time and calls, by where in `Graphics` the call is (file and line).
type Calls = HashMap<(&'static str, u32), (Duration, u32)>;

/// A frame whose work (everything but the pacing sleep) takes this long is
/// reported on its own.
const SLOW_FRAME: Duration = Duration::from_millis(100);

thread_local! {
    static TOTALS: Cell<Totals> = const { Cell::new(ZERO) };
    static SINCE: Cell<Option<Instant>> = const { Cell::new(None) };
    /// When the frame being made began, and the drawing done in it so far.
    static FRAME_BEGAN: Cell<Option<Instant>> = const { Cell::new(None) };
    static FRAME_DRAW: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    /// The drawing of the frame being made, by which `Graphics` function it
    /// was: the time in each, and how many calls.
    static FRAME_CALLS: RefCell<Calls> = RefCell::new(HashMap::new());
    /// Shapes whose coverage the CPU worked out in this frame, and the
    /// pixels of them: what the GPU's shape atlas is there to avoid.
    static FRAME_MASKS: Cell<(u32, u64)> = const { Cell::new((0, 0)) };
    /// The drawing since the last two-second line, by which `Graphics`
    /// function it was: what an ordinary frame's drawing goes to, where
    /// `FRAME_CALLS` is only told of for a frame that froze.
    static SPAN_CALLS: RefCell<Calls> = RefCell::new(HashMap::new());
}

/// A two-second line says where the drawing went when an average frame
/// drew for at least this long.
const WORTH_NAMING: f64 = 0.2;

/// A single drawing call that takes this long is reported on its own.
const SLOW_CALL: Duration = Duration::from_millis(30);

/// Time `f` as drawing; `caller` is the `Graphics.*` function it is.
pub(crate) fn drawing<R>(caller: &'static std::panic::Location<'static>, f: impl FnOnce() -> R) -> R {
    if !enabled() {
        return f();
    }
    let start = Instant::now();
    let out = f();
    let took = start.elapsed();
    if took >= SLOW_CALL {
        report(&format!(
            "shine {}: slow call {:.0} ms at {}:{}",
            std::process::id(),
            took.as_secs_f64() * 1000.0,
            caller.file(),
            caller.line()
        ));
    }
    TOTALS.with(|t| {
        let mut v = t.get();
        v.draw += took;
        t.set(v);
    });
    FRAME_DRAW.with(|d| d.set(d.get() + took));
    for calls in [&FRAME_CALLS, &SPAN_CALLS] {
        calls.with(|calls| {
            let mut calls = calls.borrow_mut();
            let entry = calls.entry((caller.file(), caller.line())).or_default();
            entry.0 += took;
            entry.1 += 1;
        });
    }
    out
}

/// The CPU worked out the coverage of a shape of `pixels` pixels, for the
/// GPU to blend: one the shape atlas did not have.
pub(crate) fn covered(pixels: usize) {
    if !enabled() {
        return;
    }
    FRAME_MASKS.with(|m| {
        let (shapes, total) = m.get();
        m.set((shapes + 1, total + pixels as u64));
    });
}

/// A surface was copied up to the GPU (`up`) or read back from it: the
/// traffic drawing on the GPU is meant to avoid, reported per frame.
pub(crate) fn copied(up: bool) {
    if !enabled() {
        return;
    }
    TOTALS.with(|t| {
        let mut v = t.get();
        if up {
            v.uploads += 1;
        } else {
            v.readbacks += 1;
        }
        t.set(v);
    });
}

/// The GPU drew `shapes` shapes in one draw.
pub(crate) fn batched(shapes: u32) {
    if !enabled() {
        return;
    }
    TOTALS.with(|t| {
        let mut v = t.get();
        v.batches += 1;
        v.batched += shapes;
        t.set(v);
    });
}

/// One frame ended: `present` spent putting it on screen, `idle` asleep.
pub(crate) fn frame(present: Duration, idle: Duration) {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    slow_frame(now, present, idle);
    let since = SINCE.with(|s| *s.get().get_or_insert(now));
    SINCE.with(|s| {
        if s.get().is_none() {
            s.set(Some(now));
        }
    });
    let mut v = TOTALS.with(|t| t.get());
    v.frames += 1;
    v.present += present;
    v.idle += idle;
    let span = now.duration_since(since);
    if span >= Duration::from_secs(2) && v.frames > 0 {
        let n = f64::from(v.frames);
        let ms = |d: Duration| d.as_secs_f64() * 1000.0 / n;
        let frame = span.as_secs_f64() * 1000.0 / n;
        let (draw, present, idle) = (ms(v.draw), ms(v.present), ms(v.idle));
        let program = (frame - draw - present - idle).max(0.0);
        let mut copies = if v.uploads + v.readbacks > 0 {
            format!(
                " · copies up {:.1}, back {:.1}",
                f64::from(v.uploads) / n,
                f64::from(v.readbacks) / n
            )
        } else {
            String::new()
        };
        if v.batches > 0 {
            copies.push_str(&format!(
                " · {:.1} shapes in {:.1} draws",
                f64::from(v.batched) / n,
                f64::from(v.batches) / n
            ));
        }
        report(&format!(
            "shine {}: {:.0} fps · frame {frame:.1} ms = draw {draw:.1} + present {present:.1} \
             + program {program:.1} (+ idle {idle:.1}){copies}",
            std::process::id(),
            n / span.as_secs_f64()
        ));
        // And what an average frame's drawing went to, when it drew for
        // long enough to ask: the functions, the time in each a frame and
        // the calls of each a frame.
        let mut calls: Vec<_> = SPAN_CALLS.with(|c| c.borrow_mut().drain().collect());
        if draw >= WORTH_NAMING {
            calls.sort_by_key(|&(_, (took, _))| std::cmp::Reverse(took));
            let mut most = String::new();
            for ((file, line), (took, count)) in calls.into_iter().take(5) {
                let file = file.rsplit('/').next().unwrap_or(file);
                most.push_str(&format!(
                    " · {file}:{line} {:.2} ms in {:.0}",
                    ms(took),
                    f64::from(count) / n
                ));
            }
            report(&format!("shine {}:   drawn a frame{most}", std::process::id()));
        }
        v = ZERO;
        SINCE.with(|s| s.set(Some(now)));
    }
    TOTALS.with(|t| t.set(v));
}

/// Report the frame that just ended, by itself, if its work took long.
fn slow_frame(now: Instant, present: Duration, idle: Duration) {
    let began = FRAME_BEGAN.with(|b| b.replace(Some(now)));
    let draw = FRAME_DRAW.with(|d| d.replace(Duration::ZERO));
    let mut calls: Vec<_> = FRAME_CALLS.with(|c| c.borrow_mut().drain().collect());
    let (masks, mask_pixels) = FRAME_MASKS.with(|m| m.replace((0, 0)));
    let Some(began) = began else {
        return;
    };
    let work = now.duration_since(began).saturating_sub(idle);
    if work < SLOW_FRAME {
        return;
    }
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let program = work.saturating_sub(draw).saturating_sub(present);
    // Where the drawing went: the few functions that took most of it.
    calls.sort_by_key(|&(_, (took, _))| std::cmp::Reverse(took));
    let mut most = String::new();
    for ((file, line), (took, count)) in calls.into_iter().take(4) {
        let file = file.rsplit('/').next().unwrap_or(file);
        most.push_str(&format!(" · {file}:{line} {:.0} ms in {count}", ms(took)));
    }
    if masks > 0 {
        most.push_str(&format!(" · {masks} shapes covered on the CPU ({mask_pixels} px)"));
    }
    report(&format!(
        "shine {}: slow frame {:.0} ms = draw {:.0} + present {:.0} + program {:.0}{most}",
        std::process::id(),
        ms(work),
        ms(draw),
        ms(present),
        ms(program)
    ));
}
