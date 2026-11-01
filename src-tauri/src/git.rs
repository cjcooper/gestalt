//! Read-only git queries for the Commits tab.

use crate::shell_env::{EnvMap, ShellEnv};
use crate::tofu::Workspace;
use serde::Serialize;
use std::path::Path;
use std::process::Stdio;
use tauri::State;
use tokio::process::Command;

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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLog {
    /// Current branch, or `None` when HEAD is detached.
    pub branch: Option<String>,
    pub commits: Vec<Commit>,
    pub has_more: bool,
}

const FIELD: char = '\x1f';
const RECORD: char = '\x1e';
const FORMAT: &str = "--format=%H%x1f%h%x1f%P%x1f%an%x1f%ae%x1f%aI%x1f%s%x1f%D%x1e";

async fn git(dir: &Path, env: &EnvMap, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .envs(env)
        // Never take .git/index.lock: the user may be running git themselves.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("Failed to run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn parse_log(out: &str) -> Vec<Commit> {
    out.split(RECORD)
        .map(|r| r.trim_start_matches('\n'))
        .filter(|r| !r.is_empty())
        .filter_map(|r| {
            let f: Vec<&str> = r.split(FIELD).collect();
            let [hash, short, parents, name, email, date, subject, refs] = f.as_slice() else {
                return None;
            };
            let parents: Vec<String> = parents.split_whitespace().map(String::from).collect();
            Some(Commit {
                hash: hash.to_string(),
                short_hash: short.to_string(),
                is_merge: parents.len() > 1,
                parents,
                author_name: name.to_string(),
                author_email: email.to_string(),
                date: date.to_string(),
                subject: subject.to_string(),
                refs: refs.split(", ").filter(|s| !s.is_empty()).map(String::from).collect(),
            })
        })
        .collect()
}

/// History of the current branch along first parents: a merged branch shows
/// up as its merge commit only.
pub async fn first_parent_log(dir: &Path, env: &EnvMap, skip: usize, limit: usize) -> Result<GitLog, String> {
    let branch = git(dir, env, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let skip_arg = format!("--skip={skip}");
    let count_arg = format!("--max-count={}", limit + 1);
    let out = match git(dir, env, &["log", "--first-parent", "--decorate=short", &skip_arg, &count_arg, FORMAT, "HEAD"]).await {
        Ok(out) => out,
        // A brand-new repo with no commits yet.
        Err(e) if e.contains("does not have any commits") || e.contains("unknown revision") => String::new(),
        Err(e) => return Err(e),
    };
    let mut commits = parse_log(&out);
    let has_more = commits.len() > limit;
    commits.truncate(limit);
    Ok(GitLog { branch, commits, has_more })
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

    fn sh(dir: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
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

    #[tokio::test]
    async fn merged_branch_shows_only_its_merge_commit() {
        let dir = std::env::temp_dir().join(format!("gestalt-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        sh(&dir, &["init", "-q", "-b", "main"]);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "A on main"]);
        sh(&dir, &["checkout", "-q", "-b", "feature"]);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "B on feature"]);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "C on feature"]);
        sh(&dir, &["checkout", "-q", "main"]);
        sh(&dir, &["commit", "-q", "--allow-empty", "-m", "D on main"]);
        sh(&dir, &["merge", "-q", "--no-ff", "feature", "-m", "Merge feature"]);
        sh(&dir, &["tag", "v1"]);

        let env = EnvMap::new();
        let log = first_parent_log(&dir, &env, 0, 50).await.unwrap();
        let subjects: Vec<&str> = log.commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["Merge feature", "D on main", "A on main"]);
        assert_eq!(log.branch.as_deref(), Some("main"));
        assert!(log.commits[0].is_merge && !log.commits[1].is_merge);
        assert!(log.commits[0].refs.contains(&"HEAD -> main".to_string()));
        assert!(log.commits[0].refs.contains(&"tag: v1".to_string()));
        assert!(!log.has_more);

        let page = first_parent_log(&dir, &env, 1, 1).await.unwrap();
        assert_eq!(page.commits[0].subject, "D on main");
        assert!(page.has_more);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn not_a_repo_errors() {
        let dir = std::env::temp_dir().join(format!("gestalt-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Guard against the temp dir itself being inside a repo.
        if git(&dir, &EnvMap::new(), &["rev-parse"]).await.is_err() {
            assert!(first_parent_log(&dir, &EnvMap::new(), 0, 10).await.is_err());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
