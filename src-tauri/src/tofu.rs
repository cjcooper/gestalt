//! Spawning OpenTofu and streaming its output to the UI.
//!
//! Applies only ever run against a saved plan file produced by a previous
//! plan run, so what gets applied is exactly what the user reviewed.
//!
//! Events emitted:
//! - `tofu://output` — one per line of stdout/stderr ([`OutputEvent`])
//! - `tofu://exit`   — once when the process ends ([`ExitEvent`])

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::env_file;
use crate::shell_env::{EnvMap, ShellEnv};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Init,
    Plan,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputEvent {
    run_id: u64,
    stream: &'static str,
    line: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitEvent {
    run_id: u64,
    /// `None` when the process was killed by a signal.
    code: Option<i32>,
    success: bool,
    cancelled: bool,
    /// Plan runs only: whether a saved plan with changes is ready to apply.
    has_changes: Option<bool>,
    error: Option<String>,
}

/// What a run is doing, which decides how its exit is handled.
enum RunKind {
    Init,
    Plan { file: PathBuf },
    Apply { file: PathBuf },
}

struct ActiveRun {
    dir: PathBuf,
    pid: Option<u32>,
    interrupted: bool,
    kill: Option<oneshot::Sender<()>>,
}

struct SavedPlan {
    dir: PathBuf,
    file: PathBuf,
}

/// The folder opened in the UI. `.gestalt.env` lookup never goes above it.
#[derive(Default)]
pub struct Workspace {
    pub root: Mutex<Option<PathBuf>>,
}

#[derive(Default)]
pub struct Runs {
    next_id: AtomicU64,
    active: Mutex<HashMap<u64, ActiveRun>>,
    /// Plans with pending changes, keyed by the run id that produced them.
    plans: Mutex<HashMap<u64, SavedPlan>>,
}

/// Look up a variable in the shell env, falling back to our own process env.
fn env_var(env: &EnvMap, key: &str) -> Option<OsString> {
    env.get(OsStr::new(key)).cloned().or_else(|| std::env::var_os(key))
}

/// Find the `tofu` binary on the shell's PATH, then common install locations.
/// `GESTALT_TOFU_BIN` (in the shell or app env) overrides.
fn tofu_bin(env: &EnvMap) -> Result<PathBuf, String> {
    if let Some(p) = env_var(env, "GESTALT_TOFU_BIN") {
        return Ok(PathBuf::from(p));
    }
    let exe = if cfg!(windows) { "tofu.exe" } else { "tofu" };
    let from_path = env_var(env, "PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    let fallbacks = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from);
    from_path
        .into_iter()
        .chain(fallbacks)
        .map(|d| d.join(exe))
        .find(|p| p.is_file())
        .ok_or_else(|| "Could not find `tofu` on PATH. Set GESTALT_TOFU_BIN to its location.".into())
}

/// Saved plan files live in the app cache, never in the user's repo: they can
/// contain secrets and must not be committed.
fn plans_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_cache_dir().map_err(|e| e.to_string())?.join("plans");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Remove plan files left over from a previous session.
pub fn clear_stale_plans(app: &AppHandle) {
    if let Ok(dir) = plans_dir(app) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn pump<R: AsyncRead + Unpin + Send + 'static>(
    app: AppHandle,
    run_id: u64,
    stream: &'static str,
    reader: R,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                    let _ = app.emit("tofu://output", OutputEvent { run_id, stream, line });
                }
            }
        }
    })
}

/// Register the run and start tofu. The caller has already allocated `run_id`.
fn spawn_run(
    app: &AppHandle,
    runs: &Runs,
    env: ResolvedEnv,
    run_id: u64,
    dir: PathBuf,
    args: Vec<OsString>,
    kind: RunKind,
) -> Result<(), String> {
    {
        let mut active = runs.active.lock().unwrap();
        // Two runs in one folder would just fight over the state lock.
        if active.values().any(|r| r.dir == dir) {
            return Err(format!("A tofu command is already running in {}", dir.display()));
        }
        active.insert(run_id, ActiveRun { dir: dir.clone(), pid: None, interrupted: false, kill: None });
    }

    let spawned = tofu_bin(&env.vars).and_then(|bin| {
        Command::new(bin)
            .args(&args)
            .current_dir(&dir)
            // `env` is the complete environment, so `unset` in .gestalt.env works.
            .env_clear()
            .envs(&env.vars)
            .env("TF_IN_AUTOMATION", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Failed to start tofu: {e}"))
    });
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            runs.active.lock().unwrap().remove(&run_id);
            return Err(e);
        }
    };

    let (kill_tx, kill_rx) = oneshot::channel();
    if let Some(run) = runs.active.lock().unwrap().get_mut(&run_id) {
        run.pid = child.id();
        run.kill = Some(kill_tx);
    }

    // Emitted before the output pumps start, so these lead the run's output.
    for line in env.notes {
        let _ = app.emit("tofu://output", OutputEvent { run_id, stream: "gestalt", line });
    }

    let out = pump(app.clone(), run_id, "stdout", child.stdout.take().unwrap());
    let err = pump(app.clone(), run_id, "stderr", child.stderr.take().unwrap());
    let app = app.clone();

    tauri::async_runtime::spawn(async move {
        let status = tokio::select! {
            s = child.wait() => s,
            _ = kill_rx => {
                let _ = child.start_kill();
                child.wait().await
            }
        };
        let _ = out.await;
        let _ = err.await;

        let runs = app.state::<Runs>();
        let cancelled = runs.active.lock().unwrap().remove(&run_id).is_some_and(|r| r.interrupted);
        let (code, error) = match status {
            Ok(s) => (s.code(), None),
            Err(e) => (None, Some(e.to_string())),
        };

        let (success, has_changes) = match kind {
            RunKind::Init => (code == Some(0), None),
            // -detailed-exitcode: 0 = no changes, 1 = error, 2 = changes present.
            RunKind::Plan { file } => {
                let has_changes = code == Some(2) && !cancelled;
                if has_changes {
                    runs.plans.lock().unwrap().insert(run_id, SavedPlan { dir, file });
                } else {
                    let _ = std::fs::remove_file(file);
                }
                (matches!(code, Some(0 | 2)), Some(has_changes))
            }
            // A plan file is single use: after an apply attempt it's stale either way.
            RunKind::Apply { file } => {
                let _ = std::fs::remove_file(file);
                (code == Some(0), None)
            }
        };

        let _ = app.emit(
            "tofu://exit",
            ExitEvent { run_id, code, success: success && !cancelled, cancelled, has_changes, error },
        );
    });

    Ok(())
}

/// A run's complete environment plus notes on where it came from.
struct ResolvedEnv {
    vars: EnvMap,
    notes: Vec<String>,
}

/// The environment tofu runs with in `dir`, lowest to highest precedence:
/// app process env, login-shell env, then each `.gestalt.env` from the
/// workspace root down to `dir`. Also returns human-readable notes describing
/// which files applied (variable names only, never values).
async fn effective_env(app: &AppHandle, dir: &Path) -> Result<ResolvedEnv, String> {
    let shell = app.state::<ShellEnv>().get().await;
    let mut env: EnvMap = std::env::vars_os().collect();
    env.extend(shell.iter().map(|(k, v)| (k.clone(), v.clone())));

    // Canonicalize both so symlinked paths (e.g. /tmp -> /private/tmp) compare.
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let root = app.state::<Workspace>().root.lock().unwrap().as_deref().map(canon);
    let dir = canon(dir);

    let mut notes = Vec::new();
    for file in env_file::chain(root.as_deref(), &dir) {
        let shown = root
            .as_deref()
            .and_then(|r| file.strip_prefix(r).ok())
            .unwrap_or(&file)
            .display()
            .to_string();
        let content = std::fs::read_to_string(&file).map_err(|e| format!("{shown}: {e}"))?;
        let effect = env_file::apply(&content, &mut env).map_err(|e| format!("{shown}: {e}"))?;
        let mut parts = Vec::new();
        if !effect.set.is_empty() {
            parts.push(format!("sets {}", effect.set.join(", ")));
        }
        if !effect.unset.is_empty() {
            parts.push(format!("unsets {}", effect.unset.join(", ")));
        }
        if parts.is_empty() {
            parts.push("no variables".into());
        }
        notes.push(format!("[gestalt] {shown}: {}", parts.join("; ")));
    }
    Ok(ResolvedEnv { vars: env, notes })
}

fn next_id(runs: &Runs) -> u64 {
    runs.next_id.fetch_add(1, Ordering::Relaxed) + 1
}

/// Run `tofu init` or `tofu plan` in `dir`. Plans are saved for a later apply.
#[tauri::command]
pub async fn run_tofu(
    app: AppHandle,
    runs: State<'_, Runs>,
    dir: String,
    action: Action,
) -> Result<u64, String> {
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let env = effective_env(&app, &dir).await?;
    let run_id = next_id(&runs);
    let mut args: Vec<OsString> = Vec::new();
    let kind = match action {
        Action::Init => {
            args.extend(["init", "-input=false", "-no-color"].map(OsString::from));
            RunKind::Init
        }
        Action::Plan => {
            let file = plans_dir(&app)?.join(format!("{run_id}.tfplan"));
            args.extend(["plan", "-input=false", "-no-color", "-detailed-exitcode"].map(OsString::from));
            let mut out = OsString::from("-out=");
            out.push(&file);
            args.push(out);
            RunKind::Plan { file }
        }
    };
    spawn_run(&app, &runs, env, run_id, dir, args, kind)?;
    Ok(run_id)
}

/// Apply the saved plan produced by run `plan_run_id`. Returns the new run id.
#[tauri::command]
pub async fn apply_plan(
    app: AppHandle,
    runs: State<'_, Runs>,
    plan_run_id: u64,
) -> Result<u64, String> {
    let dir = runs
        .plans
        .lock()
        .unwrap()
        .get(&plan_run_id)
        .map(|p| p.dir.clone())
        .ok_or("That plan is no longer available. Run a new plan.")?;
    // Re-read .gestalt.env so apply gets current (e.g. refreshed) credentials.
    let env = effective_env(&app, &dir).await?;
    // Take the plan out before spawning so it can't be applied twice.
    let plan = runs
        .plans
        .lock()
        .unwrap()
        .remove(&plan_run_id)
        .ok_or("That plan is no longer available. Run a new plan.")?;

    let run_id = next_id(&runs);
    let args = vec!["apply".into(), "-input=false".into(), "-no-color".into(), plan.file.clone().into_os_string()];
    let kind = RunKind::Apply { file: plan.file.clone() };
    if let Err(e) = spawn_run(&app, &runs, env, run_id, plan.dir.clone(), args, kind) {
        runs.plans.lock().unwrap().insert(plan_run_id, plan);
        return Err(e);
    }
    Ok(run_id)
}

/// Drop a saved plan without applying it (e.g. its tab was closed).
#[tauri::command]
pub fn discard_plan(runs: State<'_, Runs>, plan_run_id: u64) {
    if let Some(plan) = runs.plans.lock().unwrap().remove(&plan_run_id) {
        let _ = std::fs::remove_file(plan.file);
    }
}

/// First call sends SIGINT so tofu can stop gracefully and release its state
/// lock; a second call (or any call on Windows) kills the process outright.
#[tauri::command]
pub fn cancel_tofu(runs: State<'_, Runs>, run_id: u64) -> Result<(), String> {
    let mut active = runs.active.lock().unwrap();
    let run = active.get_mut(&run_id).ok_or("Run is not active")?;

    #[cfg(unix)]
    if !run.interrupted
        && let Some(pid) = run.pid
    {
        run.interrupted = true;
        // SAFETY: plain signal send to a child pid we spawned.
        unsafe { libc::kill(pid as i32, libc::SIGINT) };
        return Ok(());
    }

    run.interrupted = true;
    if let Some(kill) = run.kill.take() {
        let _ = kill.send(());
    }
    Ok(())
}

#[tauri::command]
pub async fn tofu_version(shell_env: State<'_, ShellEnv>) -> Result<String, String> {
    let env = shell_env.get().await;
    let out = Command::new(tofu_bin(&env)?)
        .arg("version")
        .envs(env.iter())
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().to_string())
}
