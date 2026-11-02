mod env_edit;
mod env_file;
mod fs;
mod git;
mod module_info;
mod order;
mod shell_env;
mod system_colors;
mod tofu;
mod watch;

use shell_env::{EnvStatus, ShellEnv};
use std::path::Path;
use tauri::{AppHandle, State};

#[tauri::command]
async fn shell_env_status(env: State<'_, ShellEnv>) -> Result<EnvStatus, String> {
    Ok(env.status().await)
}

#[tauri::command]
async fn reload_shell_env(env: State<'_, ShellEnv>) -> Result<EnvStatus, String> {
    Ok(env.reload().await)
}

#[derive(serde::Serialize)]
struct InitialRoot {
    path: String,
    /// Where it came from, shown to the user.
    source: &'static str,
}

/// Folder to open at startup, if one is configured: a directory passed on the
/// command line, else `GESTALT_ROOT` from the app env or the login shell.
/// Returns `None` to let the UI fall back to the last opened folder.
#[tauri::command]
async fn initial_root(shell_env: State<'_, ShellEnv>) -> Result<Option<InitialRoot>, String> {
    let expand = |p: std::ffi::OsString| {
        let p = std::path::PathBuf::from(p);
        match (p.strip_prefix("~"), std::env::var_os("HOME")) {
            (Ok(rest), Some(home)) => Path::new(&home).join(rest),
            _ => p,
        }
    };
    let usable = |p: std::path::PathBuf| p.canonicalize().ok().filter(|p| p.is_dir());

    // macOS may pass non-path args (e.g. -psn_...) when launched from Finder.
    let from_args = std::env::args_os().skip(1).map(expand).find_map(usable);
    let from_env = || {
        std::env::var_os("GESTALT_ROOT")
            .filter(|v| !v.is_empty())
            .map(expand)
            .and_then(usable)
    };
    let found = match from_args {
        Some(p) => Some((p, "command line")),
        None => match from_env() {
            Some(p) => Some((p, "GESTALT_ROOT")),
            None => shell_env
                .get()
                .await
                .get(std::ffi::OsStr::new("GESTALT_ROOT"))
                .filter(|v| !v.is_empty())
                .cloned()
                .map(expand)
                .and_then(usable)
                .map(|p| (p, "GESTALT_ROOT (login shell)")),
        },
    };
    Ok(found.map(|(p, source)| InitialRoot { path: p.to_string_lossy().into_owned(), source }))
}

#[tauri::command]
fn list_dir(path: String) -> Result<Vec<fs::DirEntry>, String> {
    fs::list_dir(Path::new(&path))
}

/// Set the workspace root (bounds `.gestalt.env` lookup) and describe it.
#[tauri::command]
fn open_root(app: AppHandle, workspace: State<'_, tofu::Workspace>, path: String) -> Result<fs::DirEntry, String> {
    let path = Path::new(&path);
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    let previous = workspace.root.lock().unwrap().replace(path.to_path_buf());
    // Also called on every tree refresh: only (re)start watching for a new root.
    if previous.as_deref() != Some(path) {
        tauri::async_runtime::spawn(watch::start(app, path.to_path_buf()));
    }
    Ok(fs::entry(path))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(tofu::Runs::default())
        .manage(ShellEnv::default())
        .manage(tofu::Workspace::default())
        .manage(watch::WorkspaceWatcher::default())
        .setup(|app| {
            tofu::clear_stale_plans(app.handle());
            system_colors::watch(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_dir,
            open_root,
            initial_root,
            shell_env_status,
            reload_shell_env,
            system_colors::system_colors,
            env_edit::env_file_read,
            env_edit::env_file_validate,
            env_edit::env_file_save,
            env_edit::env_file_delete,
            git::git_log,
            git::git_is_repo,
            git::git_uncommitted,
            tofu::run_plan,
            tofu::run_import,
            tofu::module_info,
            tofu::apply_plan,
            tofu::discard_plan,
            tofu::cancel_tofu,
            tofu::tofu_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
