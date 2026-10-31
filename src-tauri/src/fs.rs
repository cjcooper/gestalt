//! Folder browsing for the tree view.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    /// Folder has at least one subfolder we'd show (drives the expand arrow).
    pub has_children: bool,
    /// Folder contains `*.tf` / `*.tofu` files, i.e. it's a root module tofu can run in.
    pub is_module: bool,
}

/// Directories never worth showing in the tree.
fn is_hidden(name: &str) -> bool {
    name.starts_with('.') || name == "node_modules"
}

fn subdirs(dir: &Path) -> impl Iterator<Item = (String, PathBuf)> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (!is_hidden(&name)).then(|| (name, e.path()))
        })
}

pub fn is_module(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            e.file_type().map(|t| t.is_file()).unwrap_or(false)
                && (name.ends_with(".tf") || name.ends_with(".tofu"))
        })
}

/// List the immediate subfolders of `path`, sorted by name.
pub fn list_dir(path: &Path) -> Result<Vec<DirEntry>, String> {
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    let mut out: Vec<DirEntry> = subdirs(path)
        .map(|(name, p)| DirEntry {
            has_children: subdirs(&p).next().is_some(),
            is_module: is_module(&p),
            path: p.to_string_lossy().into_owned(),
            name,
        })
        .collect();
    out.sort_by_key(|e| e.name.to_lowercase());
    Ok(out)
}
