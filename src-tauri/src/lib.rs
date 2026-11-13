mod clean;
mod delete;
mod editor_window;
mod env_edit;
mod env_file;
mod file_edit;
mod fs;
mod git;
mod module_info;
mod order;
mod os_trash;
mod shell_env;
mod system_colors;
mod tofu;
mod watch;

use shell_env::{EnvStatus, ShellEnv};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Runtime, State};


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
    let usable = |p: OsString| {
        let p = PathBuf::from(p);
        let p = match (p.strip_prefix("~"), &home) {
            (Ok(rest), Some(home)) => Path::new(home).join(rest),
            _ => p,
        };
        p.canonicalize().ok().filter(|p| p.is_dir())
    };
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

/// Set the workspace root (bounds `.gestalt.env` lookup) and describe it.
#[tauri::command]
fn open_root<R: Runtime>(app: AppHandle<R>, workspace: State<'_, tofu::Workspace>, path: String) -> Result<fs::DirEntry, String> {
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
            .manage(tofu::Workspace::default())
            .manage(watch::WorkspaceWatcher::default())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap()
    }

    #[test]
    fn initial_root_command_reads_the_process() {
        let dir = temp_dir("lib-initial-root-cmd");
        let app = app(shell_with_root(Some(&dir)));
        // Whatever the test binary's own args and env hold, the command
        // resolves them like `find_initial_root` does.
        let root = block_on(initial_root(app.state())).unwrap().expect("a root");
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
    fn open_root_sets_the_workspace_root() {
        let dir = temp_dir("lib-open-root");
        let app = app(shell_with_root(None));
        let root = || app.state::<tofu::Workspace>().root.lock().unwrap().clone();
        let open = |p: &Path| open_root(app.handle().clone(), app.state(), p.to_string_lossy().into());

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
                .invoke_handler(tauri::generate_handler![shell_env_status, reload_shell_env, initial_root, list_dir])
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap()
        };
        let app = build(Some(shell_with_root(Some(&dir))));
        let view = webview(&app);
        let none = serde_json::json!({});

        assert_eq!(ipc(&view, "shell_env_status", none.clone()).unwrap()["varCount"], 1);
        assert!(ipc(&view, "initial_root", none.clone()).unwrap()["path"].is_string());
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
    }
}
