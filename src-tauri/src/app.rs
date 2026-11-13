//! Tauri wiring: builds and runs the app. Pure glue (no logic of its own), so
//! it is left out of the coverage target; everything it registers is tested
//! where it is defined.

use crate::cloud::{self, CloudSettings};
use crate::shell_env::ShellEnv;
use crate::windows::{self, Windows};
use crate::{clean, git, initial_root, list_dir, open_root, reload_shell_env, shell_env_status, system_colors, tofu, watch};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// The platform's standard menu plus a Workspace menu. Workspace items other
/// than New Window are handled in the UI of the window in front, which hears
/// about them as `menu://<id>` events.
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
    let new_window = MenuItem::with_id(app, NEW_WINDOW_ID, "New Window", true, Some("CmdOrCtrl+Shift+N"))?;
    let cloud = MenuItem::with_id(app, "cloud-settings", "Cloud Settings…", true, None::<&str>)?;
    let clean = MenuItem::with_id(app, "clean-all", "Clean All .terraform Caches…", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let workspace = Submenu::with_items(app, "Workspace", true, &[&new_window, &separator, &cloud, &clean])?;
    // Before Window and Help, where macOS expects them to stay.
    let at = menu.items()?.len().saturating_sub(2);
    menu.insert(&workspace, at)?;
    Ok(menu)
}

const ABOUT_ID: &str = "about";
const NEW_WINDOW_ID: &str = "new-window";

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
        // First, so a second launch exits before doing anything else.
        .plugin(tauri_plugin_single_instance::init(windows::second_instance))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(tofu::Runs::default())
        .manage(ShellEnv::default())
        .manage(Windows::default())
        .manage(watch::WorkspaceWatcher::default())
        .menu(app_menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            id @ ("clean-all" | "cloud-settings") => {
                if let Some(window) = windows::focused(app) {
                    let _ = app.emit_to(window.as_str(), &format!("menu://{id}"), ());
                }
            }
            NEW_WINDOW_ID => windows::open_folders(app, Vec::new()),
            ABOUT_ID => {
                if let Err(e) = show_about(app) {
                    eprintln!("about window: {e}");
                }
            }
            _ => {}
        })
        .on_window_event(|window, event| {
            if let WindowEvent::Destroyed = event {
                windows::closed(window.app_handle(), window.label());
            }
        })
        .setup(|app| {
            let config = app.path().app_config_dir()?;
            app.manage(CloudSettings::load(config.join(cloud::FILE_NAME)));
            tofu::clear_stale_plans(app.handle());
            system_colors::watch(app.handle());
            // The launch window takes the first folder named; the rest get their own.
            let home = std::env::var_os("HOME");
            for dir in windows::folder_args(std::env::args_os().skip(1), None, home.as_deref()).into_iter().skip(1) {
                windows::open(app.handle(), Some(dir))?;
            }
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
            cloud::cloud_get,
            cloud::cloud_set,
            cloud::cloud_effective,
            cloud::cloud_folders,
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
