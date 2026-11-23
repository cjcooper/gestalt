//! Finding things in the opened folder, for the Files tab: every visible
//! path (Go to File, which matches and completes them in the UI) and a plain
//! text search through the files (Find in Files).
//!
//! Both see what the tree shows: dotfiles and `node_modules` are skipped
//! (so `.terraform` and `.git` are too), symlinks are followed, and a linked
//! folder that leads back into one already walked is listed only once.

use crate::clean::blocking;
use crate::fs;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// Go to File stops listing here, so a huge tree can't stall the UI.
pub const MAX_FILES: usize = 50_000;
/// Find in Files stops after this many matching lines.
pub const MAX_MATCHES: usize = 1_000;
/// Files larger than this aren't searched: generated or binary data.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Longer lines are cut down to this many characters around the match.
pub const MAX_LINE_CHARS: usize = 400;

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileItem {
    /// Relative to the opened folder, `/`-separated.
    pub rel: String,
    pub is_dir: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileList {
    pub items: Vec<FileItem>,
    /// Stopped at [`MAX_FILES`].
    pub truncated: bool,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchMatch {
    pub rel: String,
    pub path: String,
    /// 1-based.
    pub line: usize,
    /// The line (or the part of a long one around the match).
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub matches: Vec<SearchMatch>,
    pub files_searched: usize,
    /// Stopped at [`MAX_MATCHES`].
    pub truncated: bool,
}

/// `path` relative to `root`, `/`-separated.
fn rel(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let names = rel.components().filter_map(|c| match c {
        Component::Normal(name) => Some(name.to_string_lossy()),
        _ => None,
    });
    names.collect::<Vec<_>>().join("/")
}

/// Visit every visible entry below `root`, a folder's contents in name order
/// before going into its subfolders, until `visit` returns false.
fn walk(root: &Path, mut visit: impl FnMut(&Path, bool) -> bool) {
    let mut seen = HashSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        // A link back to a folder already walked (or a cycle) is skipped.
        if !std::fs::canonicalize(&dir).is_ok_and(|c| seen.insert(c)) {
            continue;
        }
        let mut entries: Vec<(String, PathBuf, bool)> = fs::visible(&dir).collect();
        entries.sort_by_key(|(name, _, is_dir)| (!is_dir, name.to_lowercase()));
        let mut subdirs = Vec::new();
        for (_, path, is_dir) in entries {
            if !visit(&path, is_dir) {
                return;
            }
            if is_dir {
                subdirs.push(path);
            }
        }
        stack.extend(subdirs.into_iter().rev());
    }
}

/// Every visible file and folder below `root`, up to `limit`.
pub fn list_files(root: &Path, limit: usize) -> FileList {
    let mut items = Vec::new();
    let mut truncated = false;
    walk(root, |path, is_dir| {
        if items.len() == limit {
            truncated = true;
            return false;
        }
        items.push(FileItem { rel: rel(root, path), is_dir });
        true
    });
    FileList { items, truncated }
}

/// The file's text, unless it's too big or looks binary.
fn read_text(path: &Path) -> Option<String> {
    if std::fs::metadata(path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let head = &bytes[..bytes.len().min(8192)];
    (!head.contains(&0)).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// `line`, or the part of it around char index `at` if it's long.
fn excerpt(line: &str, at: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let start = at.saturating_sub(MAX_LINE_CHARS / 4).min(chars.len() - MAX_LINE_CHARS);
    chars[start..start + MAX_LINE_CHARS].iter().collect()
}

/// Lines containing `query` in the files below `root`, up to `limit`.
pub fn search_files(root: &Path, query: &str, case_sensitive: bool, limit: usize) -> SearchResults {
    let mut out = SearchResults { matches: Vec::new(), files_searched: 0, truncated: false };
    if query.is_empty() {
        return out;
    }
    let fold = |s: &str| if case_sensitive { s.to_string() } else { s.to_lowercase() };
    let needle = fold(query);
    walk(root, |path, is_dir| {
        let Some(text) = (!is_dir).then(|| read_text(path)).flatten() else {
            return true;
        };
        out.files_searched += 1;
        for (i, line) in text.lines().enumerate() {
            let Some(at) = fold(line).find(&needle) else { continue };
            if out.matches.len() == limit {
                out.truncated = true;
                return false;
            }
            // `at` is a byte index in the folded line; close enough to place the excerpt.
            let at = line.char_indices().take_while(|(b, _)| *b < at).count();
            out.matches.push(SearchMatch {
                rel: rel(root, path),
                path: path.to_string_lossy().into_owned(),
                line: i + 1,
                text: excerpt(line, at),
            });
        }
        true
    });
    out
}

fn require_dir(root: &str) -> Result<PathBuf, String> {
    let root = PathBuf::from(root);
    if root.is_dir() { Ok(root) } else { Err(format!("{} is not a directory", root.display())) }
}

/// Every visible path in the opened folder, for Go to File.
#[tauri::command]
pub async fn find_files(root: String) -> Result<FileList, String> {
    let root = require_dir(&root)?;
    blocking(move || list_files(&root, MAX_FILES)).await
}

/// Lines containing `query` in the opened folder's files (Find in Files).
#[tauri::command]
pub async fn find_in_files(root: String, query: String, case_sensitive: bool) -> Result<SearchResults, String> {
    let root = require_dir(&root)?;
    blocking(move || search_files(&root, &query, case_sensitive, MAX_MATCHES)).await
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::shell_env::testing::{ipc, temp_dir, webview};
    use std::fs as stdfs;
    use tauri::async_runtime::block_on;

    fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = temp_dir(name);
        for (rel, content) in files {
            let p = root.join(rel);
            stdfs::create_dir_all(p.parent().unwrap()).unwrap();
            stdfs::write(p, content).unwrap();
        }
        root
    }

    fn rels(list: &FileList) -> Vec<String> {
        list.items.iter().map(|i| format!("{}{}", i.rel, if i.is_dir { "/" } else { "" })).collect()
    }

    #[test]
    fn lists_visible_paths_folders_first_then_their_contents() {
        let root = tree(
            "search-list",
            &[
                ("envs/prod/main.tf", ""),
                ("envs/dev/main.tf", ""),
                ("README.md", ""),
                ("b.tf", ""),
                (".git/HEAD", ""),
                ("envs/prod/.terraform/x", ""),
                ("node_modules/x/index.js", ""),
            ],
        );
        let list = list_files(&root, MAX_FILES);
        assert!(!list.truncated);
        assert_eq!(rels(&list), ["envs/", "b.tf", "README.md", "envs/dev/", "envs/prod/", "envs/dev/main.tf", "envs/prod/main.tf"]);

        let first = list_files(&root, 2);
        assert!(first.truncated);
        assert_eq!(rels(&first), ["envs/", "b.tf"]);
        // Exactly at the limit isn't truncated.
        assert!(!list_files(&root, 7).truncated);
    }

    #[test]
    fn follows_links_but_walks_each_folder_once() {
        let root = tree("search-links", &[("real/main.tf", ""), ("shared.tf", "")]);
        std::os::unix::fs::symlink(root.join("real"), root.join("again")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("real/up")).unwrap();
        std::os::unix::fs::symlink(root.join("shared.tf"), root.join("real/linked.tf")).unwrap();
        let list = list_files(&root, MAX_FILES);
        // `again` is listed but, being `real` again, not walked; `real/up` leads back to the root.
        assert_eq!(rels(&list), ["again/", "real/", "shared.tf", "again/up/", "again/linked.tf", "again/main.tf"]);
    }

    #[test]
    fn rel_paths_are_slash_separated_and_fall_back_to_the_path() {
        assert_eq!(rel(Path::new("/r"), Path::new("/r/a/b.tf")), "a/b.tf");
        assert_eq!(rel(Path::new("/r"), Path::new("/elsewhere/c")), "elsewhere/c");
    }

    #[test]
    fn searches_text_files_for_lines() {
        let root = tree(
            "search-text",
            &[
                ("a/main.tf", "resource \"aws_s3_bucket\" \"logs\" {\n  bucket = \"Logs\"\n}\n"),
                ("b.tf", "# no match\nbucket\n"),
                ("bin.dat", "bucket\0binary"),
                (".hidden", "bucket"),
            ],
        );
        let found = search_files(&root, "bucket", true, MAX_MATCHES);
        let lines: Vec<_> = found.matches.iter().map(|m| (m.rel.as_str(), m.line, m.text.as_str())).collect();
        assert_eq!(lines, [("b.tf", 2, "bucket"), ("a/main.tf", 1, "resource \"aws_s3_bucket\" \"logs\" {"), ("a/main.tf", 2, "  bucket = \"Logs\"")]);
        assert_eq!(found.files_searched, 2);
        assert_eq!(found.matches[0].path, root.join("b.tf").to_string_lossy());
        assert!(!found.truncated);

        // Case: exact, or folded.
        assert_eq!(search_files(&root, "Logs", true, MAX_MATCHES).matches.len(), 1);
        assert_eq!(search_files(&root, "LOGS", false, MAX_MATCHES).matches.len(), 2);
        assert!(search_files(&root, "", false, MAX_MATCHES).matches.is_empty());

        let capped = search_files(&root, "bucket", true, 2);
        assert!(capped.truncated);
        assert_eq!(capped.matches.len(), 2);
    }

    #[test]
    fn skips_big_files_and_trims_long_lines() {
        let big = "x".repeat(MAX_FILE_BYTES as usize + 1);
        let long = format!("{}needle{}", "a".repeat(1000), "b".repeat(1000));
        let root = tree("search-big", &[("big.txt", &big), ("long.txt", &long), ("short.txt", "é needle")]);
        assert_eq!(read_text(&root.join("big.txt")), None);
        assert_eq!(read_text(&root.join("missing")), None);
        let found = search_files(&root, "needle", false, MAX_MATCHES);
        let text = &found.matches[0].text;
        assert_eq!(text.chars().count(), MAX_LINE_CHARS);
        assert!(text.starts_with(&"a".repeat(MAX_LINE_CHARS / 4)) && text[MAX_LINE_CHARS / 4..].starts_with("needle"));
        assert_eq!(found.matches[1].text, "é needle");
        // A match near the end keeps a full-length excerpt.
        let end = format!("{}needle", "a".repeat(1000));
        assert_eq!(excerpt(&end, 1000), format!("{}needle", "a".repeat(MAX_LINE_CHARS - 6)));
    }

    #[test]
    fn commands_check_the_folder_and_work_over_ipc() {
        let root = tree("search-cmd", &[("main.tf", "bucket\n")]);
        let s = |p: &Path| p.to_string_lossy().into_owned();
        assert_eq!(block_on(find_files(s(&root))).unwrap().items.len(), 1);
        assert_eq!(block_on(find_in_files(s(&root), "bucket".into(), false)).unwrap().matches.len(), 1);
        let missing = root.join("missing");
        let err = format!("{} is not a directory", missing.display());
        assert_eq!(block_on(find_files(s(&missing))).unwrap_err(), err);
        assert_eq!(block_on(find_in_files(s(&missing), "x".into(), true)).unwrap_err(), err);

        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![find_files, find_in_files])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let view = webview(&app);
        let listed = ipc(&view, "find_files", serde_json::json!({ "root": root })).unwrap();
        assert_eq!(listed["items"][0], serde_json::json!({ "rel": "main.tf", "isDir": false }));
        let found = ipc(&view, "find_in_files", serde_json::json!({ "root": root, "query": "BUCKET", "caseSensitive": false })).unwrap();
        assert_eq!(found["matches"][0]["line"], 1);
        assert_eq!(found["filesSearched"], 1);
    }
}
