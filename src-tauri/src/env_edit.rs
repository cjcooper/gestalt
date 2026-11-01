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
    std::fs::write(&tmp, content).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| {
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

        std::fs::remove_dir_all(&base).unwrap();
    }
}
