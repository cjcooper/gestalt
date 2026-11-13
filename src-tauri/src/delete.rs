//! Deleting a file or folder from the tree (right-click › Move to Trash…).
//! It goes to the Trash, so it can be put back.

use crate::file_edit::normalize;
use std::path::Path;

/// Check `path` can be deleted (it's below the workspace root, not the root
/// itself) and hand it to `remove`. A symlink is removed as a link: what it
/// points to stays.
pub fn trash_path(root: Option<&Path>, path: &str, remove: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
    let root = normalize(root.ok_or("No folder is open.")?);
    let target = normalize(Path::new(path));
    if target == root || !target.starts_with(&root) {
        return Err(format!("{} is not inside {}", target.display(), root.display()));
    }
    // symlink_metadata: a link (even a broken one) is there to delete.
    std::fs::symlink_metadata(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    remove(&target).map_err(|e| format!("Couldn't move {} to the Trash: {e}", target.display()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::shell_env::testing::temp_dir;
    use std::cell::RefCell;

    #[test]
    fn hands_over_paths_inside_the_root() {
        let root = temp_dir("delete-ok");
        std::fs::create_dir_all(root.join("mod/sub")).unwrap();
        std::fs::write(root.join("mod/main.tf"), "").unwrap();
        let removed = RefCell::new(vec![]);
        let remove = |p: &Path| {
            removed.borrow_mut().push(p.to_path_buf());
            Ok(())
        };

        trash_path(Some(&root), &root.join("mod/main.tf").to_string_lossy(), remove).unwrap();
        trash_path(Some(&root), &root.join("mod/./sub/../sub").to_string_lossy(), remove).unwrap();
        assert_eq!(*removed.borrow(), [root.join("mod/main.tf"), root.join("mod/sub")]);
    }

    #[test]
    fn deletes_a_link_not_what_it_points_to() {
        let root = temp_dir("delete-link");
        std::fs::write(root.join("real.tf"), "").unwrap();
        std::os::unix::fs::symlink(root.join("real.tf"), root.join("link.tf")).unwrap();
        std::os::unix::fs::symlink(root.join("nowhere"), root.join("broken.tf")).unwrap();
        let mut removed = vec![];
        for name in ["link.tf", "broken.tf"] {
            trash_path(Some(&root), &root.join(name).to_string_lossy(), |p| {
                removed.push(p.to_path_buf());
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(removed, [root.join("link.tf"), root.join("broken.tf")]);
    }

    #[test]
    fn refuses_the_root_outside_paths_and_missing_ones() {
        let base = temp_dir("delete-refuse");
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(base.join("outside.tf"), "").unwrap();
        let removed = RefCell::new(0);
        let try_ = |ws: Option<&Path>, p: &Path| {
            trash_path(ws, &p.to_string_lossy(), |_| {
                *removed.borrow_mut() += 1;
                Ok(())
            })
        };

        assert_eq!(try_(None, &root).unwrap_err(), "No folder is open.");
        let ws = Some(root.as_path());
        assert!(try_(ws, &root).unwrap_err().contains("is not inside"));
        assert!(try_(ws, &root.join("..")).unwrap_err().contains("is not inside"));
        assert!(try_(ws, &root.join("../outside.tf")).unwrap_err().contains("is not inside"));
        let missing = root.join("missing.tf");
        assert!(try_(ws, &missing).unwrap_err().starts_with(&*missing.to_string_lossy()));
        // The one allowed call, so the checker is known to work.
        std::fs::write(root.join("ok.tf"), "").unwrap();
        try_(ws, &root.join("ok.tf")).unwrap();
        assert_eq!(*removed.borrow(), 1);
    }

    #[test]
    fn reports_a_failed_removal() {
        let root = temp_dir("delete-fail");
        std::fs::write(root.join("a.tf"), "").unwrap();
        let err = trash_path(Some(&root), &root.join("a.tf").to_string_lossy(), |_| Err("no Trash here".into())).unwrap_err();
        assert_eq!(err, format!("Couldn't move {} to the Trash: no Trash here", root.join("a.tf").display()));
    }
}
