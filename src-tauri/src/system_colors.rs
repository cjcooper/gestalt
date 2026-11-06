//! The user's macOS colour choices (System Settings › Appearance), for the UI
//! to adopt: the accent colour (buttons, selection, links) and the highlight
//! colour (selected text). Light/dark itself needs nothing here: the webview
//! follows it via `prefers-color-scheme`.
//!
//! Both colours are resolved for the light and the dark appearance, so the
//! page can switch with its own media query. With Increase Contrast on, the
//! high-contrast variants are used.

use serde::Serialize;
use tauri::AppHandle;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Palette {
    /// `None` when the accent is "Multicolor" (the macOS default), which
    /// means apps keep their own accent colour.
    accent: Option<String>,
    highlight: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SystemColors {
    light: Palette,
    dark: Palette,
}

/// Event emitted when the user changes an accent, highlight or contrast setting.
pub const CHANGED_EVENT: &str = "system://colors";

#[cfg(target_os = "macos")]
mod mac;

/// Sync, so Tauri runs it on the main thread, as AppKit wants.
#[tauri::command]
pub fn system_colors() -> Option<SystemColors> {
    #[cfg(target_os = "macos")]
    return mac::read();
    #[cfg(not(target_os = "macos"))]
    None
}

/// Push [`CHANGED_EVENT`] to the UI whenever the user changes these settings.
pub fn watch(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        use tauri::Emitter;
        let app = app.clone();
        mac::observe(move || {
            if let Err(e) = app.emit(CHANGED_EVENT, mac::read()) {
                eprintln!("system colours: {e}");
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}
