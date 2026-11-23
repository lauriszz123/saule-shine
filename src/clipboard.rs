//! System clipboard.
//!
//! The context is kept alive in a `thread_local` rather than rebuilt per call.
//! That is not just an optimisation: on X11 the clipboard is *owned* by a live
//! connection, and dropping it takes the copied text with it, so a short-lived
//! context would leave nothing for another application to paste.
//!
//! Where there is no system clipboard at all — the bare screen of Saule
//! Linux, with no X11 or Wayland to own one — the text is kept in this
//! process instead, so copy and paste still work within the app.

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

/// `Clipboard.get()` — the clipboard's text, or `""` when it is empty or holds
/// something that isn't text (an image, a file list).
///
/// Empty rather than an error because "nothing to paste" is an ordinary state,
/// not a failure — a paste handler shouldn't need a `try` around it.
#[saule_export(class = "Clipboard", name = "get")]
fn clipboard_get() -> Result<String, String> {
    Ok(with(|store| match store {
        Store::System(clipboard) => clipboard.get_text().unwrap_or_default(),
        Store::Local(text) => text.clone(),
    }))
}

/// `Clipboard.set(text)` — replace the clipboard contents.
#[saule_export(class = "Clipboard", name = "set")]
fn clipboard_set(text: String) -> Result<(), String> {
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

/// `Clipboard.hasText()` — whether there is text to paste.
#[saule_export(class = "Clipboard", name = "hasText")]
fn clipboard_has_text() -> Result<bool, String> {
    Ok(with(|store| match store {
        Store::System(clipboard) => clipboard.get_text().is_ok_and(|text| !text.is_empty()),
        Store::Local(text) => !text.is_empty(),
    }))
}
