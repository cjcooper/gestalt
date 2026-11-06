//! Reading and writing `.gestalt.env` files from the editor.
//!
//! Writes are confined to files named `.gestalt.env` in folders inside the
//! opened workspace root, and are refused if the file changed on disk since
//! the editor loaded it.

use crate::env_file::{self, FILE_NAME};
use crate::shell_env::{EnvMap, ShellEnv};
use crate::tofu::Workspace;
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tauri::State;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inherited {
    name: String,
    /// Value after all parent files (and the shell env they may expand).
    value: String,
    /// Parent file (relative to the root) that last set it.
    source: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unset {
    name: String,
    source: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvFileContext {
    /// Path relative to the root, e.g. `accounts/prd/.gestalt.env`.
    rel_path: String,
    exists: bool,
    /// Current content; empty when the file doesn't exist.
    content: String,
    /// Variables set by parent `.gestalt.env` files, i.e. what this file overrides.
    inherited: Vec<Inherited>,
    inherited_unset: Vec<Unset>,
    /// Problems in parent files, which would also fail runs here.
    parent_errors: Vec<String>,
}

/// Canonical `dir` and root, ensuring `dir` is inside the root.
fn locate(workspace: &Workspace, dir: &str) -> Result<(PathBuf, PathBuf), String> {
    let root = workspace.root.lock().unwrap().clone().ok_or("No folder is open.")?;
    let root = root.canonicalize().map_err(|e| format!("{}: {e}", root.display()))?;
    let dir = Path::new(dir).canonicalize().map_err(|e| format!("{dir}: {e}"))?;
    if !dir.is_dir() || !dir.starts_with(&root) {
        return Err(format!("{} is not a folder inside {}", dir.display(), root.display()));
    }
    Ok((dir, root))
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

/// Content of `path`, or `None` if it doesn't exist.
fn read_existing(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(c) => Ok(Some(c)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Refuse to write if the file on disk isn't what the editor loaded.
fn check_unchanged(path: &Path, expected: &Option<String>) -> Result<(), String> {
    if read_existing(path)? != *expected {
        return Err(format!(
            "{} was changed on disk since you opened it. Close the editor and reopen it to see the current version.",
            path.display()
        ));
    }
    Ok(())
}

async fn base_env(shell: &ShellEnv) -> EnvMap {
    let mut env: EnvMap = std::env::vars_os().collect();
    env.extend(shell.get().await.iter().map(|(k, v)| (k.clone(), v.clone())));
    env
}

#[tauri::command]
pub async fn env_file_read(
    workspace: State<'_, Workspace>,
    shell: State<'_, ShellEnv>,
    dir: String,
) -> Result<EnvFileContext, String> {
    let (dir, root) = locate(&workspace, &dir)?;
    let path = dir.join(FILE_NAME);
    let content = read_existing(&path)?;

    let mut env = base_env(&shell).await;
    let mut set_by: BTreeMap<String, String> = BTreeMap::new();
    let mut unset_by: BTreeMap<String, String> = BTreeMap::new();
    let mut parent_errors = Vec::new();
    for file in env_file::chain(Some(&root), &dir).into_iter().filter(|f| *f != path) {
        let source = rel(&root, &file);
        let effect = std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|c| env_file::apply(&c, &mut env));
        match effect {
            Ok(effect) => {
                for name in effect.set {
                    unset_by.remove(&name);
                    set_by.insert(name, source.clone());
                }
                for name in effect.unset {
                    set_by.remove(&name);
                    unset_by.insert(name, source.clone());
                }
            }
            Err(e) => parent_errors.push(format!("{source}: {e}")),
        }
    }

    let inherited = set_by
        .into_iter()
        .map(|(name, source)| Inherited {
            value: env.get(&OsString::from(&name)).map(|v| v.to_string_lossy().into_owned()).unwrap_or_default(),
            name,
            source,
        })
        .collect();
    let inherited_unset = unset_by.into_iter().map(|(name, source)| Unset { name, source }).collect();

    Ok(EnvFileContext {
        rel_path: rel(&root, &path),
        exists: content.is_some(),
        content: content.unwrap_or_default(),
        inherited,
        inherited_unset,
        parent_errors,
    })
}

/// Syntax-check editor content; returns the first error, e.g. "line 3: ...".
#[tauri::command]
pub fn env_file_validate(content: String) -> Option<String> {
    env_file::apply(&content, &mut EnvMap::new()).err()
}

/// Write `.gestalt.env` in `dir`. `expected` is the content the editor loaded
/// (`None` if the file didn't exist); the write is refused if it differs.
#[tauri::command]
pub fn env_file_save(
    workspace: State<'_, Workspace>,
    dir: String,
    content: String,
    expected: Option<String>,
) -> Result<(), String> {
    let (dir, _) = locate(&workspace, &dir)?;
    let path = dir.join(FILE_NAME);
    env_file::apply(&content, &mut EnvMap::new()).map_err(|e| format!("Not saved — {e}"))?;
    check_unchanged(&path, &expected)?;

    let is_symlink = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
    if is_symlink {
        // Write through the link rather than replacing it with a plain file.
        return std::fs::write(&path, content).map_err(|e| e.to_string());
    }
    // Write-then-rename so a crash never leaves a half-written file.
    let tmp = dir.join(format!("{FILE_NAME}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, content).and_then(|()| std::fs::rename(&tmp, &path)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

#[tauri::command]
pub fn env_file_delete(workspace: State<'_, Workspace>, dir: String, expected: String) -> Result<(), String> {
    let (dir, _) = locate(&workspace, &dir)?;
    let path = dir.join(FILE_NAME);
    check_unchanged(&path, &Some(expected))?;
    std::fs::remove_file(&path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_writes_when_file_changed_on_disk() {
        let dir = std::env::temp_dir().join(format!("gestalt-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);

        // Editor opened a folder with no file: OK until someone creates one.
        assert!(check_unchanged(&path, &None).is_ok());
        std::fs::write(&path, "A=1\n").unwrap();
        assert!(check_unchanged(&path, &None).unwrap_err().contains("changed on disk"));

        // Editor loaded "A=1": OK until the content changes or the file goes away.
        assert!(check_unchanged(&path, &Some("A=1\n".into())).is_ok());
        std::fs::write(&path, "A=2\n").unwrap();
        assert!(check_unchanged(&path, &Some("A=1\n".into())).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(check_unchanged(&path, &Some("A=2\n".into())).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn locate_confines_to_workspace_root() {
        let base = std::env::temp_dir().join(format!("gestalt-locate-{}", std::process::id()));
        let inside = base.join("root/a");
        std::fs::create_dir_all(&inside).unwrap();
        let ws = Workspace::default();
        assert!(locate(&ws, inside.to_str().unwrap()).unwrap_err().contains("No folder is open"));

        *ws.root.lock().unwrap() = Some(base.join("root"));
        assert!(locate(&ws, inside.to_str().unwrap()).is_ok());
        assert!(locate(&ws, base.join("root").to_str().unwrap()).is_ok());
        assert!(locate(&ws, base.to_str().unwrap()).is_err());
        assert!(locate(&ws, base.join("root/a/../..").to_str().unwrap()).is_err());

        // Missing folder, and a root that has since gone away.
        let missing = base.join("root/missing");
        assert!(locate(&ws, missing.to_str().unwrap()).unwrap_err().starts_with(&*missing.to_string_lossy()));
        *ws.root.lock().unwrap() = Some(base.join("gone"));
        assert!(locate(&ws, inside.to_str().unwrap()).unwrap_err().starts_with(&*base.join("gone").to_string_lossy()));

        std::fs::remove_dir_all(&base).unwrap();
    }

    use tauri::Manager;

    /// Temp folder (canonical) and a mock app with it open as the workspace root.
    fn setup(name: &str) -> (PathBuf, tauri::App<tauri::test::MockRuntime>) {
        let base = std::env::temp_dir().join(format!("gestalt-envedit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![env_file_read])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(Workspace { root: Some(base.clone()).into() });
        app.manage(ShellEnv::default());
        (base, app)
    }

    fn arg(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn read_reports_what_parent_files_set() {
        let (root, app) = setup("read");
        let leaf = root.join("a/b/c");
        std::fs::create_dir_all(&leaf).unwrap();
        std::fs::create_dir_all(root.join("x/y")).unwrap();
        std::fs::write(root.join(FILE_NAME), "GESTALT_T_SHARED=root\nGESTALT_T_GONE=1\nunset GESTALT_T_BACK\nGESTALT_T_REGION=us\n").unwrap();
        std::fs::write(root.join("a").join(FILE_NAME), "unset GESTALT_T_GONE\nGESTALT_T_BACK=again\nGESTALT_T_NAME=${GESTALT_T_REGION}-a\n").unwrap();
        std::fs::write(root.join("a/b").join(FILE_NAME), [0xff, 0xfe]).unwrap(); // unreadable
        std::fs::write(leaf.join(FILE_NAME), "OWN=1\n").unwrap();
        std::fs::write(root.join("x").join(FILE_NAME), "not a pair\n").unwrap();

        let ctx = env_file_read(app.state(), app.state(), arg(&leaf)).await.unwrap();
        assert_eq!(ctx.rel_path, "a/b/c/.gestalt.env");
        assert!(ctx.exists);
        assert_eq!(ctx.content, "OWN=1\n");
        let inherited: Vec<(&str, &str, &str)> =
            ctx.inherited.iter().map(|i| (i.name.as_str(), i.value.as_str(), i.source.as_str())).collect();
        assert_eq!(
            inherited,
            [
                ("GESTALT_T_BACK", "again", "a/.gestalt.env"),
                ("GESTALT_T_NAME", "us-a", "a/.gestalt.env"),
                ("GESTALT_T_REGION", "us", ".gestalt.env"),
                ("GESTALT_T_SHARED", "root", ".gestalt.env"),
            ]
        );
        let unset: Vec<(&str, &str)> = ctx.inherited_unset.iter().map(|u| (u.name.as_str(), u.source.as_str())).collect();
        assert_eq!(unset, [("GESTALT_T_GONE", "a/.gestalt.env")]);
        assert_eq!(ctx.parent_errors.len(), 1);
        assert!(ctx.parent_errors[0].starts_with("a/b/.gestalt.env: "), "{:?}", ctx.parent_errors);

        // No file of its own; a parent with a syntax error.
        let ctx = env_file_read(app.state(), app.state(), arg(&root.join("x/y"))).await.unwrap();
        assert!(!ctx.exists && ctx.content.is_empty());
        assert_eq!(ctx.parent_errors, ["x/.gestalt.env: line 1: expected KEY=value"]);

        // Through IPC, as the UI calls it. The root's own file isn't a parent of itself.
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default()).build().unwrap();
        let ctx = ipc(&webview, "env_file_read", serde_json::json!({ "dir": root })).unwrap();
        assert_eq!(ctx["relPath"], ".gestalt.env");
        assert_eq!(ctx["exists"], true);
        assert_eq!(ctx["inherited"], serde_json::json!([]));
        assert_eq!(ctx["parentErrors"], serde_json::json!([]));

        // Unreadable file, and a folder outside the root.
        std::fs::create_dir_all(root.join("d").join(FILE_NAME)).unwrap();
        assert!(env_file_read(app.state(), app.state(), arg(&root.join("d"))).await.is_err());
        let err = ipc(&webview, "env_file_read", serde_json::json!({ "dir": root.parent().unwrap() })).unwrap_err();
        assert!(err.as_str().unwrap().contains("is not a folder inside"), "{err}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Call a command through the IPC layer, as the UI does.
    fn ipc(
        webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
        cmd: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, serde_json::Value> {
        let request = tauri::webview::InvokeRequest {
            cmd: cmd.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: tauri::ipc::InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.to_string(),
        };
        tauri::test::get_ipc_response(webview, request).map(|b| b.deserialize().unwrap())
    }

    #[test]
    fn validate_returns_the_first_error() {
        assert_eq!(env_file_validate("A=1\n# ok\n".into()), None);
        assert_eq!(env_file_validate("A=1\nB\n".into()).as_deref(), Some("line 2: expected KEY=value"));
    }

    #[test]
    fn save_creates_replaces_and_refuses_stale_or_invalid() {
        let (root, app) = setup("save");
        let path = root.join(FILE_NAME);
        let save = |content: &str, expected: Option<&str>| {
            env_file_save(app.state(), arg(&root), content.into(), expected.map(String::from))
        };

        save("A=1\n", None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A=1\n");
        save("A=2\n", Some("A=1\n")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A=2\n");
        // No temp file left behind.
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);

        assert!(save("A=3\n", Some("A=1\n")).unwrap_err().contains("changed on disk"));
        assert!(save("A=3\n", None).unwrap_err().contains("changed on disk"));
        assert_eq!(save("oops\n", Some("A=2\n")).unwrap_err(), "Not saved — line 1: expected KEY=value");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A=2\n");
        // Outside the root.
        assert!(env_file_save(app.state(), arg(root.parent().unwrap()), "A=1\n".into(), None).is_err());
        // Something unreadable in the way.
        std::fs::create_dir_all(root.join("d").join(FILE_NAME)).unwrap();
        let err = env_file_save(app.state(), arg(&root.join("d")), "A=1\n".into(), None).unwrap_err();
        assert!(err.starts_with(&*root.join("d").join(FILE_NAME).to_string_lossy()), "{err}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_through_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let (root, app) = setup("link");
        let target = root.join("shared.env");
        std::fs::write(&target, "A=1\n").unwrap();
        let link = root.join(FILE_NAME);
        std::os::unix::fs::symlink(&target, &link).unwrap();

        env_file_save(app.state(), arg(&root), "A=2\n".into(), Some("A=1\n".into())).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "A=2\n");

        // A read-only target: the write error is reported.
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert!(env_file_save(app.state(), arg(&root), "A=3\n".into(), Some("A=2\n".into())).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "A=2\n");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn save_and_delete_report_filesystem_errors() {
        use std::os::unix::fs::PermissionsExt;
        let (root, app) = setup("readonly");
        let dir = root.join("locked");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE_NAME), "A=1\n").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let saved = env_file_save(app.state(), arg(&dir), "A=2\n".into(), Some("A=1\n".into()));
        let deleted = env_file_delete(app.state(), arg(&dir), "A=1\n".into());
        let leftovers = std::fs::read_dir(&dir).unwrap().count();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(saved.is_err() && deleted.is_err());
        assert_eq!(leftovers, 1);
        assert_eq!(std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(), "A=1\n");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn delete_removes_only_the_expected_content() {
        let (root, app) = setup("delete");
        let path = root.join(FILE_NAME);
        std::fs::write(&path, "A=1\n").unwrap();

        assert!(env_file_delete(app.state(), arg(&root), "A=0\n".into()).unwrap_err().contains("changed on disk"));
        assert!(path.exists());
        env_file_delete(app.state(), arg(&root), "A=1\n".into()).unwrap();
        assert!(!path.exists());
        assert!(env_file_delete(app.state(), arg(root.parent().unwrap()), "A=1\n".into()).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
