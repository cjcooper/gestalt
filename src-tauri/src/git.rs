//! Read-only git queries for the Commits tab.

use crate::shell_env::{EnvMap, ShellEnv};
use crate::tofu::Workspace;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Stdio;
use tauri::State;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// A module folder (one holding a `main.tf`) touched by a commit.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModuleChange {
    /// Folder path from the repo root ("." for the root itself).
    pub path: String,
    /// The whole folder was removed by this commit.
    pub deleted: bool,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub hash: String,
    pub short_hash: String,
    pub parents: Vec<String>,
    pub is_merge: bool,
    pub author_name: String,
    pub author_email: String,
    /// ISO 8601 author date.
    pub date: String,
    pub subject: String,
    /// Ref decorations, e.g. ["HEAD -> main", "origin/main", "tag: v1.2"].
    pub refs: Vec<String>,
    /// Module folders changed, compared with the first parent.
    pub modules: Vec<ModuleChange>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLog {
    /// Current branch, or `None` when HEAD is detached.
    pub branch: Option<String>,
    pub commits: Vec<Commit>,
    pub has_more: bool,
    /// The branch's upstream (as of the last fetch by any tool) has commits
    /// that aren't local. Deliberately just a flag: nothing is fetched and no
    /// details about the remote commits are exposed.
    pub remote_has_changes: bool,
    /// Uncommitted work compared with HEAD; only on the first page.
    pub uncommitted: Option<Uncommitted>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Uncommitted {
    /// Changed files (staged, unstaged and untracked; ignored files excluded).
    pub file_count: usize,
    /// Module folders among them, same rules as for commits.
    pub modules: Vec<ModuleChange>,
}

const FIELD: char = '\x1f';
const RECORD: char = '\x1e';
/// Record marker first, so each record is "<fields>\x1f\n\n<changed paths>".
const FORMAT: &str = "--format=%x1e%H%x1f%h%x1f%P%x1f%an%x1f%ae%x1f%aI%x1f%s%x1f%D%x1f";
const MODULE_FILE: &str = "main.tf";

fn git_cmd(dir: &Path, env: &EnvMap) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        // Report paths verbatim rather than C-quoting non-ASCII names.
        .args(["-c", "core.quotePath=false"])
        .envs(env)
        // Never take .git/index.lock: the user may be running git themselves.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

async fn git(dir: &Path, env: &EnvMap, args: &[&str]) -> Result<String, String> {
    let out = git_cmd(dir, env)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("Failed to run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Commits plus, for each, the paths it changed (vs. its first parent).
fn parse_log(out: &str) -> Vec<(Commit, Vec<String>)> {
    out.split(RECORD)
        .filter(|r| !r.trim().is_empty())
        .filter_map(|r| {
            let f: Vec<&str> = r.splitn(9, FIELD).collect();
            let [hash, short, parents, name, email, date, subject, refs, files] = f.as_slice() else {
                return None;
            };
            let parents: Vec<String> = parents.split_whitespace().map(String::from).collect();
            let files = files.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
            let commit = Commit {
                hash: hash.to_string(),
                short_hash: short.to_string(),
                is_merge: parents.len() > 1,
                parents,
                author_name: name.to_string(),
                author_email: email.to_string(),
                date: date.to_string(),
                subject: subject.to_string(),
                refs: refs.split(", ").filter(|s| !s.is_empty()).map(String::from).collect(),
                modules: Vec::new(),
            };
            Some((commit, files))
        })
        .collect()
}

/// Folder containing `path` ("" for the repo root).
fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `rev:path` object spec; `rev:` alone is the root tree.
fn spec(rev: &str, dir: &str, file: Option<&str>) -> String {
    match (dir, file) {
        ("", Some(f)) => format!("{rev}:{f}"),
        ("", None) => format!("{rev}:"),
        (d, Some(f)) => format!("{rev}:{d}/{f}"),
        (d, None) => format!("{rev}:{d}"),
    }
}

/// Object types for many `rev:path` specs in one git process ("missing" if absent).
async fn object_types(dir: &Path, env: &EnvMap, specs: &[String]) -> Result<Vec<String>, String> {
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = git_cmd(dir, env)
        .args(["cat-file", "--batch-check=%(objecttype)"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to run git: {e}"))?;
    let input = specs.join("\n") + "\n";
    let mut stdin = child.stdin.take().unwrap();
    // Write concurrently with reading so a large batch can't deadlock on pipes.
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
    });
    let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
    let _ = writer.await;
    let types: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| if l.ends_with(" missing") { "missing".into() } else { l.trim().to_string() })
        .collect();
    if types.len() != specs.len() {
        return Err("git cat-file returned an unexpected number of results".into());
    }
    Ok(types)
}

/// Fill in each commit's module changes: folders of changed files that held
/// a `main.tf` after the commit (changed) or only before it (changed, or
/// deleted if the whole folder is gone).
///
/// Entries must agree with the local checkout (`top` is the repo's top-level
/// folder on disk): a changed folder is listed only if it exists locally, and
/// a deleted one only if it still doesn't.
async fn fill_module_changes(
    dir: &Path,
    top: &Path,
    env: &EnvMap,
    entries: &mut [(Commit, Vec<String>)],
) -> Result<(), String> {
    // Per commit: candidate folders, and the 3 specs queried for each.
    let mut plan: Vec<Vec<String>> = Vec::new();
    let mut specs = Vec::new();
    for (commit, files) in entries.iter() {
        let dirs: BTreeSet<&str> = files.iter().map(|f| parent_dir(f)).collect();
        let parent = commit.parents.first();
        for d in &dirs {
            specs.push(spec(&commit.hash, d, Some(MODULE_FILE)));
            // A root commit has no parent: its "before" is simply missing.
            specs.push(parent.map_or_else(|| "0000000000000000000000000000000000000000:x".into(), |p| spec(p, d, Some(MODULE_FILE))));
            specs.push(spec(&commit.hash, d, None));
        }
        plan.push(dirs.into_iter().map(String::from).collect());
    }

    let types = object_types(dir, env, &specs).await?;
    let mut results = types.chunks(3);
    for ((commit, _), dirs) in entries.iter_mut().zip(plan) {
        for d in dirs {
            let [after, before, folder] = results.next().unwrap() else { unreachable!() };
            let exists_locally = top.join(&d).is_dir();
            let path = if d.is_empty() { ".".to_string() } else { d };
            let change = if after == "blob" {
                Some(false)
            } else if before == "blob" {
                Some(folder != "tree")
            } else {
                None
            };
            match change {
                Some(deleted) if deleted != exists_locally => commit.modules.push(ModuleChange { path, deleted }),
                _ => {}
            }
        }
    }
    Ok(())
}

/// History of the current branch along first parents: a merged branch shows
/// up as its merge commit only, with changes compared to the first parent.
pub async fn first_parent_log(dir: &Path, env: &EnvMap, skip: usize, limit: usize) -> Result<GitLog, String> {
    let branch = git(dir, env, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let skip_arg = format!("--skip={skip}");
    let count_arg = format!("--max-count={}", limit + 1);
    let args = [
        "log",
        "--first-parent",
        "--diff-merges=first-parent",
        "--name-only",
        "--no-renames",
        "--decorate=short",
        // Local view only: no remote-tracking branch labels.
        "--decorate-refs-exclude=refs/remotes/",
        &skip_arg,
        &count_arg,
        FORMAT,
        "HEAD",
    ];
    let out = match git(dir, env, &args).await {
        Ok(out) => out,
        // A brand-new repo with no commits yet.
        Err(e) if e.contains("does not have any commits") || e.contains("unknown revision") => String::new(),
        Err(e) => return Err(e),
    };
    let mut entries = parse_log(&out);
    let has_more = entries.len() > limit;
    entries.truncate(limit);
    let top = toplevel(dir, env).await?;
    fill_module_changes(dir, &top, env, &mut entries).await?;
    let uncommitted = if skip == 0 { Some(uncommitted_changes(dir, env).await?) } else { None };
    let remote_has_changes = remote_has_changes(dir, env).await;
    Ok(GitLog {
        branch,
        commits: entries.into_iter().map(|(c, _)| c).collect(),
        has_more,
        remote_has_changes,
        uncommitted,
    })
}

async fn toplevel(dir: &Path, env: &EnvMap) -> Result<std::path::PathBuf, String> {
    Ok(std::path::PathBuf::from(git(dir, env, &["rev-parse", "--show-toplevel"]).await?.trim()))
}

/// Whether the upstream ref has commits not in HEAD, using only what is
/// already in the local repo (no fetch). False when there's no upstream.
async fn remote_has_changes(dir: &Path, env: &EnvMap) -> bool {
    git(dir, env, &["rev-list", "--max-count=1", "HEAD..@{upstream}"])
        .await
        .is_ok_and(|out| !out.trim().is_empty())
}

/// NUL-separated path list from a git command.
async fn paths_z(dir: &Path, env: &EnvMap, args: &[&str]) -> Result<Vec<String>, String> {
    Ok(git(dir, env, args).await?.split('\0').filter(|p| !p.is_empty()).map(String::from).collect())
}

/// Module folders with uncommitted changes: "after" is the working tree on
/// disk, "before" is HEAD. Runs from the repo top level so paths match the
/// commit list even when the opened folder is a subfolder of the repo.
pub async fn uncommitted_changes(dir: &Path, env: &EnvMap) -> Result<Uncommitted, String> {
    let top = toplevel(dir, env).await?;
    let has_head = git(&top, env, &["rev-parse", "--verify", "--quiet", "HEAD"]).await.is_ok();

    let mut files: BTreeSet<String> = BTreeSet::new();
    if has_head {
        // Staged + unstaged changes against HEAD (deletions included).
        files.extend(paths_z(&top, env, &["diff", "--name-only", "--no-renames", "-z", "HEAD"]).await?);
    } else {
        // No commits yet: everything in the index counts.
        files.extend(paths_z(&top, env, &["ls-files", "-z", "--cached"]).await?);
    }
    files.extend(paths_z(&top, env, &["ls-files", "-z", "--others", "--exclude-standard"]).await?);

    let dirs: BTreeSet<&str> = files.iter().map(|f| parent_dir(f)).collect();
    let specs: Vec<String> = if has_head {
        dirs.iter().map(|d| spec("HEAD", d, Some(MODULE_FILE))).collect()
    } else {
        Vec::new()
    };
    let before = object_types(&top, env, &specs).await?;

    let mut modules = Vec::new();
    for (i, d) in dirs.iter().enumerate() {
        let folder = top.join(d);
        let had = before.get(i).is_some_and(|t| t == "blob");
        let path = if d.is_empty() { ".".to_string() } else { d.to_string() };
        if folder.join(MODULE_FILE).is_file() {
            modules.push(ModuleChange { path, deleted: false });
        } else if had {
            modules.push(ModuleChange { path, deleted: !folder.is_dir() });
        }
    }
    Ok(Uncommitted { file_count: files.len(), modules })
}

async fn is_repo(dir: &Path, env: &EnvMap) -> bool {
    git(dir, env, &["rev-parse", "--is-inside-work-tree"]).await.is_ok_and(|s| s.trim() == "true")
}

/// Whether the opened folder is inside a git work tree.
#[tauri::command]
pub async fn git_is_repo(workspace: State<'_, Workspace>, shell: State<'_, ShellEnv>) -> Result<bool, String> {
    let Some(root) = workspace.root.lock().unwrap().clone() else { return Ok(false) };
    let env = shell.get().await;
    Ok(is_repo(&root, &env).await)
}

/// Just the uncommitted-changes summary (for refreshing on file changes).
#[tauri::command]
pub async fn git_uncommitted(workspace: State<'_, Workspace>, shell: State<'_, ShellEnv>) -> Result<Uncommitted, String> {
    let root = workspace.root.lock().unwrap().clone().ok_or("No folder is open.")?;
    let env = shell.get().await;
    uncommitted_changes(&root, &env).await
}

#[tauri::command]
pub async fn git_log(
    workspace: State<'_, Workspace>,
    shell: State<'_, ShellEnv>,
    skip: usize,
    limit: usize,
) -> Result<GitLog, String> {
    let root = workspace.root.lock().unwrap().clone().ok_or("No folder is open.")?;
    let env = shell.get().await;
    first_parent_log(&root, &env, skip, limit.clamp(1, 1000)).await.map_err(|e| {
        if e.contains("not a git repository") { format!("{} is not in a git repository.", root.display()) } else { e }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Repo(std::path::PathBuf);

    impl Repo {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("gestalt-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let r = Repo(dir);
            r.git(&["init", "-q", "-b", "main"]);
            r
        }

        fn git(&self, args: &[&str]) {
            let st = std::process::Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(args)
                .env("GIT_AUTHOR_NAME", "T")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "T")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(st.success(), "git {args:?}");
        }

        fn write(&self, path: &str, content: &str) {
            let p = self.0.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }

        fn commit(&self, msg: &str) {
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "--allow-empty", "-m", msg]);
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mods(c: &Commit) -> Vec<(String, bool)> {
        c.modules.iter().map(|m| (m.path.clone(), m.deleted)).collect()
    }

    #[tokio::test]
    async fn merged_branch_shows_only_its_merge_commit() {
        let r = Repo::new("git-merge");
        r.commit("A on main");
        r.git(&["checkout", "-q", "-b", "feature"]);
        r.commit("B on feature");
        r.commit("C on feature");
        r.git(&["checkout", "-q", "main"]);
        r.commit("D on main");
        r.git(&["merge", "-q", "--no-ff", "feature", "-m", "Merge feature"]);
        r.git(&["tag", "v1"]);

        let env = EnvMap::new();
        let log = first_parent_log(&r.0, &env, 0, 50).await.unwrap();
        let subjects: Vec<&str> = log.commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["Merge feature", "D on main", "A on main"]);
        assert_eq!(log.branch.as_deref(), Some("main"));
        assert!(log.commits[0].is_merge && !log.commits[1].is_merge);
        assert!(log.commits[0].refs.contains(&"HEAD -> main".to_string()));
        assert!(log.commits[0].refs.contains(&"tag: v1".to_string()));
        assert!(!log.has_more);

        let page = first_parent_log(&r.0, &env, 1, 1).await.unwrap();
        assert_eq!(page.commits[0].subject, "D on main");
        assert!(page.has_more);
    }

    #[tokio::test]
    async fn lists_module_folders_and_deletions() {
        let r = Repo::new("git-modules");
        r.write("README.md", "x");
        r.write("a/main.tf", "");
        r.write("a/vars.tf", "");
        r.write("b/main.tf", "");
        r.write("docs/notes.md", "");
        r.write("lib/helper.tf", ""); // .tf but no main.tf: not a module folder
        r.commit("initial");

        r.write("a/vars.tf", "changed");
        r.write("docs/notes.md", "changed");
        r.write("lib/helper.tf", "changed");
        r.commit("edit a, docs, lib");

        r.git(&["rm", "-q", "-r", "b"]);
        r.git(&["rm", "-q", "a/main.tf"]);
        r.commit("delete folder b; drop a/main.tf but keep folder a");

        // Feature branch touching c, merged: the merge commit lists c.
        r.git(&["checkout", "-q", "-b", "feature"]);
        r.write("c/main.tf", "");
        r.commit("add c");
        r.git(&["checkout", "-q", "main"]);
        r.git(&["merge", "-q", "--no-ff", "feature", "-m", "Merge feature"]);

        let log = first_parent_log(&r.0, &EnvMap::new(), 0, 50).await.unwrap();
        let by_subject = |s: &str| log.commits.iter().find(|c| c.subject.starts_with(s)).unwrap();

        assert_eq!(mods(by_subject("Merge feature")), [("c".into(), false)]);
        assert_eq!(mods(by_subject("delete folder b")), [("a".into(), false), ("b".into(), true)]);
        assert_eq!(mods(by_subject("edit a")), [("a".into(), false)]);
        // Root commit: compared with nothing. b no longer exists locally, so
        // its earlier change isn't listed.
        assert_eq!(mods(by_subject("initial")), [("a".into(), false)]);
    }

    #[tokio::test]
    async fn folders_must_match_the_local_checkout() {
        let r = Repo::new("git-local");
        r.write("x/main.tf", "");
        r.write("y/main.tf", "");
        r.commit("add x and y");
        r.git(&["rm", "-q", "-r", "x"]);
        r.commit("delete x");
        r.write("x/main.tf", "back");
        r.commit("recreate x");
        r.write("y/main.tf", "edit");
        r.commit("edit y");
        // y deleted locally but not committed: its committed changes aren't shown.
        std::fs::remove_dir_all(r.0.join("y")).unwrap();

        let log = first_parent_log(&r.0, &EnvMap::new(), 0, 50).await.unwrap();
        let by_subject = |s: &str| log.commits.iter().find(|c| c.subject == s).unwrap();
        assert_eq!(mods(by_subject("edit y")), []);
        assert_eq!(mods(by_subject("recreate x")), [("x".into(), false)]);
        // x exists again locally, so "deleted" would contradict the checkout.
        assert_eq!(mods(by_subject("delete x")), []);
        assert_eq!(mods(by_subject("add x and y")), [("x".into(), false)]);
    }

    #[tokio::test]
    async fn local_view_with_remote_indicator() {
        // "server" repo with a commit the local clone hasn't pulled yet.
        let server = Repo::new("git-server");
        server.commit("one");
        let local_dir = std::env::temp_dir().join(format!("gestalt-git-clone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&local_dir);
        let st = std::process::Command::new("git")
            .args(["clone", "-q"])
            .arg(&server.0)
            .arg(&local_dir)
            .status()
            .unwrap();
        assert!(st.success());
        let local = Repo(local_dir);
        let env = EnvMap::new();

        let log = first_parent_log(&local.0, &env, 0, 50).await.unwrap();
        assert!(!log.remote_has_changes);
        // Remote-tracking labels are not shown.
        assert!(log.commits[0].refs.iter().all(|r| !r.contains("origin")), "{:?}", log.commits[0].refs);

        server.commit("two");
        local.git(&["fetch", "-q"]); // done by the user/IDE, never by Gestalt
        let log = first_parent_log(&local.0, &env, 0, 50).await.unwrap();
        assert!(log.remote_has_changes);
        // Still only the local commits.
        let subjects: Vec<&str> = log.commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["one"]);
    }

    #[tokio::test]
    async fn uncommitted_module_changes() {
        let r = Repo::new("git-uncommitted");
        r.write(".gitignore", ".terraform/\n");
        r.write("a/main.tf", "");
        r.write("b/main.tf", "");
        r.write("c/main.tf", "");
        r.write("docs/x.md", "");
        r.commit("initial");

        let clean = uncommitted_changes(&r.0, &EnvMap::new()).await.unwrap();
        assert_eq!(clean, Uncommitted { file_count: 0, modules: vec![] });

        r.write("a/main.tf", "edited, unstaged");
        r.write("b/new.tf", "staged");
        r.git(&["add", "b/new.tf"]);
        std::fs::remove_dir_all(r.0.join("c")).unwrap(); // deleted, unstaged
        r.write("d/main.tf", ""); // new module, untracked
        r.write("docs/x.md", "edited"); // not a module folder
        r.write("a/.terraform/providers.txt", ""); // ignored

        // Query from a subfolder: paths are still from the repo root.
        let u = uncommitted_changes(&r.0.join("docs"), &EnvMap::new()).await.unwrap();
        assert_eq!(u.file_count, 5);
        let mods: Vec<(String, bool)> = u.modules.iter().map(|m| (m.path.clone(), m.deleted)).collect();
        assert_eq!(mods, [("a".into(), false), ("b".into(), false), ("c".into(), true), ("d".into(), false)]);
    }

    #[tokio::test]
    async fn repo_detection() {
        let r = Repo::new("git-isrepo");
        assert!(is_repo(&r.0, &EnvMap::new()).await);

        let dir = std::env::temp_dir().join(format!("gestalt-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Only meaningful if the temp dir isn't itself inside a repo.
        if !is_repo(&dir, &EnvMap::new()).await {
            assert!(first_parent_log(&dir, &EnvMap::new(), 0, 10).await.is_err());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// Manual check against a real repo:
/// `GESTALT_GIT_DIR=/path/to/repo cargo test print_real_log -- --ignored --nocapture`
#[cfg(test)]
#[tokio::test]
#[ignore]
async fn print_real_log() {
    let dir = std::env::var("GESTALT_GIT_DIR").expect("set GESTALT_GIT_DIR");
    let start = std::time::Instant::now();
    let log = first_parent_log(Path::new(&dir), &EnvMap::new(), 0, 200).await.unwrap();
    println!("{} commits in {:?}, remote_has_changes={}", log.commits.len(), start.elapsed(), log.remote_has_changes);
    for c in log.commits.iter().take(12) {
        println!("{} {}", c.short_hash, c.subject);
        for m in &c.modules {
            println!("    {}{}", m.path, if m.deleted { "  (deleted)" } else { "" });
        }
    }
    let deleted: Vec<_> = log.commits.iter().flat_map(|c| c.modules.iter().filter(|m| m.deleted).map(move |m| (&c.short_hash, &m.path))).collect();
    println!("deleted in page: {deleted:?}");
}
