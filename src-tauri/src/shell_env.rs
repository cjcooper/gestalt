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

pub struct ShellEnv {
    /// The shell to capture from.
    shell: String,
    // Async mutex: concurrent callers wait for an in-flight load instead of
    // spawning the shell again.
    inner: Mutex<Option<Loaded>>,
}

impl Default for ShellEnv {
    /// Captures from the user's login shell (`$SHELL`).
    fn default() -> Self {
        Self::with_shell(login_shell(std::env::var("SHELL").ok()))
    }
}

/// `$SHELL`, falling back to zsh (the macOS default) when unset or empty.
fn login_shell(var: Option<String>) -> String {
    var.filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into())
}

impl ShellEnv {
    /// Capture from `shell` instead of `$SHELL`.
    pub fn with_shell(shell: impl Into<String>) -> Self {
        ShellEnv { shell: shell.into(), inner: Mutex::new(None) }
    }

    /// Already loaded with `env`, without running a shell. For tests that
    /// need app state with a known environment.
    #[cfg(test)]
    pub fn preloaded(env: EnvMap) -> Self {
        let status = EnvStatus { shell: None, var_count: env.len(), error: None };
        ShellEnv { shell: String::new(), inner: Mutex::new(Some(Loaded { env: Arc::new(env), status })) }
    }

    /// The captured environment, loading it on first use.
    pub async fn get(&self) -> Arc<EnvMap> {
        let mut guard = self.inner.lock().await;
        if guard.is_none() {
            *guard = Some(load(&self.shell).await);
        }
        guard.as_ref().unwrap().env.clone()
    }

    pub async fn status(&self) -> EnvStatus {
        let mut guard = self.inner.lock().await;
        if guard.is_none() {
            *guard = Some(load(&self.shell).await);
        }
        guard.as_ref().unwrap().status.clone()
    }

    pub async fn reload(&self) -> EnvStatus {
        let mut guard = self.inner.lock().await;
        let loaded = load(&self.shell).await;
        let status = loaded.status.clone();
        *guard = Some(loaded);
        status
    }
}

async fn load(shell: &str) -> Loaded {
    match capture(shell).await {
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
async fn capture(_shell: &str) -> Result<(String, EnvMap), (Option<String>, String)> {
    // Windows GUI apps already get the user's environment.
    Err((None, "Shell environment loading is only needed on macOS/Linux".into()))
}

#[cfg(unix)]
async fn capture(shell: &str) -> Result<(String, EnvMap), (Option<String>, String)> {
    // Interactive first (most people export in .zshrc/.bashrc), then plain login.
    let mut last_err = String::new();
    for flags in [&["-l", "-i"][..], &["-l"][..]] {
        match run_shell(shell, flags, TIMEOUT).await {
            Ok(env) => return Ok((shell.to_string(), env)),
            Err(e) => last_err = e,
        }
    }
    Err((Some(shell.to_string()), last_err))
}

#[cfg(unix)]
async fn run_shell(shell: &str, flags: &[&str], timeout: Duration) -> Result<EnvMap, String> {
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

    // A timeout is reported like any other wait failure.
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .unwrap_or_else(|_| {
            let msg = format!("{shell} {} timed out after {}s", flags.join(" "), timeout.as_secs());
            Err(std::io::Error::other(msg))
        })
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

/// Helpers for tests that need fake executables (a shell, `tofu`).
#[cfg(all(test, unix))]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};

    /// A fresh, empty temp folder unique to this test.
    pub fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gestalt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    /// Write an executable `/bin/sh` script.
    pub fn script(path: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_path_buf()
    }

    /// A window on a mock app, to invoke commands through.
    pub fn webview(app: &tauri::App<tauri::test::MockRuntime>) -> tauri::WebviewWindow<tauri::test::MockRuntime> {
        tauri::WebviewWindowBuilder::new(app, "main", Default::default()).build().unwrap()
    }

    /// Invoke `cmd` with `args` over IPC, as the UI does.
    pub fn ipc(
        webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
        cmd: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, serde_json::Value> {
        let request = tauri::webview::InvokeRequest {
            cmd: cmd.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: tauri::ipc::InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.to_string(),
        };
        tauri::test::get_ipc_response(webview, request).map(|b| b.deserialize().unwrap())
    }

    /// A fake login shell that reports `vars` (`NAME=value` entries) as its
    /// environment, ignoring its arguments.
    pub fn fake_shell(dir: &Path, vars: &[&str]) -> String {
        let entries: String = vars.iter().map(|v| format!("printf '%s\\000' '{v}'\n")).collect();
        script(&dir.join("fake-shell"), &format!("printf '__GESTALT_ENV_BEGIN__'\n{entries}"))
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::testing::{fake_shell, script, temp_dir};
    use super::*;
    use std::path::Path;

    fn var(env: &EnvMap, k: &str) -> Option<String> {
        env.get(&OsString::from(k)).map(|v| v.to_string_lossy().into_owned())
    }

    #[test]
    fn parses_nul_separated_env_and_skips_shell_bookkeeping() {
        let env = parse_env0(b"A=1\0B=x=y\0PWD=/tmp\0MULTI=line1\nline2\0NOEQUALS\0=nokey\0\0");
        assert_eq!(env.get(&OsString::from("A")), Some(&OsString::from("1")));
        assert_eq!(env.get(&OsString::from("B")), Some(&OsString::from("x=y")));
        assert_eq!(env.get(&OsString::from("MULTI")), Some(&OsString::from("line1\nline2")));
        assert!(!env.contains_key(&OsString::from("PWD")));
        assert_eq!(env.len(), 3);
    }

    #[test]
    fn login_shell_defaults_to_zsh() {
        assert_eq!(login_shell(None), "/bin/zsh");
        assert_eq!(login_shell(Some(String::new())), "/bin/zsh");
        assert_eq!(login_shell(Some("/bin/bash".into())), "/bin/bash");
        // Default reads $SHELL up front, without running anything.
        assert_eq!(ShellEnv::default().shell, login_shell(std::env::var("SHELL").ok()));
    }

    #[test]
    fn finds_subslices() {
        assert_eq!(find(b"noise__M__rest", b"__M__"), Some(5));
        assert_eq!(find(b"nothing here", b"__M__"), None);
    }

    #[tokio::test]
    async fn captures_real_shell_env() {
        let env = run_shell("/bin/sh", &["-l"], TIMEOUT).await.expect("sh env");
        assert!(env.contains_key(&OsString::from("PATH")));
    }

    #[tokio::test]
    async fn passes_flags_and_skips_rc_noise_before_the_marker() {
        let dir = temp_dir("shell-flags");
        // Echo our own arguments and the probe flag back as variables, after
        // some rc-file noise on stdout.
        let shell = script(
            &dir.join("sh"),
            r#"echo "welcome!"
printf '__GESTALT_ENV_BEGIN__'
printf 'ARGS=%s\000' "$1 $2 $3"
printf 'PROBE=%s\000' "$GESTALT_ENV_PROBE""#,
        );
        let env = run_shell(shell.to_str().unwrap(), &["-l", "-i"], TIMEOUT).await.unwrap();
        assert_eq!(var(&env, "ARGS").as_deref(), Some("-l -i -c"));
        assert_eq!(var(&env, "PROBE").as_deref(), Some("1"));
        assert_eq!(env.len(), 2);
    }

    #[tokio::test]
    async fn falls_back_to_a_non_interactive_login_shell() {
        let dir = temp_dir("shell-fallback");
        // Fails when interactive, as a broken .zshrc might.
        let shell = script(
            &dir.join("sh"),
            r#"[ "$2" = "-i" ] && exit 3
printf '__GESTALT_ENV_BEGIN__'
printf 'FROM=login\000'"#,
        );
        let env = ShellEnv::with_shell(shell.to_str().unwrap());
        let status = env.status().await;
        assert_eq!(status.shell.as_deref(), shell.to_str());
        assert_eq!(status.var_count, 1);
        assert_eq!(status.error, None);
        assert_eq!(var(&*env.get().await, "FROM").as_deref(), Some("login"));
    }

    #[tokio::test]
    async fn reports_why_capture_failed() {
        let dir = temp_dir("shell-fail");
        let shell = script(&dir.join("sh"), "echo 'no marker here'; exit 3");
        let env = ShellEnv::with_shell(shell.to_str().unwrap());
        // `get` loads on first use too; a failed capture leaves an empty env.
        assert!(env.get().await.is_empty());
        let status = env.status().await;
        assert_eq!(status.shell.as_deref(), shell.to_str());
        assert_eq!(status.var_count, 0);
        // The last attempt's error (plain login) is the one reported.
        assert_eq!(status.error, Some(format!("{} -l produced no environment (exit Some(3))", shell.display())));
    }

    #[tokio::test]
    async fn rejects_an_empty_environment() {
        let dir = temp_dir("shell-empty");
        let shell = script(&dir.join("sh"), "printf '__GESTALT_ENV_BEGIN__'; printf 'PWD=/x\\000'");
        let err = run_shell(shell.to_str().unwrap(), &["-l"], TIMEOUT).await.unwrap_err();
        assert_eq!(err, format!("{} -l produced an empty environment", shell.display()));
    }

    #[tokio::test]
    async fn reports_a_shell_that_cannot_start() {
        let missing = temp_dir("shell-missing").join("no-such-shell");
        let status = ShellEnv::with_shell(missing.to_str().unwrap()).status().await;
        let error = status.error.unwrap();
        assert!(error.starts_with(&format!("Failed to start {}: ", missing.display())), "{error}");
    }

    #[tokio::test]
    async fn gives_up_on_a_hung_shell() {
        let dir = temp_dir("shell-hang");
        let shell = script(&dir.join("sh"), "exec sleep 30");
        let started = std::time::Instant::now();
        let err = run_shell(shell.to_str().unwrap(), &["-l", "-i"], Duration::from_millis(200)).await.unwrap_err();
        assert_eq!(err, format!("{} -l -i timed out after 0s", shell.display()));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn caches_until_reloaded() {
        let dir = temp_dir("shell-reload");
        let value = dir.join("value");
        std::fs::write(&value, "one").unwrap();
        let shell = fake_shell(&dir, &[]);
        // Report the current contents of `value`.
        script(
            Path::new(&shell),
            &format!("printf '__GESTALT_ENV_BEGIN__'; printf 'V=%s\\000' \"$(cat '{}')\"", value.display()),
        );
        let env = ShellEnv::with_shell(shell);
        assert_eq!(var(&*env.get().await, "V").as_deref(), Some("one"));

        std::fs::write(&value, "two").unwrap();
        // Still the cached capture.
        assert_eq!(var(&*env.get().await, "V").as_deref(), Some("one"));
        assert_eq!(env.status().await.var_count, 1);

        let status = env.reload().await;
        assert_eq!(status.var_count, 1);
        assert_eq!(var(&*env.get().await, "V").as_deref(), Some("two"));
    }

    #[tokio::test]
    async fn fake_shell_and_preloaded_helpers() {
        let dir = temp_dir("shell-helpers");
        let env = ShellEnv::with_shell(fake_shell(&dir, &["A=1", "B=two words"]));
        let got = env.get().await;
        assert_eq!(var(&got, "A").as_deref(), Some("1"));
        assert_eq!(var(&got, "B").as_deref(), Some("two words"));

        let pre = ShellEnv::preloaded(EnvMap::from([("X".into(), "y".into())]));
        assert_eq!(pre.status().await.var_count, 1);
        assert_eq!(pre.status().await.shell, None);
        assert_eq!(var(&*pre.get().await, "X").as_deref(), Some("y"));
    }
}
