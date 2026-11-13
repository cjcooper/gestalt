//! File editors as real windows (`editor.html`), one per file, so the user
//! can switch back to the workspace window — run a plan, say — while editing.
//! Each belongs to the workspace window it was opened from: it edits files in
//! that window's folder, plans there, and closes with it.

use crate::file_edit;
use crate::windows::{self, Windows};
use tauri::{AppHandle, Manager, Runtime, State, WebviewUrl, WebviewWindowBuilder, Webview};

/// Editor window labels start with this (`capabilities/editor.json` matches
/// it), then the workspace window's label: `editor-<owner>-<hash>`.
pub const LABEL_PREFIX: &str = "editor-";

/// The window label for a file opened from workspace window `owner`: stable,
/// so opening it again finds the window. Labels allow only a few characters,
/// so it's a hash of the path.
pub fn label_for(owner: &str, path: &str) -> String {
    // FNV-1a, 64-bit.
    let hash = path.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("{LABEL_PREFIX}{owner}-{hash:016x}")
}

/// The workspace window an editor window (by label) belongs to.
pub fn owner(label: &str) -> Option<&str> {
    let (owner, hash) = label.strip_prefix(LABEL_PREFIX)?.rsplit_once('-')?;
    (hash.len() == 16 && windows::is_workspace(owner)).then_some(owner)
}

/// Open `path` (a file inside the calling window's folder) in its own editor
/// window, or bring its window to the front if it's already open.
#[tauri::command]
pub fn open_file_editor<R: Runtime>(webview: Webview<R>, windows: State<'_, Windows>, path: String) -> Result<(), String> {
    let app = webview.app_handle();
    let owner = owner(webview.label()).unwrap_or(webview.label()).to_string();
    // One window per place in the tree: two links to the same file are two
    // windows, each planning in its own folder.
    let file = file_edit::locate(windows.workspace_root(&owner).as_deref(), &path)?;
    let label = label_for(&owner, &file.shown.to_string_lossy());
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    let title = file.rel.display().to_string();
    // The page reads which file to edit from here (src/editorWindow.ts): the
    // path as the tree spells it, so Plan runs in the folder the file appears
    // in (not where a link points) and names it as the tree does.
    let init = format!("window.__GESTALT_EDITOR__ = {};", serde_json::json!({ "path": file.shown, "owner": owner }));
    create_window(app, &label, &title, init)
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
        let a = label_for("main", "/repo/a/main.tf");
        assert_eq!(a, label_for("main", "/repo/a/main.tf"));
        assert_ne!(a, label_for("main", "/repo/b/main.tf"));
        assert_ne!(a, label_for("workspace-2", "/repo/a/main.tf"));
        assert_eq!(a.len(), "editor-main-".len() + 16);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        assert_eq!(label_for("workspace-12", ""), "editor-workspace-12-cbf29ce484222325");
    }

    #[test]
    fn labels_name_their_workspace_window() {
        assert_eq!(owner(&label_for("main", "/x")), Some("main"));
        assert_eq!(owner(&label_for("workspace-12", "/x")), Some("workspace-12"));
        for not_an_editor in ["main", "about", "editor-", "editor-main", "editor-main-abc", "editor-about-cbf29ce484222325"] {
            assert_eq!(owner(not_an_editor), None, "{not_an_editor}");
        }
    }

    #[test]
    fn opens_one_window_per_file() {
        let root = temp_dir("editor-window");
        std::fs::create_dir_all(root.join("mod")).unwrap();
        std::fs::write(root.join("mod/main.tf"), "").unwrap();
        let app = tauri::test::mock_builder().build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
        app.manage(Windows::default());
        app.state::<Windows>().set_root("main", &root);
        let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default()).build().unwrap();
        let open_from = |w: &tauri::Webview<_>, p: &std::path::Path| open_file_editor(w.clone(), app.state(), p.to_string_lossy().into());
        let open = |p: &std::path::Path| open_from(main.as_ref(), p);

        let file = root.join("mod/main.tf");
        open(&file).unwrap();
        let label = label_for("main", &file.to_string_lossy());
        let editor = app.get_webview_window(&label).expect("an editor window");
        // Again (even by another spelling of the path, or from the editor): the same window.
        open(&root.join("mod/../mod/main.tf")).unwrap();
        open_from(editor.as_ref(), &file).unwrap();
        assert_eq!(app.webview_windows().len(), 2);

        // Two links to the same file are two windows: each plans in its own folder.
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::os::unix::fs::symlink(&file, root.join("other/main.tf")).unwrap();
        open(&root.join("other/main.tf")).unwrap();
        assert_eq!(app.webview_windows().len(), 3);

        // A window without a folder open can't open files.
        let other = tauri::WebviewWindowBuilder::new(&app, "workspace-1", Default::default()).build().unwrap();
        assert_eq!(open_from(other.as_ref(), &file).unwrap_err(), "No folder is open.");

        assert!(open(&root.join("mod")).unwrap_err().contains("is not a file inside"));
        // A window that can't be made (its label is taken) reports why.
        assert!(!create_window(app.handle(), &label, "x", String::new()).unwrap_err().is_empty());
    }
}
