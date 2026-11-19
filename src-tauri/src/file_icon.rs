//! The system's icon for a file (what Finder shows), as a PNG data URL the
//! tree can put in an `<img>`. macOS only; elsewhere the UI keeps its own
//! icons.

#[cfg(target_os = "macos")]
mod mac;

/// Sync, so Tauri runs it on the main thread, as AppKit wants. `None` when the
/// platform has no icon to give, or the file (or a symlink's target) is gone.
///
/// A symlink gets its target's icon: macOS would otherwise draw its own alias
/// arrow, and the tree already puts one on.
#[tauri::command]
pub fn file_icon(path: String) -> Option<String> {
    let path = std::fs::canonicalize(path).ok()?;
    #[cfg(target_os = "macos")]
    return mac::png(&path.to_string_lossy()).map(|png| data_url(&png));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn data_url(png: &[u8]) -> String {
    format!("data:image/png;base64,{}", base64(png))
}

/// Standard base64 with padding (RFC 4648).
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want);
        }
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn wraps_png_in_a_data_url() {
        assert_eq!(data_url(b"foo"), "data:image/png;base64,Zm9v");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn returns_a_png_for_a_file() {
        let url = file_icon(file!().into()).expect("an icon");
        // base64 of the PNG signature's first bytes.
        assert!(url.starts_with("data:image/png;base64,iVBORw0KGgo"), "{url}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_symlink_gets_its_targets_icon() {
        let dir = std::env::temp_dir().join(format!("gestalt-icon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (real, link) = (dir.join("real.md"), dir.join("link.md"));
        std::fs::write(&real, "hi").unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let icon = |p: &std::path::Path| file_icon(p.to_string_lossy().into_owned());
        assert_eq!(icon(&link), icon(&real));
        std::fs::remove_file(&real).unwrap();
        assert_eq!(icon(&link), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_has_no_icon() {
        assert_eq!(file_icon("/no/such/file.md".into()), None);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn has_no_icon_elsewhere() {
        assert_eq!(file_icon(file!().into()), None);
    }
}
