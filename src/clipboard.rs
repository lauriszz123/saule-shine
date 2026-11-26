//! System clipboard.
//!
//! The context is kept alive in a `thread_local` rather than rebuilt per call.
//! That is not just an optimisation: on X11 the clipboard is *owned* by a live
//! connection, and dropping it takes the copied text with it, so a short-lived
//! context would leave nothing for another application to paste.
//!
//! Where there is no system clipboard at all — the bare screen of Saule
//! Linux, with no X11 or Wayland to own one — the text is kept in this
//! process instead. A display server's process, there, keeps it for all
//! its apps: an app whose window is in a display server copies to and
//! pastes from the server's clipboard (`getClipboard`, `setClipboard` in
//! [`crate::protocol`]), so what is copied in one app is pasted in
//! another, and a server that is itself a window on a desktop passes it on
//! to the desktop's.

use std::cell::RefCell;

use saule_sdk::saule_export;

enum Store {
    System(arboard::Clipboard),
    /// No system clipboard: the text lives here.
    Local(String),
}

thread_local! {
    static CLIPBOARD: RefCell<Option<Store>> = const { RefCell::new(None) };
}

/// Run `f` against the process clipboard, creating it on first use.
fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    CLIPBOARD.with(|cell| {
        let mut guard = cell.borrow_mut();
        let store = guard.get_or_insert_with(|| match arboard::Clipboard::new() {
            Ok(clipboard) => Store::System(clipboard),
            Err(_) => Store::Local(String::new()),
        });
        f(store)
    })
}

/// The text on this process's clipboard: the system's, or its own.
pub(crate) fn text() -> String {
    with(|store| match store {
        Store::System(clipboard) => clipboard.get_text().unwrap_or_default(),
        Store::Local(text) => text.clone(),
    })
}

/// Put `text` on this process's clipboard.
pub(crate) fn put(text: String) -> Result<(), String> {
    with(|store| match store {
        Store::System(clipboard) => clipboard
            .set_text(text)
            .map_err(|e| format!("Clipboard.set: {e}")),
        Store::Local(local) => {
            *local = text;
            Ok(())
        }
    })
}

/// The clipboard of the display server this program's window is in, if
/// it is in one: what every app of that server copies to and pastes from.
fn served() -> Option<String> {
    crate::state::with(|e| e.backend.clipboard()).ok().flatten()
}

/// `Clipboard.get()` — the clipboard's text, or `""` when it is empty or holds
/// something that isn't text (an image, a file list).
///
/// Empty rather than an error because "nothing to paste" is an ordinary state,
/// not a failure — a paste handler shouldn't need a `try` around it.
#[saule_export(class = "Clipboard", name = "get")]
fn clipboard_get() -> Result<String, String> {
    Ok(served().unwrap_or_else(text))
}

/// `Clipboard.set(text)` — replace the clipboard contents.
#[saule_export(class = "Clipboard", name = "set")]
fn clipboard_set(text: String) -> Result<(), String> {
    if crate::state::with(|e| e.backend.set_clipboard(&text)).unwrap_or(false) {
        return Ok(());
    }
    put(text)
}

/// `Clipboard.hasText()` — whether there is text to paste.
#[saule_export(class = "Clipboard", name = "hasText")]
fn clipboard_has_text() -> Result<bool, String> {
    Ok(!served().unwrap_or_else(text).is_empty())
}
