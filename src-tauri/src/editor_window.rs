//! File editors as real windows (`editor.html`), one per file, so the user
//! can switch back to the main window — run a plan, say — while editing.

use crate::file_edit;
use crate::tofu::Workspace;
use tauri::{AppHandle, Manager, Runtime, State, WebviewUrl, WebviewWindowBuilder};

/// Editor window labels start with this (`capabilities/editor.json` matches it).
pub const LABEL_PREFIX: &str = "editor-";

/// The window label for a file: stable, so opening it again finds the window.
/// Labels allow only a few characters, so it's a hash of the path.
pub fn label_for(path: &str) -> String {
    // FNV-1a, 64-bit.
    let hash = path.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("{LABEL_PREFIX}{hash:016x}")
}

/// Open `path` (a file inside the workspace) in its own editor window, or
/// bring its window to the front if it's already open.
#[tauri::command]
pub fn open_file_editor<R: Runtime>(app: AppHandle<R>, workspace: State<'_, Workspace>, path: String) -> Result<(), String> {
    // One window per place in the tree: two links to the same file are two
    // windows, each planning in its own folder.
    let file = file_edit::locate(&workspace, &path)?;
    let label = label_for(&file.shown.to_string_lossy());
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    let title = file.rel.display().to_string();
    // The page reads which file to edit from here (src/editorWindow.ts): the
    // path as the tree spells it, so Plan runs in the folder the file appears
    // in (not where a link points) and names it as the tree does.
    let init = format!("window.__GESTALT_EDITOR__ = {};", serde_json::json!({ "path": file.shown }));
    create_window(&app, &label, &title, init)
}

fn create_window<R: Runtime>(app: &AppHandle<R>, label: &str, title: &str, init: String) -> Result<(), String> {
    WebviewWindowBuilder::new(app, label, WebviewUrl::App("editor.html".into()))
        .title(title)
        .inner_size(960.0, 720.0)
        .min_inner_size(420.0, 280.0)
        .initialization_script(init)
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::shell_env::testing::temp_dir;

    #[test]
    fn labels_are_stable_distinct_and_valid() {
        let a = label_for("/repo/a/main.tf");
        assert_eq!(a, label_for("/repo/a/main.tf"));
        assert_ne!(a, label_for("/repo/b/main.tf"));
        assert_eq!(a.len(), LABEL_PREFIX.len() + 16);
        assert!(a.starts_with(LABEL_PREFIX));
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        assert_eq!(label_for(""), "editor-cbf29ce484222325");
    }

    #[test]
    fn opens_one_window_per_file() {
        let root = temp_dir("editor-window");
        std::fs::create_dir_all(root.join("mod")).unwrap();
        std::fs::write(root.join("mod/main.tf"), "").unwrap();
        let app = tauri::test::mock_builder().build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
        app.manage(Workspace { root: Some(root.clone()).into() });
        let open = |p: &std::path::Path| open_file_editor(app.handle().clone(), app.state(), p.to_string_lossy().into());

        let file = root.join("mod/main.tf");
        open(&file).unwrap();
        let label = label_for(&file.to_string_lossy());
        assert!(app.get_webview_window(&label).is_some());
        // Again (even by another spelling of the path): the same window.
        open(&root.join("mod/../mod/main.tf")).unwrap();
        assert_eq!(app.webview_windows().len(), 1);

        // Two links to the same file are two windows: each plans in its own folder.
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::os::unix::fs::symlink(&file, root.join("other/main.tf")).unwrap();
        open(&root.join("other/main.tf")).unwrap();
        assert_eq!(app.webview_windows().len(), 2);

        assert!(open(&root.join("mod")).unwrap_err().contains("is not a file inside"));
        // A window that can't be made (its label is taken) reports why.
        assert!(!create_window(app.handle(), &label, "x", String::new()).unwrap_err().is_empty());
    }
}
