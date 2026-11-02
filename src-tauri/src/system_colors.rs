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
mod mac {
    use super::{Palette, SystemColors};
    use block2::RcBlock;
    use objc2_app_kit::{
        NSAppearance, NSAppearanceNameAccessibilityHighContrastAqua,
        NSAppearanceNameAccessibilityHighContrastDarkAqua, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
        NSColor, NSColorSpace, NSWorkspace,
    };
    use objc2_foundation::{NSUserDefaults, ns_string};
    use std::cell::RefCell;

    fn hex(c: &NSColor) -> Option<String> {
        let c = c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
        let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Some(format!(
            "#{:02x}{:02x}{:02x}",
            byte(c.redComponent()),
            byte(c.greenComponent()),
            byte(c.blueComponent())
        ))
    }

    /// Resolve the (dynamic) system colours as drawn in one appearance.
    fn palette(appearance: &NSAppearance, multicolor: bool) -> Palette {
        let out = RefCell::new(None);
        let block = RcBlock::new(|| {
            let accent = if multicolor { None } else { hex(&NSColor::controlAccentColor()) };
            let highlight = hex(&NSColor::selectedTextBackgroundColor()).unwrap_or_default();
            *out.borrow_mut() = Some(Palette { accent, highlight });
        });
        appearance.performAsCurrentDrawingAppearance(&block);
        drop(block);
        out.into_inner().unwrap_or(Palette { accent: None, highlight: String::new() })
    }

    pub fn read() -> Option<SystemColors> {
        // No AppleAccentColor default = "Multicolor".
        let multicolor = NSUserDefaults::standardUserDefaults()
            .objectForKey(ns_string!("AppleAccentColor"))
            .is_none();
        let contrast = NSWorkspace::sharedWorkspace().accessibilityDisplayShouldIncreaseContrast();
        let (light, dark) = unsafe {
            if contrast {
                (NSAppearanceNameAccessibilityHighContrastAqua, NSAppearanceNameAccessibilityHighContrastDarkAqua)
            } else {
                (NSAppearanceNameAqua, NSAppearanceNameDarkAqua)
            }
        };
        Some(SystemColors {
            light: palette(&*NSAppearance::appearanceNamed(light)?, multicolor),
            dark: palette(&*NSAppearance::appearanceNamed(dark)?, multicolor),
        })
    }

    /// Call `changed` (on the main thread) whenever the colours may have changed.
    pub fn observe(changed: impl Fn() + 'static) {
        use objc2_app_kit::{NSSystemColorsDidChangeNotification, NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification};
        use objc2_foundation::{NSNotificationCenter, NSOperationQueue};

        let block = RcBlock::new(move |_| changed());
        let queue = NSOperationQueue::mainQueue();
        unsafe {
            // Accent and highlight changes.
            let token = NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(NSSystemColorsDidChangeNotification),
                None,
                Some(&queue),
                &block,
            );
            std::mem::forget(token); // observe for the app's lifetime
            // Increase Contrast.
            let token = NSWorkspace::sharedWorkspace()
                .notificationCenter()
                .addObserverForName_object_queue_usingBlock(
                    Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
                    None,
                    Some(&queue),
                    &block,
                );
            std::mem::forget(token);
        }
    }
}

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
