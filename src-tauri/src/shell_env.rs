//! Capture the user's login-shell environment.
//!
//! GUI apps launched from Finder/Dock inherit launchd's bare environment, so
//! credentials and settings exported in `~/.zshrc` etc. (AWS_PROFILE,
//! TF_VAR_*, PATH additions, ...) are missing. We run the user's shell once as
//! an interactive login shell, dump its environment, and overlay that on every
//! tofu process we spawn. Our own process env is never mutated.

use serde::Serialize;
use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

pub type EnvMap = HashMap<OsString, OsString>;

/// Printed right before the env dump so rc-file noise on stdout is skipped.
const MARKER: &str = "__GESTALT_ENV_BEGIN__";
const TIMEOUT: Duration = Duration::from_secs(15);

/// Shell-session bookkeeping that would be wrong or meaningless for tofu.
const SKIP: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvStatus {
    pub shell: Option<String>,
    pub var_count: usize,
    pub error: Option<String>,
}

struct Loaded {
    env: Arc<EnvMap>,
    status: EnvStatus,
}

#[derive(Default)]
pub struct ShellEnv {
    // Async mutex: concurrent callers wait for an in-flight load instead of
    // spawning the shell again.
    inner: Mutex<Option<Loaded>>,
}

impl ShellEnv {
    /// The captured environment, loading it on first use.
    pub async fn get(&self) -> Arc<EnvMap> {
        let mut guard = self.inner.lock().await;
        if guard.is_none() {
            *guard = Some(load().await);
        }
        guard.as_ref().unwrap().env.clone()
    }

    pub async fn status(&self) -> EnvStatus {
        let mut guard = self.inner.lock().await;
        if guard.is_none() {
            *guard = Some(load().await);
        }
        guard.as_ref().unwrap().status.clone()
    }

    pub async fn reload(&self) -> EnvStatus {
        let mut guard = self.inner.lock().await;
        let loaded = load().await;
        let status = loaded.status.clone();
        *guard = Some(loaded);
        status
    }
}

async fn load() -> Loaded {
    match capture().await {
        Ok((shell, env)) => Loaded {
            status: EnvStatus { shell: Some(shell), var_count: env.len(), error: None },
            env: Arc::new(env),
        },
        Err((shell, e)) => Loaded {
            status: EnvStatus { shell, var_count: 0, error: Some(e) },
            env: Arc::new(EnvMap::new()),
        },
    }
}

#[cfg(not(unix))]
async fn capture() -> Result<(String, EnvMap), (Option<String>, String)> {
    // Windows GUI apps already get the user's environment.
    Err((None, "Shell environment loading is only needed on macOS/Linux".into()))
}

#[cfg(unix)]
async fn capture() -> Result<(String, EnvMap), (Option<String>, String)> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".into());

    // Interactive first (most people export in .zshrc/.bashrc), then plain login.
    let mut last_err = String::new();
    for flags in [&["-l", "-i"][..], &["-l"][..]] {
        match run_shell(&shell, flags).await {
            Ok(env) => return Ok((shell, env)),
            Err(e) => last_err = e,
        }
    }
    Err((Some(shell), last_err))
}

#[cfg(unix)]
async fn run_shell(shell: &str, flags: &[&str]) -> Result<EnvMap, String> {
    use std::process::Stdio;
    use tokio::process::Command;

    let script = format!("printf '%s' '{MARKER}'; command env -0");
    let child = Command::new(shell)
        .args(flags)
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Tell rc files that care (e.g. to skip slow prompts) who's asking.
        .env("GESTALT_ENV_PROBE", "1")
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Failed to start {shell}: {e}"))?;

    let out = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("{shell} {} timed out after {}s", flags.join(" "), TIMEOUT.as_secs()))?
        .map_err(|e| e.to_string())?;

    let stdout = out.stdout;
    let start = find(&stdout, MARKER.as_bytes())
        .ok_or_else(|| format!("{shell} {} produced no environment (exit {:?})", flags.join(" "), out.status.code()))?
        + MARKER.len();
    let env = parse_env0(&stdout[start..]);
    if env.is_empty() {
        return Err(format!("{shell} {} produced an empty environment", flags.join(" ")));
    }
    Ok(env)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(unix)]
fn parse_env0(bytes: &[u8]) -> EnvMap {
    use std::os::unix::ffi::OsStringExt;
    bytes
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let eq = entry.iter().position(|b| *b == b'=')?;
            let (k, v) = (&entry[..eq], &entry[eq + 1..]);
            if k.is_empty() || SKIP.iter().any(|s| s.as_bytes() == k) {
                return None;
            }
            Some((OsString::from_vec(k.to_vec()), OsString::from_vec(v.to_vec())))
        })
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn parses_nul_separated_env_and_skips_shell_bookkeeping() {
        let env = parse_env0(b"A=1\0B=x=y\0PWD=/tmp\0MULTI=line1\nline2\0\0");
        assert_eq!(env.get(&OsString::from("A")), Some(&OsString::from("1")));
        assert_eq!(env.get(&OsString::from("B")), Some(&OsString::from("x=y")));
        assert_eq!(env.get(&OsString::from("MULTI")), Some(&OsString::from("line1\nline2")));
        assert!(!env.contains_key(&OsString::from("PWD")));
        assert_eq!(env.len(), 3);
    }

    #[tokio::test]
    async fn captures_real_shell_env() {
        let env = run_shell("/bin/sh", &["-l"]).await.expect("sh env");
        assert!(env.contains_key(&OsString::from("PATH")));
    }
}
