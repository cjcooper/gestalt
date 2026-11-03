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

/// What cleaning `dir` would remove, for the confirmation prompt.
#[tauri::command]
pub async fn clean_scan(dir: String) -> Result<CleanSummary, String> {
    let dir = require_dir(&dir)?;
    tauri::async_runtime::spawn_blocking(move || summarize(scan(&dir))).await.map_err(|e| e.to_string())
}

/// Remove every `.terraform` dir and `.terraform.lock.hcl` at or below `dir`.
/// Refused while tofu is running anywhere in that tree: it'd pull the
/// providers out from under it.
#[tauri::command]
pub async fn clean_caches(runs: State<'_, Runs>, dir: String) -> Result<CleanSummary, String> {
    let dir = require_dir(&dir)?;
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let target = canon(&dir);
    if let Some(busy) = runs.active_dirs().into_iter().find(|d| canon(d).starts_with(&target)) {
        return Err(format!("tofu is running in {}; wait for it to finish (or cancel it) first.", busy.display()));
    }
    tauri::async_runtime::spawn_blocking(move || clean(&dir)).await.map_err(|e| e.to_string())
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
}
