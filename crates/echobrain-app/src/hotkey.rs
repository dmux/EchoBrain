//! The global shortcut: ⌥Space, the same chord Raycast and the Gemini and
//! Claude desktop apps use to summon a floating panel.
//!
//! # What this deliberately does *not* do
//!
//! It does not start or stop recording. It raises the window and emits an
//! event; the frontend decides what that means, because the frontend is the
//! only side that knows whether a dictation is already in progress, whether the
//! user is mid-edit in the capture field, or whether the panel was already
//! open. Duplicating that state in Rust would mean two sources of truth for one
//! toggle, and they would drift.

use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// The event the frontend listens for.
pub const TOGGLE_EVENT: &str = "echobrain://toggle-dictation";

pub fn register<R: Runtime>(app: &AppHandle<R>) -> Result<(), Box<dyn std::error::Error>> {
    let shortcut = Shortcut::new(Some(Modifiers::ALT), Code::Space);
    let handle = app.clone();

    app.global_shortcut()
        .on_shortcut(shortcut, move |_, _, event| {
            // Fire on press only. Without this the handler runs twice per tap and
            // the toggle lands back where it started.
            if event.state() != ShortcutState::Pressed {
                return;
            }

            if let Some(window) = handle.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }

            if let Err(error) = handle.emit(TOGGLE_EVENT, ()) {
                tracing::warn!(%error, "could not deliver the dictation toggle");
            }
        })?;

    tracing::info!("⌥Space registered");
    Ok(())
}
