//! Cleaning OpenTofu/Terraform caches out of a folder tree to free disk space,
//! like the infra repo's `cleanup-terraform-caches` script: every `.terraform`
//! directory (providers, downloaded modules) and `.terraform.lock.hcl` file at
//! or below a folder. `tofu init` (which plan runs automatically when
//! `.terraform` is missing) recreates them.

use crate::tofu::Runs;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::State;

const CACHE_DIR: &str = ".terraform";
const LOCK_FILE: &str = ".terraform.lock.hcl";

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanSummary {
    /// Everything found (scan) or removed (clean), in walk order.
    pub paths: Vec<String>,
    pub dirs: usize,
    pub files: usize,
    pub bytes: u64,
    /// Paths that couldn't be removed, with the reason.
    pub errors: Vec<String>,
}

/// Total size of `path`, not following symlinks.
fn size_of(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).into_iter().flatten().flatten().map(|e| size_of(&e.path())).sum()
}

/// Cache dirs and lock files at or below `dir`. Symlinked folders aren't
/// followed (they may point outside `dir`), and `.git` is skipped.
fn find(dir: &Path, out: &mut Vec<(PathBuf, bool)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Ok(ft) = e.file_type() else { continue };
        let name = e.file_name();
        if ft.is_dir() {
            if name == CACHE_DIR {
                out.push((e.path(), true)); // removed whole; don't look inside
            } else if name != ".git" {
                find(&e.path(), out);
            }
        } else if ft.is_file() && name == LOCK_FILE {
            out.push((e.path(), false));
        }
    }
}

fn scan(dir: &Path) -> Vec<(PathBuf, bool, u64)> {
    let mut found = Vec::new();
    find(dir, &mut found);
    found
        .into_iter()
        .map(|(p, is_dir)| {
            let size = size_of(&p);
            (p, is_dir, size)
        })
        .collect()
}

fn summarize(items: impl IntoIterator<Item = (PathBuf, bool, u64)>) -> CleanSummary {
    let mut s = CleanSummary::default();
    for (p, is_dir, size) in items {
        s.paths.push(p.to_string_lossy().into_owned());
        if is_dir {
            s.dirs += 1
        } else {
            s.files += 1
        }
        s.bytes += size;
    }
    s
}

fn clean(dir: &Path) -> CleanSummary {
    let mut errors = Vec::new();
    let removed: Vec<_> = scan(dir)
        .into_iter()
        .filter(|(p, is_dir, _)| {
            let r = if *is_dir { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
            r.map_err(|e| errors.push(format!("{}: {e}", p.display()))).is_ok()
        })
        .collect();
    CleanSummary { errors, ..summarize(removed) }
}

fn require_dir(dir: &str) -> Result<PathBuf, String> {
    let dir = PathBuf::from(dir);
    if dir.is_dir() { Ok(dir) } else { Err(format!("{} is not a directory", dir.display())) }
}

/// Disk work off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

/// Refuse if tofu is running (in one of `active`) at or below `dir`.
fn ensure_idle(active: Vec<PathBuf>, dir: &Path) -> Result<(), String> {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let target = canon(dir);
    if let Some(busy) = active.into_iter().find(|d| canon(d).starts_with(&target)) {
        return Err(format!("tofu is running in {}; wait for it to finish (or cancel it) first.", busy.display()));
    }
    Ok(())
}

/// What cleaning `dir` would remove, for the confirmation prompt.
#[tauri::command]
pub async fn clean_scan(dir: String) -> Result<CleanSummary, String> {
    let dir = require_dir(&dir)?;
    blocking(move || summarize(scan(&dir))).await
}

/// Remove every `.terraform` dir and `.terraform.lock.hcl` at or below `dir`.
/// Refused while tofu is running anywhere in that tree: it'd pull the
/// providers out from under it.
#[tauri::command]
pub async fn clean_caches(runs: State<'_, Runs>, dir: String) -> Result<CleanSummary, String> {
    let dir = require_dir(&dir)?;
    ensure_idle(runs.active_dirs(), &dir)?;
    blocking(move || clean(&dir)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn cleans_only_at_and_below_the_folder() {
        let base = std::env::temp_dir().join(format!("gestalt-clean-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let mk = |rel: &str, bytes: usize| {
            let p = base.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, vec![b'x'; bytes]).unwrap();
        };
        mk("main.tf", 1);
        mk(".terraform.lock.hcl", 1);
        mk(".terraform/providers/p", 1);
        mk("dev/app/main.tf", 1);
        mk("dev/app/.terraform.lock.hcl", 10);
        mk("dev/app/.terraform/providers/aws", 100);
        mk("dev/app/.terraform/modules/m/.terraform/x", 5);
        mk("dev/net/.terraform/terraform.tfstate", 7);
        mk("dev/app/terraform.lock.hcl", 1); // not the lock file name
        mk(".git/.terraform/x", 1);

        let dev = base.join("dev");
        let preview = summarize(scan(&dev));
        assert_eq!((preview.dirs, preview.files, preview.bytes), (2, 1, 122));
        assert!(dev.join("app/.terraform").exists(), "scan removes nothing");

        let done = clean(&dev);
        assert!(done.errors.is_empty());
        assert_eq!((done.dirs, done.files, done.bytes), (2, 1, 122));
        assert!(!dev.join("app/.terraform").exists());
        assert!(!dev.join("app/.terraform.lock.hcl").exists());
        assert!(!dev.join("net/.terraform").exists());
        assert!(dev.join("app/main.tf").exists());
        assert!(dev.join("app/terraform.lock.hcl").exists());
        // Above the folder: untouched.
        assert!(base.join(".terraform").exists());
        assert!(base.join(".terraform.lock.hcl").exists());

        // From the top, .git is left alone.
        let all = clean(&base);
        assert_eq!((all.dirs, all.files), (1, 1));
        assert!(base.join(".git/.terraform/x").exists());
        fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symlinked_folders() {
        let base = std::env::temp_dir().join(format!("gestalt-clean-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let outside = base.join("outside");
        fs::create_dir_all(outside.join(".terraform")).unwrap();
        fs::create_dir_all(base.join("tree")).unwrap();
        std::os::unix::fs::symlink(&outside, base.join("tree/link")).unwrap();

        let done = clean(&base.join("tree"));
        assert_eq!(done.dirs, 0);
        assert!(outside.join(".terraform").exists());
        fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reports_what_could_not_be_removed() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("gestalt-clean-perm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let locked = base.join("locked");
        fs::create_dir_all(locked.join(".terraform")).unwrap();
        fs::write(locked.join(".terraform.lock.hcl"), "x").unwrap();
        fs::create_dir_all(base.join("open/.terraform")).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();

        let done = clean(&base);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        // Only what was actually removed is counted; the rest are errors.
        assert_eq!((done.dirs, done.files), (1, 0));
        assert_eq!(done.paths, [base.join("open/.terraform").to_string_lossy()]);
        assert_eq!(done.errors.len(), 2, "{:?}", done.errors);
        assert!(done.errors.iter().all(|e| e.starts_with(&*locked.to_string_lossy())));
        assert!(locked.join(".terraform.lock.hcl").exists());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn size_of_missing_path_is_zero() {
        assert_eq!(size_of(Path::new("/nonexistent/gestalt-clean")), 0);
    }

    #[test]
    fn refuses_while_tofu_runs_in_the_tree() {
        let base = std::env::temp_dir().join(format!("gestalt-clean-busy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("dev/app")).unwrap();
        fs::create_dir_all(base.join("prd")).unwrap();
        let active = || vec![base.join("dev/app")];

        let err = ensure_idle(active(), &base.join("dev")).unwrap_err();
        assert!(err.contains("tofu is running in") && err.contains("dev/app"), "{err}");
        assert!(ensure_idle(active(), &base).is_err());
        assert!(ensure_idle(active(), &base.join("prd")).is_ok());
        // Paths that can't be canonicalized are compared as given.
        let gone = Path::new("/nonexistent/gestalt-clean");
        assert!(ensure_idle(vec![gone.join("app")], gone).is_err());
        assert!(ensure_idle(Vec::new(), &base).is_ok());
        fs::remove_dir_all(&base).unwrap();
    }

    #[tokio::test]
    async fn blocking_work_that_panics_is_an_error() {
        assert_eq!(blocking(|| 7).await, Ok(7));
        assert!(blocking(|| -> u8 { panic!("boom") }).await.is_err());
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
    fn scan_and_clean_commands() {
        use serde_json::json;
        use tauri::Manager;
        let base = std::env::temp_dir().join(format!("gestalt-clean-cmd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("app/.terraform/providers")).unwrap();
        fs::write(base.join("app/.terraform/providers/p"), "1234").unwrap();
        fs::write(base.join("app/.terraform.lock.hcl"), "12").unwrap();
        fs::write(base.join("app/main.tf"), "").unwrap();

        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![clean_scan, clean_caches])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(Runs::default());
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default()).build().unwrap();

        let missing = json!({ "dir": base.join("missing") });
        let err = ipc(&webview, "clean_scan", missing.clone()).unwrap_err();
        assert!(err.as_str().unwrap().contains("is not a directory"), "{err}");
        assert!(ipc(&webview, "clean_caches", missing).is_err());

        let dir = json!({ "dir": base });
        let preview = ipc(&webview, "clean_scan", dir.clone()).unwrap();
        assert_eq!((&preview["dirs"], &preview["files"], &preview["bytes"]), (&json!(1), &json!(1), &json!(6)));
        assert!(base.join("app/.terraform").exists());

        let done = ipc(&webview, "clean_caches", dir).unwrap();
        assert_eq!(done["paths"].as_array().unwrap().len(), 2);
        assert_eq!(done["errors"], json!([]));
        assert!(!base.join("app/.terraform").exists() && !base.join("app/.terraform.lock.hcl").exists());
        assert!(base.join("app/main.tf").exists());
        fs::remove_dir_all(&base).unwrap();
    }
}
