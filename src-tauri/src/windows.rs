//! Several windows, each with its own open folder.
//!
//! Running `gestalt <folder>` while Gestalt is already open starts a second
//! process, which the single-instance plugin stops after handing its
//! arguments to [`second_instance`]: each folder opens in a new window, or
//! brings forward the window that already has it open.

use crate::{tofu, watch};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// The window from `tauri.conf.json`, opened at launch.
pub const MAIN: &str = "main";
/// Label prefix for windows opened later. Must match `capabilities/default.json`.
const PREFIX: &str = "workspace-";

#[derive(Default)]
pub struct Windows {
    next: AtomicU64,
    /// The folder each window has open.
    roots: Mutex<HashMap<String, PathBuf>>,
    /// The folder a new window was opened for, until it opens it.
    pending: Mutex<HashMap<String, PathBuf>>,
}

impl Windows {
    /// Record `window`'s folder; returns the one it had before.
    pub fn set_root(&self, window: &str, root: &Path) -> Option<PathBuf> {
        self.roots.lock().unwrap().insert(window.into(), root.into())
    }

    /// The folder `window` should open: the one it has open (if its page
    /// reloads), else the one it was created for.
    pub fn root_for(&self, window: &str) -> Option<PathBuf> {
        let open = self.roots.lock().unwrap().get(window).cloned();
        open.or_else(|| self.pending.lock().unwrap().get(window).cloned())
    }

    fn window_with(&self, root: &Path) -> Option<String> {
        let open = self.roots.lock().unwrap().iter().find(|(_, r)| *r == root).map(|(w, _)| w.clone());
        open.or_else(|| self.pending.lock().unwrap().iter().find(|(_, r)| *r == root).map(|(w, _)| w.clone()))
    }

    fn forget(&self, window: &str) {
        self.roots.lock().unwrap().remove(window);
        self.pending.lock().unwrap().remove(window);
    }
}

/// A command-line argument as a folder: `~/` expanded, relative paths taken
/// from `cwd` (or the process's own), and only if it is an existing folder.
/// Other arguments (e.g. macOS's `-psn_…`, or files) give `None`.
pub fn folder_arg(arg: impl AsRef<OsStr>, cwd: Option<&Path>, home: Option<&OsStr>) -> Option<PathBuf> {
    let p = Path::new(arg.as_ref());
    let p = match (p.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => Path::new(home).join(rest),
        _ => cwd.map(|c| c.join(p)).unwrap_or_else(|| p.to_path_buf()),
    };
    p.canonicalize().ok().filter(|p| p.is_dir())
}

/// The folders named on a command line (`args` without the program name).
pub fn folder_args<I: IntoIterator<Item = S>, S: AsRef<OsStr>>(args: I, cwd: Option<&Path>, home: Option<&OsStr>) -> Vec<PathBuf> {
    args.into_iter().filter_map(|a| folder_arg(a, cwd, home)).collect()
}

/// A new window, opening `root` if given (else the welcome panel).
pub fn open<R: Runtime>(app: &AppHandle<R>, root: Option<PathBuf>) -> tauri::Result<WebviewWindow<R>> {
    let windows = app.state::<Windows>();
    let label = format!("{PREFIX}{}", windows.next.fetch_add(1, Ordering::SeqCst) + 1);
    if let Some(root) = root {
        windows.pending.lock().unwrap().insert(label.clone(), root);
    }
    WebviewWindowBuilder::new(app, &label, WebviewUrl::App("index.html".into()))
        .title("Gestalt")
        .inner_size(1200.0, 800.0)
        .min_inner_size(700.0, 400.0)
        .build()
        .inspect_err(|_| windows.forget(&label))
}

fn focus<R: Runtime>(window: &WebviewWindow<R>) {
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

fn is_workspace(label: &str) -> bool {
    label == MAIN || label.starts_with(PREFIX)
}

/// Bring forward the window that has each folder open, or open one for it.
/// With no folders, bring a window forward (or open an empty one).
pub fn open_folders<R: Runtime>(app: &AppHandle<R>, dirs: Vec<PathBuf>) {
    let windows = app.state::<Windows>();
    if dirs.is_empty() {
        let mut open: Vec<_> = app.webview_windows().into_iter().filter(|(l, _)| is_workspace(l)).collect();
        open.sort_by(|a, b| a.0.cmp(&b.0));
        match open.first() {
            Some((_, w)) => focus(w),
            None => report(open_window(app, None)),
        }
    }
    for dir in dirs {
        match windows.window_with(&dir).and_then(|l| app.get_webview_window(&l)) {
            Some(w) => focus(&w),
            None => report(open_window(app, Some(dir))),
        }
    }
}

fn open_window<R: Runtime>(app: &AppHandle<R>, root: Option<PathBuf>) -> tauri::Result<()> {
    open(app, root).map(|_| ())
}

fn report(result: tauri::Result<()>) {
    if let Err(e) = result {
        eprintln!("new window: {e}");
    }
}

/// The single-instance plugin's callback: `args` and `cwd` of the launch it stopped.
pub fn second_instance<R: Runtime>(app: &AppHandle<R>, args: Vec<String>, cwd: String) {
    let home = std::env::var_os("HOME");
    let dirs = folder_args(args.iter().skip(1), Some(Path::new(&cwd)), home.as_deref());
    open_folders(app, dirs);
}

/// The window in front, if it is a workspace window (not e.g. About).
pub fn focused<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    app.webview_windows()
        .into_iter()
        .find(|(label, w)| is_workspace(label) && w.is_focused().unwrap_or(false))
        .map(|(label, _)| label)
}

/// `window` closed: stop watching its folder, cancel its runs and drop its plans.
pub fn closed<R: Runtime>(app: &AppHandle<R>, window: &str) {
    app.state::<Windows>().forget(window);
    watch::stop(app, window);
    tofu::window_closed(&app.state::<tofu::Runs>(), window);
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::shell_env::testing::temp_dir;
    use crate::shell_env::ShellEnv;

    fn app() -> tauri::App<tauri::test::MockRuntime> {
        let app = tauri::test::mock_app();
        app.manage(Windows::default());
        app.manage(watch::WorkspaceWatcher::default());
        app.manage(tofu::Runs::default());
        app.manage(ShellEnv::preloaded(Default::default()));
        app
    }

    fn labels(app: &tauri::App<tauri::test::MockRuntime>) -> Vec<String> {
        let mut l: Vec<_> = app.webview_windows().into_keys().collect();
        l.sort();
        l
    }

    #[test]
    fn folder_args_resolve_relative_and_home_paths() {
        let base = temp_dir("windows-args");
        std::fs::create_dir_all(base.join("infra/a")).unwrap();
        std::fs::write(base.join("file"), "").unwrap();
        let home = Some(base.as_os_str());
        let infra = base.join("infra");

        assert_eq!(folder_arg("infra", Some(&base), None), Some(infra.clone()));
        assert_eq!(folder_arg(&infra, Some(Path::new("/elsewhere")), None), Some(infra.clone()));
        assert_eq!(folder_arg("~/infra/a", None, home), Some(infra.join("a")));
        assert_eq!(folder_arg(&base, None, None), Some(base.clone()));
        // Without HOME, `~` is a relative name.
        assert_eq!(folder_arg("~/infra", Some(&base), None), None);
        assert_eq!(
            folder_args(["-psn_0_1", "file", "missing", "infra", "infra/a"], Some(&base), home),
            [infra.clone(), infra.join("a")]
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn second_launch_opens_or_focuses_windows() {
        let base = temp_dir("windows-open");
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let app = app();
        let h = app.handle();
        let windows = app.state::<Windows>();
        let arg = |p: &Path| p.to_string_lossy().into_owned();

        // No windows at all and no folder: an empty window.
        second_instance(h, vec!["gestalt".into()], "/".into());
        assert_eq!(labels(&app), ["workspace-1"]);
        assert_eq!(windows.root_for("workspace-1"), None);

        // A new window per folder; relative to the launch's folder.
        second_instance(h, vec!["gestalt".into(), "a".into(), "-psn_1".into(), arg(&b)], arg(&base));
        assert_eq!(labels(&app), ["workspace-1", "workspace-2", "workspace-3"]);
        assert_eq!(windows.root_for("workspace-2"), Some(a.clone()));
        assert_eq!(windows.root_for("workspace-3"), Some(b.clone()));

        // A folder already open (or about to be) is brought forward instead.
        windows.set_root("workspace-2", &a);
        second_instance(h, vec!["gestalt".into(), arg(&a), arg(&b)], "/".into());
        // No folder: the first window comes forward.
        second_instance(h, vec!["gestalt".into(), "missing".into()], "/".into());
        assert_eq!(labels(&app), ["workspace-1", "workspace-2", "workspace-3"]);

        // A window that opens another folder keeps it across a reload.
        assert_eq!(windows.set_root("workspace-3", &a), None);
        assert_eq!(windows.root_for("workspace-3"), Some(a.clone()));
        assert_eq!(windows.set_root("workspace-3", &b), Some(a.clone()));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn closing_forgets_the_window() {
        let base = temp_dir("windows-close");
        let app = app();
        let h = app.handle();
        let w = open(h, Some(base.clone())).unwrap();
        let windows = app.state::<Windows>();
        windows.set_root(w.label(), &base);
        closed(h, w.label());
        assert_eq!(windows.root_for(w.label()), None);
        assert_eq!(windows.window_with(&base), None);
        // Mock windows never have focus.
        assert_eq!(focused(h), None);
        assert!(is_workspace(MAIN) && is_workspace("workspace-9") && !is_workspace("about"));
        report(Ok(()));
        report(Err(tauri::Error::WindowNotFound));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_window_that_cannot_open_is_forgotten() {
        let app = app();
        let h = app.handle();
        // Same label as an existing window: the builder refuses.
        tauri::WebviewWindowBuilder::new(h, "workspace-1", Default::default()).build().unwrap();
        assert!(open(h, Some(PathBuf::from("/x"))).is_err());
        assert_eq!(app.state::<Windows>().root_for("workspace-1"), None);
    }
}
