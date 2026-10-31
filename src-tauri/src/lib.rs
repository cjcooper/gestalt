mod env_file;
mod fs;
mod shell_env;
mod tofu;

use shell_env::{EnvStatus, ShellEnv};
use std::path::Path;
use tauri::State;

#[tauri::command]
async fn shell_env_status(env: State<'_, ShellEnv>) -> Result<EnvStatus, String> {
    Ok(env.status().await)
}

#[tauri::command]
async fn reload_shell_env(env: State<'_, ShellEnv>) -> Result<EnvStatus, String> {
    Ok(env.reload().await)
}

#[tauri::command]
fn list_dir(path: String) -> Result<Vec<fs::DirEntry>, String> {
    fs::list_dir(Path::new(&path))
}

/// Set the workspace root (bounds `.gestalt.env` lookup) and describe it.
#[tauri::command]
fn open_root(workspace: State<'_, tofu::Workspace>, path: String) -> Result<fs::DirEntry, String> {
    let path = Path::new(&path);
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    *workspace.root.lock().unwrap() = Some(path.to_path_buf());
    Ok(fs::entry(path))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(tofu::Runs::default())
        .manage(ShellEnv::default())
        .manage(tofu::Workspace::default())
        .setup(|app| {
            tofu::clear_stale_plans(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_dir,
            open_root,
            shell_env_status,
            reload_shell_env,
            tofu::run_tofu,
            tofu::apply_plan,
            tofu::discard_plan,
            tofu::cancel_tofu,
            tofu::tofu_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
