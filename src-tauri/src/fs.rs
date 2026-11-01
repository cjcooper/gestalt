//! Folder browsing for the tree view.

use crate::env_file;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    /// Folder has at least one visible entry (drives the expand arrow).
    pub has_children: bool,
    /// Folder contains `*.tf` / `*.tofu` files, i.e. tofu can run in it.
    pub is_module: bool,
    /// Folder has its own `.gestalt.env`.
    pub has_env_file: bool,
}

/// Entries never worth showing. Dotfiles are hidden except `.gestalt.env`,
/// which affects how tofu runs.
fn is_hidden(name: &str, is_dir: bool) -> bool {
    if is_dir {
        name.starts_with('.') || name == "node_modules"
    } else {
        name.starts_with('.') && name != env_file::FILE_NAME
    }
}

/// Visible entries of `dir` as (name, path, is_dir).
fn visible(dir: &Path) -> impl Iterator<Item = (String, std::path::PathBuf, bool)> {
    std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| {
        // Follow symlinks so linked folders/files show as what they point to.
        let is_dir = std::fs::metadata(e.path()).map(|m| m.is_dir()).ok()?;
        let name = e.file_name().to_string_lossy().into_owned();
        (!is_hidden(&name, is_dir)).then(|| (name, e.path(), is_dir))
    })
}

pub fn is_module(dir: &Path) -> bool {
    visible(dir).any(|(name, _, is_dir)| !is_dir && (name.ends_with(".tf") || name.ends_with(".tofu")))
}

pub fn entry(path: &Path) -> DirEntry {
    let is_dir = path.is_dir();
    DirEntry {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        path: path.to_string_lossy().into_owned(),
        is_dir,
        has_children: is_dir && visible(path).next().is_some(),
        is_module: is_dir && is_module(path),
        has_env_file: is_dir && path.join(env_file::FILE_NAME).is_file(),
    }
}

/// List the visible contents of `path`: folders first, then files, by name.
pub fn list_dir(path: &Path) -> Result<Vec<DirEntry>, String> {
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    let mut out: Vec<DirEntry> = visible(path).map(|(_, p, _)| entry(&p)).collect();
    out.sort_by_key(|e| (!e.is_dir, e.name.to_lowercase()));
    Ok(out)
}
