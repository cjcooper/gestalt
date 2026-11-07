//! The user's macOS colour choices (System Settings › Appearance), for the UI
//! to adopt: the accent colour (buttons, selection, links) and the highlight
//! colour (selected text). Light/dark itself needs nothing here: the webview
//! follows it via `prefers-color-scheme`.
//!
//! Both colours are resolved for the light and the dark appearance, so the
//! page can switch with its own media query. With Increase Contrast on, the
//! high-contrast variants are used.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

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
pub fn watch<R: Runtime>(app: &AppHandle<R>) {
    #[cfg(target_os = "macos")]
    mac::observe(app.clone());
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// The settings changed: send the UI the current colours.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn changed<R: Runtime>(app: &AppHandle<R>) {
    emit(app, system_colors());
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn emit<R: Runtime>(app: &AppHandle<R>, payload: impl Serialize + Clone) {
    if let Err(e) = app.emit(CHANGED_EVENT, payload) {
        eprintln!("system colours: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use tauri::Listener;

    fn hex(s: &str) -> bool {
        s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_both_palettes_as_hex() {
        let colors = system_colors().expect("macOS colours");
        for p in [&colors.light, &colors.dark] {
            assert!(hex(&p.highlight), "{p:?}");
            assert!(p.accent.as_deref().is_none_or(hex), "{p:?}");
        }
    }

    #[test]
    fn serializes_for_the_ui() {
        let p = |a: Option<&str>| Palette { accent: a.map(Into::into), highlight: "#aabbcc".into() };
        let colors = SystemColors { light: p(Some("#007aff")), dark: p(None) };
        assert_eq!(
            serde_json::to_value(&colors).unwrap(),
            serde_json::json!({
                "light": {"accent": "#007aff", "highlight": "#aabbcc"},
                "dark": {"accent": null, "highlight": "#aabbcc"},
            })
        );
    }

    #[test]
    fn emits_current_colours_on_change() {
        let app = tauri::test::mock_app();
        // Registering the observers must not emit anything by itself.
        watch(app.handle());
        let (tx, rx) = mpsc::channel();
        app.listen_any(CHANGED_EVENT, move |e| tx.send(e.payload().to_string()).unwrap());

        changed(app.handle());
        let payload = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(payload, serde_json::to_string(&system_colors()).unwrap());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_failed_emit_is_only_logged() {
        let app = tauri::test::mock_app();
        let (tx, rx) = mpsc::channel();
        app.listen_any(CHANGED_EVENT, move |e| tx.send(e.payload().to_string()).unwrap());
        // JSON maps need string keys, so this can't be serialized.
        emit(app.handle(), std::collections::BTreeMap::from([((1, 2), 3)]));
        emit(app.handle(), "next");
        // Only the second one arrives.
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "\"next\"");
        assert!(rx.try_recv().is_err());
    }
}
