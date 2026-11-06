//! AppKit calls behind [`super`]. Thin FFI with no logic worth asserting on,
//! so it is left out of the coverage target.

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
