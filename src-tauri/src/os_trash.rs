//! The one call that actually moves something to the system Trash. Kept in
//! its own file, out of the coverage target: tests can't call it without
//! filling the developer's real Trash. The rules around it (what may be
//! deleted) live in `delete.rs`, under test.

use crate::delete;
use crate::tofu::Workspace;
use std::path::Path;
use tauri::State;

fn move_to_os_trash(path: &Path) -> Result<(), String> {
    let mut trash = trash::TrashContext::default();
    // NSFileManager: no Finder automation permission prompt.
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        trash.set_delete_method(DeleteMethod::NsFileManager);
    }
    trash.delete(path).map_err(|e| e.to_string())
}

/// Right-click › Move to Trash…: a file or folder inside the workspace.
#[tauri::command]
pub fn move_to_trash(workspace: State<'_, Workspace>, path: String) -> Result<(), String> {
    delete::trash_path(&workspace, &path, move_to_os_trash)
}
