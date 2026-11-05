mod clean;
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
use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

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
        .on_menu_event(|app, event| {
            match event.id().as_ref() {
                "clean-all" => {
                    let _ = app.emit("menu://clean-all", ());
                }
                ABOUT_ID => {
                    if let Err(e) = show_about(app) {
                        eprintln!("about window: {e}");
                    }
                }
                _ => {}
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
