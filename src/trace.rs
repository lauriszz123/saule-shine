//! `SHINE_TRACE_FRAMES=1`: where a program's frames spend their time.
//!
//! Every two seconds, on stderr:
//!
//! ```text
//! shine: 31 fps · frame 32.1 ms = draw 11.0 + present 2.4 + program 18.7 (+ idle 0.0)
//! ```
//!
//! - **draw**: inside Shine's drawing calls (`Graphics.*` on the renderer);
//! - **present**: putting the frame on screen, or sending it to the display
//!   server;
//! - **program**: everything else between two presents: the program's own
//!   Saule code, and the native calls that aren't drawing;
//! - **idle**: the pacing sleep that holds the loop to its target rate.
//!
//! `SHINE_TRACE_FRAMES=/some/file` appends there instead, for a program
//! whose stderr isn't a terminal: an app a display server started.
//!
//! Off unless the variable is set; when off, each hook is one cached check.

use std::cell::Cell;
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

thread_local! {
    static TOTALS: Cell<Totals> = const { Cell::new(ZERO) };
    static SINCE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Time `f` as drawing.
pub(crate) fn drawing<R>(f: impl FnOnce() -> R) -> R {
    if !enabled() {
        return f();
    }
    let start = Instant::now();
    let out = f();
    let took = start.elapsed();
    TOTALS.with(|t| {
        let mut v = t.get();
        v.draw += took;
        t.set(v);
    });
    out
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
        v = ZERO;
        SINCE.with(|s| s.set(Some(now)));
    }
    TOTALS.with(|t| t.set(v));
}
