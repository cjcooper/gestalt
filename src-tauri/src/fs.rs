//! Folder browsing for the tree view.

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
    /// Folder contains a `main.tf`: a root module Gestalt runs tofu in
    /// (same rule as the Commits tab).
    pub is_module: bool,
}

/// Entries never worth showing: dotfiles, and dependency folders.
fn is_hidden(name: &str, is_dir: bool) -> bool {
    name.starts_with('.') || (is_dir && name == "node_modules")
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
    dir.join("main.tf").is_file()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn lists_visible_entries_folders_first() {
        let base = std::env::temp_dir().join(format!("gestalt-fs-list-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let mk = |rel: &str| {
            let p = base.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        };
        mk("Zeta/main.tf");
        mk("alpha/x.tf");
        mk("alpha/.hidden");
        mk("empty/.DS_Store");
        mk("b.tf");
        mk("A.md");
        mk(".gestalt.env");
        mk(".secret");
        mk(".git/HEAD");
        mk("node_modules/x/index.js");

        let entries = list_dir(&base).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "empty", "Zeta", "A.md", "b.tf"]);

        let get = |n: &str| entries.iter().find(|e| e.name == n).unwrap();
        let zeta = get("Zeta");
        assert!(zeta.is_dir && zeta.is_module && zeta.has_children);
        assert_eq!(zeta.path, base.join("Zeta").to_string_lossy());
        let alpha = get("alpha");
        assert!(alpha.is_dir && !alpha.is_module && alpha.has_children);
        // Only hidden entries inside: no expand arrow.
        assert!(!get("empty").has_children);
        let file = get("b.tf");
        assert!(!file.is_dir && !file.has_children && !file.is_module);

        fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn follows_symlinks_and_skips_broken_ones() {
        let base = std::env::temp_dir().join(format!("gestalt-fs-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("real")).unwrap();
        fs::write(base.join("real/main.tf"), "").unwrap();
        fs::create_dir_all(base.join("tree")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("tree/linked")).unwrap();
        std::os::unix::fs::symlink(base.join("nowhere"), base.join("tree/broken")).unwrap();

        let entries = list_dir(&base.join("tree")).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].name == "linked" && entries[0].is_dir && entries[0].is_module);
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn rejects_non_directories() {
        let base = std::env::temp_dir().join(format!("gestalt-fs-notdir-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("f"), "").unwrap();
        assert!(list_dir(&base.join("f")).unwrap_err().contains("is not a directory"));
        assert!(list_dir(&base.join("missing")).is_err());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn entry_for_a_path_without_a_file_name() {
        // `/` has no file name: the display path is used instead.
        let e = entry(Path::new("/"));
        assert_eq!(e.name, "/");
        assert!(e.is_dir && e.has_children);
    }
}
