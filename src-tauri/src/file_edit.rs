//! Reading and writing any text file in the tree, for the code editor.
//!
//! Only files that appear inside the opened workspace root can be read or
//! written, and a save is refused if the file changed on disk since the editor
//! loaded it (unless the user chose to overwrite).
//!
//! A file may be a symlink (or sit in a symlinked folder), e.g. a shared
//! `providers.tf` linked into many modules. It's edited where it really is,
//! but belongs to the folder it appears in: that's where Plan runs.

use crate::tofu::Workspace;
use serde::Serialize;
use std::path::{Component, Path, PathBuf};
use tauri::State;

/// Files bigger than this aren't opened: the editor is for config and code.
const MAX_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    /// Path relative to the root, as it appears in the tree, e.g. `envs/prod/main.tf`.
    rel_path: String,
    content: String,
    /// The folder it appears in has a `main.tf`: the editor offers Plan.
    is_module: bool,
    /// Set when the file is reached through a symlink: the real file, relative
    /// to the root if it's inside it. Edits change it for every link.
    link_target: Option<String>,
}

/// A file as the tree shows it, and the real file behind it.
#[derive(Debug, PartialEq)]
pub(crate) struct Located {
    /// Where it appears (`.`/`..` removed, links kept), inside the root.
    pub shown: PathBuf,
    /// The real file, all links resolved.
    pub target: PathBuf,
    /// `shown` relative to `base` (the root, as given or canonical).
    pub rel: PathBuf,
    base: PathBuf,
    /// The root, canonical.
    root: PathBuf,
}

impl Located {
    /// Is any part of the path below the root a symlink?
    pub fn is_linked(&self) -> bool {
        let mut p = self.base.clone();
        self.rel.components().any(|c| {
            p.push(c);
            std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink())
        })
    }

    /// The real file, relative to the root when it's inside it.
    fn target_display(&self) -> String {
        self.target.strip_prefix(&self.root).unwrap_or(&self.target).display().to_string()
    }
}

/// `p` with `.` and `..` resolved by name, without following links.
pub(crate) fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// Find `path`: a file that appears inside the workspace root. The file (or
/// a folder on the way) may link elsewhere, even outside the root.
pub(crate) fn locate(workspace: &Workspace, path: &str) -> Result<Located, String> {
    let given_root = workspace.root.lock().unwrap().clone().ok_or("No folder is open.")?;
    let root = given_root.canonicalize().map_err(|e| format!("{}: {e}", given_root.display()))?;
    let shown = normalize(Path::new(path));
    let target = shown.canonicalize().map_err(|e| format!("{path}: {e}"))?;
    // Inside the root as spelled (the tree's own paths), or once its folder is
    // resolved (the same folder spelled another way, e.g. /tmp vs /private/tmp).
    let given_root = normalize(&given_root);
    let within = |base: &Path, p: &Path| p.strip_prefix(base).ok().map(|rel| (base.to_path_buf(), rel.to_path_buf()));
    let inside = within(&given_root, &shown).or_else(|| {
        let folder = shown.parent()?.canonicalize().ok()?;
        within(&root, &folder.join(shown.file_name()?))
    });
    match inside {
        Some((base, rel)) if target.is_file() => Ok(Located { shown, target, rel, base, root }),
        _ => Err(format!("{} is not a file inside {}", shown.display(), root.display())),
    }
}

/// The file as text, refusing anything too big or not text.
fn read_text(path: &Path) -> Result<String, String> {
    let len = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    if len > MAX_BYTES {
        return Err(format!("{} is too large to edit ({:.1} MB).", path.display(), len as f64 / (1024.0 * 1024.0)));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    match String::from_utf8(bytes) {
        Ok(s) if !s.contains('\0') => Ok(s),
        _ => Err(format!("{} isn't a text file.", path.display())),
    }
}

/// Error prefix the UI looks for to offer overwriting.
const CHANGED_ON_DISK: &str = "changed on disk";

#[tauri::command]
pub fn file_read(workspace: State<'_, Workspace>, path: String) -> Result<FileContent, String> {
    let file = locate(&workspace, &path)?;
    Ok(FileContent {
        content: read_text(&file.target)?,
        rel_path: file.rel.display().to_string(),
        is_module: file.shown.parent().is_some_and(crate::fs::is_module),
        link_target: file.is_linked().then(|| file.target_display()),
    })
}

/// Write `content` to `path`. `expected` is what the editor loaded; the write
/// is refused if the file now holds something else. `None` overwrites.
#[tauri::command]
pub fn file_save(workspace: State<'_, Workspace>, path: String, content: String, expected: Option<String>) -> Result<(), String> {
    // Through any links: the real file changes, for every place linking to it.
    let file = locate(&workspace, &path)?.target;
    if let Some(expected) = expected
        && std::fs::read_to_string(&file).ok() != Some(expected)
    {
        return Err(format!("{} was {CHANGED_ON_DISK} since you opened it.", file.display()));
    }
    write_atomic(&file, &content)
}

/// Write-then-rename so a crash never leaves a half-written file, keeping the
/// file's permissions (e.g. an executable script stays executable).
fn write_atomic(file: &Path, content: &str) -> Result<(), String> {
    let dir = file.parent().ok_or_else(|| format!("{} has no parent folder", file.display()))?;
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.gestalt-{}.tmp", std::process::id()));
    let result = std::fs::metadata(file)
        .and_then(|meta| {
            std::fs::write(&tmp, content)?;
            std::fs::set_permissions(&tmp, meta.permissions())?;
            std::fs::rename(&tmp, file)
        })
        .map_err(|e| format!("{}: {e}", file.display()));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::shell_env::testing::{ipc, temp_dir, webview};
    use std::os::unix::fs::PermissionsExt;
    use tauri::Manager;

    fn setup(name: &str) -> (PathBuf, tauri::App<tauri::test::MockRuntime>) {
        let root = temp_dir(&format!("fileedit-{name}"));
        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![file_read, file_save])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(Workspace { root: Some(root.clone()).into() });
        (root, app)
    }

    fn arg(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn locate_confines_to_files_in_the_root() {
        let base = temp_dir("fileedit-locate");
        let root = base.join("root");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/main.tf"), "").unwrap();
        std::fs::write(base.join("outside.tf"), "").unwrap();
        let ws = Workspace::default();
        let file = root.join("a/main.tf");
        assert_eq!(locate(&ws, &arg(&file)).unwrap_err(), "No folder is open.");

        *ws.root.lock().unwrap() = Some(root.clone());
        let found = locate(&ws, &arg(&file)).unwrap();
        assert_eq!((&found.shown, &found.target, &found.rel), (&file, &file, &PathBuf::from("a/main.tf")));
        assert!(!found.is_linked());
        // `.` and `..` are resolved by name.
        assert_eq!(locate(&ws, &arg(&root.join("./a/../a/main.tf"))).unwrap(), found);
        assert!(locate(&ws, &arg(&root.join("a"))).unwrap_err().contains("is not a file inside"));
        assert!(locate(&ws, &arg(&root.join("a/../../outside.tf"))).unwrap_err().contains("is not a file inside"));
        assert!(locate(&ws, &arg(&base.join("outside.tf"))).unwrap_err().contains("is not a file inside"));
        let missing = root.join("missing.tf");
        assert!(locate(&ws, &arg(&missing)).unwrap_err().starts_with(&*missing.to_string_lossy()));

        *ws.root.lock().unwrap() = Some(base.join("gone"));
        assert!(locate(&ws, &arg(&file)).unwrap_err().starts_with(&*base.join("gone").to_string_lossy()));
    }

    #[test]
    fn normalize_resolves_dots_by_name() {
        assert_eq!(normalize(Path::new("./a/../b/./c")), PathBuf::from("b/c"));
        assert_eq!(normalize(Path::new("/r/link/../x")), PathBuf::from("/r/x"));
    }

    #[test]
    fn locate_follows_links_but_keeps_where_the_file_appears() {
        let base = temp_dir("fileedit-links");
        let root = base.join("root");
        std::fs::create_dir_all(root.join("mod")).unwrap();
        std::fs::create_dir_all(root.join("shared")).unwrap();
        std::fs::write(root.join("shared/providers.tf"), "").unwrap();
        std::fs::write(base.join("outside.tf"), "").unwrap();
        std::fs::create_dir_all(base.join("lib")).unwrap();
        std::fs::write(base.join("lib/x.tf"), "").unwrap();
        let link = |to: &Path, at: &str| std::os::unix::fs::symlink(to, root.join(at)).unwrap();
        link(Path::new("../shared/providers.tf"), "mod/providers.tf");
        link(&base.join("outside.tf"), "mod/outside.tf");
        link(&base.join("lib"), "mod/lib");
        let ws = Workspace { root: Some(root.clone()).into() };

        // A file linked from elsewhere in the root.
        let f = locate(&ws, &arg(&root.join("mod/providers.tf"))).unwrap();
        assert_eq!(f.shown, root.join("mod/providers.tf"));
        assert_eq!(f.target, root.join("shared/providers.tf"));
        assert_eq!(f.rel, PathBuf::from("mod/providers.tf"));
        assert!(f.is_linked());
        assert_eq!(f.target_display(), "shared/providers.tf");

        // Linked from outside the root: allowed, shown with its full path.
        let f = locate(&ws, &arg(&root.join("mod/outside.tf"))).unwrap();
        assert_eq!(f.target_display(), base.join("outside.tf").to_string_lossy());
        // A plain file in a linked folder.
        let f = locate(&ws, &arg(&root.join("mod/lib/x.tf"))).unwrap();
        assert!(f.is_linked());
        assert_eq!(f.rel, PathBuf::from("mod/lib/x.tf"));

        // The root itself reached through a link isn't "linked".
        std::os::unix::fs::symlink(&root, base.join("alias")).unwrap();
        *ws.root.lock().unwrap() = Some(base.join("alias"));
        let f = locate(&ws, &arg(&base.join("alias/shared/providers.tf"))).unwrap();
        assert!(!f.is_linked());
        // The same folder spelled another way still counts as inside.
        let f = locate(&ws, &arg(&root.join("shared/providers.tf"))).unwrap();
        assert_eq!(f.rel, PathBuf::from("shared/providers.tf"));
        assert!(!f.is_linked());
        // A path without a folder or name.
        assert!(locate(&ws, "/").is_err());
    }

    #[test]
    fn read_returns_text_and_refuses_binary_or_huge_files() {
        let (root, app) = setup("read");
        std::fs::create_dir_all(root.join("mod")).unwrap();
        std::fs::write(root.join("mod/main.tf"), "resource \"x\" \"y\" {}\n").unwrap();
        let read = |p: &Path| file_read(app.state(), arg(p));

        let got = read(&root.join("mod/main.tf")).unwrap();
        assert_eq!((got.rel_path.as_str(), got.content.as_str()), ("mod/main.tf", "resource \"x\" \"y\" {}\n"));
        assert!(got.is_module);

        std::fs::write(root.join("bad.bin"), [0xff, 0xfe, 0x00]).unwrap();
        assert!(read(&root.join("bad.bin")).unwrap_err().ends_with("isn't a text file."));
        std::fs::write(root.join("nul.bin"), "a\0b").unwrap();
        assert!(read(&root.join("nul.bin")).unwrap_err().ends_with("isn't a text file."));

        let big = std::fs::File::create(root.join("big.txt")).unwrap();
        big.set_len(MAX_BYTES + 1).unwrap();
        assert!(read(&root.join("big.txt")).unwrap_err().ends_with("is too large to edit (5.0 MB)."));

        // Unreadable.
        let locked = root.join("locked.tf");
        std::fs::write(&locked, "x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let err = read(&locked);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(err.unwrap_err().starts_with(&*locked.to_string_lossy()));

        assert!(read(&root.join("missing")).is_err());

        // Through IPC, as the UI calls it.
        let view = webview(&app);
        let got = ipc(&view, "file_read", serde_json::json!({ "path": root.join("mod/main.tf") })).unwrap();
        assert_eq!(got["relPath"], "mod/main.tf");
        assert_eq!(got["isModule"], true);
        std::fs::write(root.join("README.md"), "hi").unwrap();
        assert!(!read(&root.join("README.md")).unwrap().is_module);
        assert!(ipc(&view, "file_read", serde_json::json!({})).is_err());
    }

    #[test]
    fn read_text_reports_a_missing_file() {
        let missing = temp_dir("fileedit-readtext").join("gone");
        assert!(read_text(&missing).unwrap_err().starts_with(&*missing.to_string_lossy()));
    }

    #[test]
    fn save_replaces_keeps_permissions_and_refuses_stale_writes() {
        let (root, app) = setup("save");
        let file = root.join("run.sh");
        std::fs::write(&file, "echo 1\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        let save = |content: &str, expected: Option<&str>| file_save(app.state(), arg(&file), content.into(), expected.map(String::from));

        save("echo 2\n", Some("echo 1\n")).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "echo 2\n");
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "no temp file left");

        let err = save("echo 3\n", Some("echo 1\n")).unwrap_err();
        assert_eq!(err, format!("{} was changed on disk since you opened it.", file.display()));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "echo 2\n");
        // Overwrite on request.
        save("echo 3\n", None).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "echo 3\n");

        // Outside the root, or gone.
        assert!(file_save(app.state(), arg(root.parent().unwrap()), "x".into(), None).is_err());
        std::fs::remove_file(&file).unwrap();
        assert!(save("x", None).is_err());

        let view = webview(&app);
        std::fs::write(&file, "a").unwrap();
        ipc(&view, "file_save", serde_json::json!({ "path": file, "content": "b", "expected": "a" })).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "b");
    }

    #[test]
    fn reads_and_saves_through_symlinks() {
        let (root, app) = setup("link");
        let target = root.join("shared.tfvars");
        std::fs::write(&target, "a = 1\n").unwrap();
        std::fs::create_dir_all(root.join("mod")).unwrap();
        std::fs::write(root.join("mod/main.tf"), "").unwrap();
        let link = root.join("mod/link.tfvars");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        // It belongs to the folder it appears in (a module), and says what it links to.
        let got = file_read(app.state(), arg(&link)).unwrap();
        assert_eq!(got.rel_path, "mod/link.tfvars");
        assert!(got.is_module);
        assert_eq!(got.link_target.as_deref(), Some("shared.tfvars"));
        assert_eq!(file_read(app.state(), arg(&target)).unwrap().link_target, None);

        file_save(app.state(), arg(&link), "a = 2\n".into(), Some("a = 1\n".into())).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "a = 2\n");
    }

    #[test]
    fn write_atomic_reports_errors_and_cleans_up() {
        let root = temp_dir("fileedit-atomic");
        let dir = root.join("locked");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.tf");
        std::fs::write(&file, "a").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = write_atomic(&file, "b");
        let entries = std::fs::read_dir(&dir).unwrap().count();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.unwrap_err().starts_with(&*file.to_string_lossy()));
        assert_eq!((entries, std::fs::read_to_string(&file).unwrap().as_str()), (1, "a"));

        // The rename fails (a folder is in the way): the temp file is removed.
        let blocked = root.join("blocked");
        std::fs::create_dir_all(blocked.join("inner")).unwrap();
        assert!(write_atomic(&blocked, "x").is_err());
        assert!(!root.join(".blocked.gestalt-tmp").exists());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);

        assert!(write_atomic(Path::new("/"), "x").unwrap_err().contains("has no parent folder"));
    }
}
