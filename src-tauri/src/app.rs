//! Tauri wiring: builds and runs the app. Pure glue (no logic of its own), so
//! it is left out of the coverage target; everything it registers is tested
//! where it is defined.

use crate::{clean, env_edit, git, initial_root, list_dir, open_root, reload_shell_env, shell_env_status, system_colors, tofu, watch};
use crate::shell_env::ShellEnv;
use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// The platform's standard menu plus a Workspace menu. Workspace items are
/// handled in the UI, which hears about them as `menu://<id>` events.
///
/// The standard About item (app menu on macOS, Help elsewhere) is swapped
/// for one that opens Gestalt's own About window, which also lists the
/// third-party licenses.
fn app_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::default(app)?;
    let about = MenuItem::with_id(app, ABOUT_ID, "About Gestalt", true, None::<&str>)?;
    for sub in menu.items()?.iter().filter_map(|i| i.as_submenu()) {
        let items = sub.items()?;
        let found = items.iter().position(|i| {
            i.as_predefined_menuitem()
                .and_then(|p| p.text().ok())
                .is_some_and(|t| t.starts_with("About"))
        });
        if let Some(at) = found {
            sub.remove_at(at)?;
            sub.insert(&about, at)?;
        }
    }
    let clean = MenuItem::with_id(app, "clean-all", "Clean All .terraform Caches…", true, None::<&str>)?;
    let workspace = Submenu::with_items(app, "Workspace", true, &[&clean])?;
    // Before Window and Help, where macOS expects them to stay.
    let at = menu.items()?.len().saturating_sub(2);
    menu.insert(&workspace, at)?;
    Ok(menu)
}

const ABOUT_ID: &str = "about";

/// Open the About window (version, third-party licenses), or bring it
/// forward if it's already open. Its own window, so closing it never
/// closes the app. Label must match `capabilities/about.json`.
fn show_about(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(ABOUT_ID) {
        let _ = window.unminimize();
        return window.set_focus();
    }
    WebviewWindowBuilder::new(app, ABOUT_ID, WebviewUrl::App("about.html".into()))
        .title("About Gestalt")
        .inner_size(640.0, 640.0)
        .min_inner_size(480.0, 420.0)
        .minimizable(false)
        .build()?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(tofu::Runs::default())
        .manage(ShellEnv::default())
        .manage(tofu::Workspace::default())
        .manage(watch::WorkspaceWatcher::default())
        .menu(app_menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "clean-all" => {
                let _ = app.emit("menu://clean-all", ());
            }
            ABOUT_ID => {
                if let Err(e) = show_about(app) {
                    eprintln!("about window: {e}");
                }
            }
            _ => {}
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
