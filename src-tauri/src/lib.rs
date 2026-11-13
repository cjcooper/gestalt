mod clean;
mod cloud;
mod fs;
mod git;
mod module_info;
mod order;
mod shell_env;
mod system_colors;
mod tofu;
mod watch;
mod windows;

use shell_env::{EnvStatus, ShellEnv};
use std::ffi::{OsStr, OsString};
use std::path::Path;
use tauri::{Manager, Runtime, State, Webview};
use windows::Windows;


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

/// Folder a window opens with. A window opened for a folder (`gestalt
/// <folder>` while running) gets that one, and any window keeps its folder
/// across a reload. The launch window gets a folder passed on the command
/// line, else `GESTALT_ROOT` from the app env or the login shell. `None` lets
/// the UI fall back to the last opened folder (launch window) or the welcome
/// panel.
#[tauri::command]
async fn initial_root<R: Runtime>(
    webview: Webview<R>,
    windows: State<'_, Windows>,
    shell_env: State<'_, ShellEnv>,
) -> Result<Option<InitialRoot>, String> {
    if let Some(root) = windows.root_for(webview.label()) {
        return Ok(Some(InitialRoot { path: root.to_string_lossy().into_owned(), source: "command line" }));
    }
    if webview.label() != windows::MAIN {
        return Ok(None);
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    Ok(find_initial_root(args.into_iter(), std::env::var_os("GESTALT_ROOT"), std::env::var_os("HOME"), &shell_env).await)
}

/// [`initial_root`] with the process args, app `GESTALT_ROOT` and `HOME` passed in.
async fn find_initial_root(
    mut args: impl Iterator<Item = OsString>,
    app_root: Option<OsString>,
    home: Option<OsString>,
    shell_env: &ShellEnv,
) -> Option<InitialRoot> {
    // `~/...` expanded, then only if it is an existing directory.
    let usable = |p: OsString| windows::folder_arg(p, None, home.as_deref());
    let configured = |v: Option<OsString>| v.filter(|v| !v.is_empty()).and_then(usable);

    // macOS may pass non-path args (e.g. -psn_...) when launched from Finder.
    let found = if let Some(p) = args.find_map(&usable) {
        Some((p, "command line"))
    } else if let Some(p) = configured(app_root) {
        Some((p, "GESTALT_ROOT"))
    } else {
        // Only now pay for loading the login shell.
        let shell_root = shell_env.get().await.get(OsStr::new("GESTALT_ROOT")).cloned();
        configured(shell_root).map(|p| (p, "GESTALT_ROOT (login shell)"))
    };
    found.map(|(p, source)| InitialRoot { path: p.to_string_lossy().into_owned(), source })
}

#[tauri::command]
fn list_dir(path: String) -> Result<Vec<fs::DirEntry>, String> {
    fs::list_dir(Path::new(&path))
}

/// Record the calling window's folder, watch it, and describe it.
#[tauri::command]
fn open_root<R: Runtime>(webview: Webview<R>, windows: State<'_, Windows>, path: String) -> Result<fs::DirEntry, String> {
    let path = Path::new(&path);
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    let window = webview.label().to_string();
    let previous = windows.set_root(&window, path);
    // Also called on every tree refresh: only (re)start watching for a new root.
    if previous.as_deref() != Some(path) {
        tauri::async_runtime::spawn(watch::start(webview.app_handle().clone(), window, path.to_path_buf()));
    }
    Ok(fs::entry(path))
}

// Last, so the `#[tauri::command]` macros above are in scope there.
mod app;
pub use app::run;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::shell_env::EnvMap;
    use crate::shell_env::testing::{fake_shell, ipc, temp_dir, webview};
    use tauri::Manager;
    use tauri::async_runtime::block_on;

    fn shell_with_root(root: Option<&Path>) -> ShellEnv {
        let env: EnvMap = root.map(|r| ("GESTALT_ROOT".into(), r.into())).into_iter().collect();
        ShellEnv::preloaded(env)
    }

    fn find(args: &[&OsStr], app_root: Option<&OsStr>, home: Option<&Path>, shell: &ShellEnv) -> Option<(String, &'static str)> {
        let args = args.iter().map(|a| a.to_os_string()).collect::<Vec<_>>().into_iter();
        let root = block_on(find_initial_root(args, app_root.map(Into::into), home.map(Into::into), shell));
        root.map(|r| (r.path, r.source))
    }

    #[test]
    fn initial_root_prefers_args_then_app_env_then_login_shell() {
        let dir = temp_dir("lib-initial-root");
        let (a, b, c) = (dir.join("a"), dir.join("b"), dir.join("c"));
        for d in [&a, &b, &c] {
            std::fs::create_dir(d).unwrap();
        }
        let file = dir.join("file");
        std::fs::write(&file, "").unwrap();
        let shell = shell_with_root(Some(&c));
        let s = |p: &Path| p.to_string_lossy().into_owned();

        // The first usable arg wins; Finder's -psn_ args and files are skipped.
        assert_eq!(
            find(&["-psn_0_123".as_ref(), file.as_ref(), a.as_ref(), b.as_ref()], Some(b.as_ref()), None, &shell),
            Some((s(&a), "command line"))
        );
        assert_eq!(find(&[], Some(b.as_ref()), None, &shell), Some((s(&b), "GESTALT_ROOT")));
        // Empty or unusable app values fall through to the login shell.
        assert_eq!(find(&[], Some("".as_ref()), None, &shell), Some((s(&c), "GESTALT_ROOT (login shell)")));
        assert_eq!(find(&[], Some(file.as_ref()), None, &shell), Some((s(&c), "GESTALT_ROOT (login shell)")));
        // Nothing configured anywhere.
        assert_eq!(find(&[], None, None, &shell_with_root(None)), None);
        assert_eq!(find(&[], None, None, &shell_with_root(Some(Path::new("")))), None);
    }

    #[test]
    fn initial_root_expands_home() {
        let home = temp_dir("lib-initial-root-home");
        std::fs::create_dir(home.join("infra")).unwrap();
        let shell = shell_with_root(None);
        let infra = home.join("infra").to_string_lossy().into_owned();
        assert_eq!(find(&["~/infra".as_ref()], None, Some(&home), &shell), Some((infra.clone(), "command line")));
        assert_eq!(find(&[], Some("~/infra".as_ref()), Some(&home), &shell), Some((infra, "GESTALT_ROOT")));
        // Without HOME, `~` is taken literally.
        assert_eq!(find(&["~/infra".as_ref()], None, None, &shell), None);
    }

    fn app(shell: ShellEnv) -> tauri::App<tauri::test::MockRuntime> {
        tauri::test::mock_builder()
            .manage(shell)
            .manage(Windows::default())
            .manage(watch::WorkspaceWatcher::default())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap()
    }

    fn view(app: &tauri::App<tauri::test::MockRuntime>, label: &str) -> Webview<tauri::test::MockRuntime> {
        let w = tauri::WebviewWindowBuilder::new(app, label, Default::default()).build().unwrap();
        w.as_ref().clone()
    }

    #[test]
    fn initial_root_command_reads_the_process() {
        let dir = temp_dir("lib-initial-root-cmd");
        let app = app(shell_with_root(Some(&dir)));
        // Whatever the test binary's own args and env hold, the command
        // resolves them like `find_initial_root` does.
        let root = block_on(initial_root(view(&app, "main"), app.state(), app.state())).unwrap().expect("a root");
        let expected = block_on(find_initial_root(
            std::env::args_os().skip(1),
            std::env::var_os("GESTALT_ROOT"),
            std::env::var_os("HOME"),
            &app.state::<ShellEnv>(),
        ))
        .unwrap();
        assert_eq!((root.path, root.source), (expected.path, expected.source));
    }

    #[test]
    fn other_windows_open_their_own_folder_or_none() {
        let dir = temp_dir("lib-initial-root-window");
        let app = app(shell_with_root(Some(&dir)));
        let other = view(&app, "workspace-1");
        assert!(block_on(initial_root(other.clone(), app.state(), app.state())).unwrap().is_none());

        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        app.state::<Windows>().set_root("workspace-1", &sub);
        let root = block_on(initial_root(other, app.state(), app.state())).unwrap().unwrap();
        assert_eq!((root.path, root.source), (sub.to_string_lossy().into_owned(), "command line"));
    }

    #[test]
    fn shell_env_commands_report_and_reload() {
        let dir = temp_dir("lib-shell-env");
        let app = app(ShellEnv::with_shell(fake_shell(&dir, &["A=1", "B=2"])));
        let status = block_on(shell_env_status(app.state())).unwrap();
        assert_eq!((status.var_count, status.error), (2, None));

        fake_shell(&dir, &["A=1"]);
        assert_eq!(block_on(shell_env_status(app.state())).unwrap().var_count, 2);
        assert_eq!(block_on(reload_shell_env(app.state())).unwrap().var_count, 1);
    }

    #[test]
    fn list_dir_lists_visible_entries() {
        let dir = temp_dir("lib-list-dir");
        std::fs::create_dir(dir.join("mod")).unwrap();
        std::fs::write(dir.join("main.tf"), "").unwrap();
        std::fs::write(dir.join(".hidden"), "").unwrap();
        let names: Vec<_> = list_dir(dir.to_string_lossy().into()).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["mod", "main.tf"]);
        assert!(list_dir(dir.join("missing").to_string_lossy().into()).is_err());
    }

    #[test]
    fn open_root_sets_the_window_root() {
        let dir = temp_dir("lib-open-root");
        let app = app(shell_with_root(None));
        let main = view(&app, "main");
        let root = || app.state::<Windows>().root_for("main");
        let open = |p: &Path| open_root(main.clone(), app.state(), p.to_string_lossy().into());

        let missing = dir.join("missing");
        assert_eq!(open(&missing).unwrap_err(), format!("{} is not a directory", missing.display()));
        assert_eq!(root(), None);

        let entry = open(&dir).unwrap();
        assert_eq!(entry.path, dir.to_string_lossy());
        assert!(entry.is_dir);
        assert_eq!(root(), Some(dir.clone()));
        // Refreshing the same root keeps it.
        open(&dir).unwrap();
        assert_eq!(root(), Some(dir.clone()));

        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        open(&sub).unwrap();
        assert_eq!(root(), Some(sub));
    }

    /// The UI reaches these through IPC; check the generated handlers work.
    #[test]
    fn commands_are_invokable_over_ipc() {
        let dir = temp_dir("lib-ipc");
        let build = |shell: Option<ShellEnv>| {
            let builder = tauri::test::mock_builder();
            let builder = match shell {
                Some(shell) => builder.manage(shell),
                None => builder,
            };
            builder
                .invoke_handler(tauri::generate_handler![shell_env_status, reload_shell_env, initial_root, list_dir, open_root])
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap()
        };
        let app = build(Some(shell_with_root(Some(&dir))));
        app.manage(Windows::default());
        app.manage(watch::WorkspaceWatcher::default());
        let view = webview(&app);
        let none = serde_json::json!({});

        assert_eq!(ipc(&view, "shell_env_status", none.clone()).unwrap()["varCount"], 1);
        assert!(ipc(&view, "initial_root", none.clone()).unwrap()["path"].is_string());
        assert_eq!(ipc(&view, "open_root", serde_json::json!({"path": dir})).unwrap()["isDir"], true);
        let missing = dir.join("missing");
        assert_eq!(
            ipc(&view, "list_dir", serde_json::json!({"path": missing})).unwrap_err(),
            serde_json::json!(format!("{} is not a directory", missing.display()))
        );
        // A preloaded env has no shell to reload from.
        let reloaded = ipc(&view, "reload_shell_env", none.clone()).unwrap();
        assert_eq!(reloaded["varCount"], 0);
        assert!(reloaded["error"].is_string());

        // Without the state they need, commands report an error.
        let app = build(None);
        let view = webview(&app);
        for cmd in ["shell_env_status", "reload_shell_env", "initial_root"] {
            assert!(ipc(&view, cmd, none.clone()).unwrap_err().as_str().unwrap().contains("state not managed"), "{cmd}");
        }
        let err = ipc(&view, "open_root", serde_json::json!({"path": dir})).unwrap_err();
        assert!(err.as_str().unwrap().contains("state not managed"), "{err}");
        // Windows, but no shell environment.
        app.manage(Windows::default());
        let err = ipc(&view, "initial_root", none.clone()).unwrap_err();
        assert!(err.as_str().unwrap().contains("state not managed"), "{err}");
    }
}
