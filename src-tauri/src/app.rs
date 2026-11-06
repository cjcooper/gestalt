//! Tauri wiring: builds and runs the app. Pure glue (no logic of its own), so
//! it is left out of the coverage target; everything it registers is tested
//! where it is defined.

use crate::{clean, env_edit, git, initial_root, list_dir, open_root, reload_shell_env, shell_env_status, system_colors, tofu, watch};
use crate::shell_env::ShellEnv;
use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::{AppHandle, Emitter};

/// The platform's standard menu plus a Workspace menu. Its items are handled
/// in the UI, which hears about them as `menu://<id>` events.
fn app_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::default(app)?;
    let clean = MenuItem::with_id(app, "clean-all", "Clean All .terraform Caches…", true, None::<&str>)?;
    let workspace = Submenu::with_items(app, "Workspace", true, &[&clean])?;
    // Before Window and Help, where macOS expects them to stay.
    let at = menu.items()?.len().saturating_sub(2);
    menu.insert(&workspace, at)?;
    Ok(menu)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(tofu::Runs::default())
        .manage(ShellEnv::default())
        .manage(tofu::Workspace::default())
        .manage(watch::WorkspaceWatcher::default())
        .menu(app_menu)
        .on_menu_event(|app, event| {
            if event.id() == "clean-all" {
                let _ = app.emit("menu://clean-all", ());
            }
        })
        .setup(|app| {
            tofu::clear_stale_plans(app.handle());
            system_colors::watch(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_dir,
            clean::clean_scan,
            clean::clean_caches,
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
