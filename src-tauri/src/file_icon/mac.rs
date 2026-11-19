//! AppKit calls behind [`super`]. Thin FFI with no logic worth asserting on,
//! so it is left out of the coverage target.

use objc2::AnyThread;
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSDeviceRGBColorSpace, NSGraphicsContext, NSWorkspace,
};
use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};

/// Points the tree draws icons at.
const POINTS: f64 = 16.0;
/// Pixels per point: renders the Retina (@2x) artwork, sharp on either kind of screen.
const SCALE: f64 = 2.0;

/// The icon Finder shows for `path`, drawn at 16pt @2x and encoded as PNG.
pub fn png(path: &str) -> Option<Vec<u8>> {
    let image = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(path));
    let px = (POINTS * SCALE) as isize;
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            px,
            px,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }?;
    // A 16pt canvas of 32px, so the image picks its 16pt@2x artwork.
    rep.setSize(NSSize::new(POINTS, POINTS));
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    image.drawInRect(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(POINTS, POINTS)));
    context.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }?;
    Some(data.to_vec())
}
