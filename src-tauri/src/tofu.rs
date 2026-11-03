//! Running OpenTofu and streaming its output to the UI.
//!
//! A run is one or more tofu invocations ("steps") in a folder: `tofu init`
//! always runs first (it's idempotent, and a stale `.terraform` — new modules,
//! providers or backend settings — would otherwise fail the command), then the
//! requested command. Everything — including failures to start — is reported through
//! events, so it all lands in the run's output pane.
//!
//! Applies only ever run against a saved plan file produced by a previous
//! plan run, so what gets applied is exactly what the user reviewed. An apply
//! continues its plan's run: it reports under the plan's run id, so init, plan
//! and apply all land in one output pane (like `make apply`'s single stream).
//!
//! Events emitted:
//! - `tofu://output` — one per output line ([`OutputEvent`])
//! - `tofu://exit`   — once when the run ends ([`ExitEvent`])

use crate::env_file;
use crate::module_info::{self, ModuleInfo};
use crate::shell_env::{EnvMap, ShellEnv};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::oneshot;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputEvent {
    run_id: u64,
    /// "stdout" | "stderr" | "gestalt" (the app's own notes and command echoes)
    stream: &'static str,
    line: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitEvent {
    run_id: u64,
    /// Exit code of the last step that ran; `None` if killed or never started.
    code: Option<i32>,
    success: bool,
    cancelled: bool,
    /// Plan runs only: whether a saved plan with changes is ready to apply.
    has_changes: Option<bool>,
    error: Option<String>,
    /// Whether the requested command itself ran (vs. failing in setup or a
    /// preceding init). An apply that never started keeps its plan.
    started: bool,
}

/// What a run is doing, which decides how its exit is handled.
enum RunKind {
    Plan { file: PathBuf },
    Apply { plan: SavedPlan },
    Import,
}

impl RunKind {
    /// Whether to `tofu init` first. An apply continues its plan's run, which
    /// already initialised the folder; re-initialising could change the
    /// providers the saved plan was made with.
    fn auto_init(&self) -> bool {
        !matches!(self, RunKind::Apply { .. })
    }
}

/// One tofu invocation, plus how to echo it (secrets masked).
struct Step {
    args: Vec<OsString>,
    shown: Vec<String>,
}

impl Step {
    fn new(args: &[&str]) -> Self {
        Step { args: args.iter().map(OsString::from).collect(), shown: args.iter().map(|s| s.to_string()).collect() }
    }

    fn arg(mut self, arg: impl Into<OsString>, shown: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self.shown.push(shown.into());
        self
    }

    fn plain(self, arg: impl Into<String>) -> Self {
        let arg = arg.into();
        self.arg(arg.clone(), arg)
    }
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

/// Look up a variable in the resolved env, falling back to our own process env.
fn env_var(env: &EnvMap, key: &str) -> Option<OsString> {
    env.get(OsStr::new(key)).cloned().or_else(|| std::env::var_os(key))
}

/// Find the `tofu` binary on the resolved PATH, then common install locations.
/// `GESTALT_TOFU_BIN` overrides.
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

/// A run's complete environment plus notes on where it came from.
struct ResolvedEnv {
    vars: EnvMap,
    notes: Vec<String>,
}

/// The environment tofu runs with in `dir`, lowest to highest precedence:
/// app process env, login-shell env, then each `.gestalt.env` from the
/// workspace root down to `dir`. Notes name the files and variables applied
/// (names only, never values).
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
        notes.push(format!("{shown}: {}", parts.join("; ")));
    }
    Ok(ResolvedEnv { vars: env, notes })
}

fn emit_line(app: &AppHandle, run_id: u64, stream: &'static str, line: impl Into<String>) {
    let _ = app.emit("tofu://output", OutputEvent { run_id, stream, line: line.into() });
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
                    emit_line(&app, run_id, stream, line);
                }
            }
        }
    })
}

fn next_id(runs: &Runs) -> u64 {
    runs.next_id.fetch_add(1, Ordering::Relaxed) + 1
}

fn interrupted(app: &AppHandle, run_id: u64) -> bool {
    app.state::<Runs>().active.lock().unwrap().get(&run_id).is_some_and(|r| r.interrupted)
}

/// Run one step to completion, wiring it up for cancellation.
async fn run_step(
    app: &AppHandle,
    run_id: u64,
    bin: &Path,
    dir: &Path,
    env: &EnvMap,
    step: &Step,
) -> Result<Option<i32>, String> {
    let mut child = Command::new(bin)
        .args(&step.args)
        .current_dir(dir)
        // `env` is the complete environment, so `unset` in .gestalt.env works.
        .env_clear()
        .envs(env)
        .env("TF_IN_AUTOMATION", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Failed to start tofu: {e}"))?;

    let (kill_tx, kill_rx) = oneshot::channel();
    if let Some(run) = app.state::<Runs>().active.lock().unwrap().get_mut(&run_id) {
        run.pid = child.id();
        run.kill = Some(kill_tx);
    }

    let out = pump(app.clone(), run_id, "stdout", child.stdout.take().unwrap());
    let err = pump(app.clone(), run_id, "stderr", child.stderr.take().unwrap());
    let status = tokio::select! {
        s = child.wait() => s,
        _ = kill_rx => {
            let _ = child.start_kill();
            child.wait().await
        }
    };
    let _ = out.await;
    let _ = err.await;
    status.map(|s| s.code()).map_err(|e| e.to_string())
}

struct Outcome {
    code: Option<i32>,
    error: Option<String>,
    /// Whether the requested command (not just a preceding init) ran.
    main_ran: bool,
}

impl Outcome {
    fn failed(error: String) -> Self {
        Outcome { code: None, error: Some(error), main_ran: false }
    }
}

async fn execute_steps(app: &AppHandle, run_id: u64, dir: &Path, kind: &RunKind, main: Step) -> Outcome {
    {
        let runs = app.state::<Runs>();
        let mut active = runs.active.lock().unwrap();
        // Two runs in one folder would just fight over the state lock.
        if active.values().any(|r| r.dir == dir) {
            return Outcome::failed(format!("A tofu command is already running in {}", dir.display()));
        }
        active.insert(run_id, ActiveRun { dir: dir.to_path_buf(), pid: None, interrupted: false, kill: None });
    }

    let env = match effective_env(app, dir).await {
        Ok(env) => env,
        Err(e) => return Outcome::failed(e),
    };
    for note in &env.notes {
        emit_line(app, run_id, "gestalt", format!("env: {note}"));
    }
    let bin = match tofu_bin(&env.vars) {
        Ok(b) => b,
        Err(e) => return Outcome::failed(e),
    };

    let mut steps = Vec::new();
    if kind.auto_init() {
        steps.push(Step::new(&["init", "-input=false", "-no-color"]));
    }
    steps.push(main);
    let last = steps.len() - 1;

    for (i, step) in steps.iter().enumerate() {
        if interrupted(app, run_id) {
            return Outcome { code: None, error: None, main_ran: false };
        }
        emit_line(app, run_id, "gestalt", format!("$ tofu {}", step.shown.join(" ")));
        let result = run_step(app, run_id, &bin, dir, &env.vars, step).await;
        let (code, error) = match result {
            Ok(c) => (c, None),
            Err(e) => (None, Some(e)),
        };
        if i == last {
            return Outcome { code, error, main_ran: true };
        }
        if code != Some(0) || error.is_some() {
            return Outcome { code, error, main_ran: false };
        }
        emit_line(app, run_id, "gestalt", "");
    }
    unreachable!("steps always has a main step")
}

/// Run `main` (after an init, except for applies) in `dir`, then report the exit.
async fn execute(app: AppHandle, run_id: u64, dir: PathBuf, kind: RunKind, main: Step) {
    let outcome = execute_steps(&app, run_id, &dir, &kind, main).await;

    let runs = app.state::<Runs>();
    let cancelled = runs.active.lock().unwrap().remove(&run_id).is_some_and(|r| r.interrupted);
    let Outcome { code, error, main_ran } = outcome;
    let ok = !cancelled && error.is_none();

    let (success, has_changes) = match kind {
        // -detailed-exitcode: 0 = no changes, 1 = error, 2 = changes present.
        RunKind::Plan { file } => {
            let has_changes = ok && main_ran && code == Some(2);
            if has_changes {
                runs.plans.lock().unwrap().insert(run_id, SavedPlan { dir, file });
            } else {
                let _ = std::fs::remove_file(file);
            }
            (main_ran && matches!(code, Some(0 | 2)), Some(has_changes))
        }
        RunKind::Apply { plan } => {
            if main_ran {
                // A plan file is single use: after an apply attempt it's stale.
                let _ = std::fs::remove_file(&plan.file);
            } else {
                // Never got to run (folder busy, env error...): keep it applicable.
                runs.plans.lock().unwrap().insert(run_id, plan);
            }
            (main_ran && code == Some(0), None)
        }
        RunKind::Import => (main_ran && code == Some(0), None),
    };

    let _ = app.emit(
        "tofu://exit",
        ExitEvent { run_id, code, success: success && ok, cancelled, has_changes, error, started: main_ran },
    );
}

fn require_dir(dir: &str) -> Result<PathBuf, String> {
    let dir = PathBuf::from(dir);
    if dir.is_dir() { Ok(dir) } else { Err(format!("{} is not a directory", dir.display())) }
}

/// Report a run that couldn't even be set up, in its output pane.
fn fail_run(app: &AppHandle, run_id: u64, error: String) {
    let _ = app.emit(
        "tofu://exit",
        ExitEvent {
            run_id,
            code: None,
            success: false,
            cancelled: false,
            has_changes: None,
            error: Some(error),
            started: false,
        },
    );
}

/// `tofu plan` in `dir`, saving the plan for a later apply. Returns the run id;
/// progress and any failure arrive as events.
#[tauri::command]
pub fn run_plan(app: AppHandle, runs: State<'_, Runs>, dir: String) -> u64 {
    let run_id = next_id(&runs);
    let setup = require_dir(&dir).and_then(|dir| Ok((dir, plans_dir(&app)?.join(format!("{run_id}.tfplan")))));
    match setup {
        Ok((dir, file)) => {
            let mut out = OsString::from("-out=");
            out.push(&file);
            let step =
                Step::new(&["plan", "-input=false", "-no-color", "-detailed-exitcode"]).arg(out, "-out=<saved plan>");
            tauri::async_runtime::spawn(execute(app, run_id, dir, RunKind::Plan { file }, step));
        }
        Err(e) => fail_run(&app, run_id, e),
    }
    run_id
}

/// Apply the saved plan produced by run `plan_run_id`, continuing that run:
/// output and the exit event arrive under `plan_run_id`, in the same pane.
#[tauri::command]
pub fn apply_plan(app: AppHandle, runs: State<'_, Runs>, plan_run_id: u64) {
    // Take the plan out now so it can't be applied twice.
    match runs.plans.lock().unwrap().remove(&plan_run_id) {
        Some(plan) => {
            emit_line(&app, plan_run_id, "gestalt", "");
            emit_line(&app, plan_run_id, "gestalt", "Applying the reviewed plan.");
            let step =
                Step::new(&["apply", "-input=false", "-no-color"]).arg(plan.file.clone(), "<saved plan from above>");
            let dir = plan.dir.clone();
            let kind = RunKind::Apply { plan };
            tauri::async_runtime::spawn(execute(app, plan_run_id, dir, kind, step));
        }
        None => fail_run(&app, plan_run_id, "That plan is no longer available. Run a new plan.".into()),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VarValue {
    name: String,
    value: String,
}

/// Everything `tofu import` accepts. Blank/absent fields are omitted.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {
    address: String,
    id: String,
    #[serde(default)]
    vars: Vec<VarValue>,
    var_file: Option<String>,
    config: Option<String>,
    lock: Option<bool>,
    lock_timeout: Option<String>,
    parallelism: Option<u32>,
    #[serde(default)]
    ignore_remote_version: bool,
}

fn non_blank(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

fn import_step(req: &ImportRequest) -> Result<Step, String> {
    let address = req.address.trim();
    let id = req.id.trim();
    if address.is_empty() {
        return Err("Import needs a resource address.".into());
    }
    if id.is_empty() {
        return Err("Import needs the ID of the existing resource.".into());
    }

    let mut step = Step::new(&["import", "-input=false", "-no-color"]);
    if let Some(f) = non_blank(&req.var_file) {
        step = step.plain(format!("-var-file={f}"));
    }
    if let Some(c) = non_blank(&req.config) {
        step = step.plain(format!("-config={c}"));
    }
    if let Some(lock) = req.lock {
        step = step.plain(format!("-lock={lock}"));
    }
    if let Some(t) = non_blank(&req.lock_timeout) {
        step = step.plain(format!("-lock-timeout={t}"));
    }
    if let Some(p) = req.parallelism {
        step = step.plain(format!("-parallelism={p}"));
    }
    if req.ignore_remote_version {
        step = step.plain("-ignore-remote-version");
    }
    for v in &req.vars {
        let name = v.name.trim();
        if name.is_empty() || v.value.is_empty() {
            continue;
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err(format!("Invalid variable name `{name}`"));
        }
        // Values can be secrets: echo the name only.
        step = step.plain("-var").arg(format!("{name}={}", v.value), format!("{name}=…"));
    }
    Ok(step.plain(address).plain(id))
}

/// `tofu import` in `dir`. Returns the run id; progress arrives as events.
#[tauri::command]
pub fn run_import(app: AppHandle, runs: State<'_, Runs>, dir: String, request: ImportRequest) -> u64 {
    let run_id = next_id(&runs);
    match require_dir(&dir).and_then(|dir| Ok((dir, import_step(&request)?))) {
        Ok((dir, step)) => {
            tauri::async_runtime::spawn(execute(app, run_id, dir, RunKind::Import, step));
        }
        Err(e) => fail_run(&app, run_id, e),
    }
    run_id
}

/// Variables, resources and var files of the module in `dir`, for the import form.
#[tauri::command]
pub async fn module_info(app: AppHandle, dir: String) -> Result<ModuleInfo, String> {
    let dir = require_dir(&dir)?;
    // A broken .gestalt.env shouldn't block the form; it'll be reported on run.
    let env = effective_env(&app, &dir).await.map(|e| e.vars).unwrap_or_default();
    module_info::read(&dir, &env)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn req(json: serde_json::Value) -> ImportRequest {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn import_minimal() {
        let step = import_step(&req(serde_json::json!({"address": " aws_s3_bucket.b ", "id": "my-bucket"}))).unwrap();
        assert_eq!(step.shown, ["import", "-input=false", "-no-color", "aws_s3_bucket.b", "my-bucket"]);
    }

    #[test]
    fn import_all_options_and_masked_vars() {
        let step = import_step(&req(serde_json::json!({
            "address": "aws_s3_bucket.b",
            "id": "my-bucket",
            "vars": [{"name": "region", "value": "us-east-1"}, {"name": "unused", "value": ""}],
            "varFile": "prod.tfvars",
            "config": "  ",
            "lock": false,
            "lockTimeout": "30s",
            "parallelism": 4,
            "ignoreRemoteVersion": true
        })))
        .unwrap();
        assert_eq!(
            step.shown,
            [
                "import",
                "-input=false",
                "-no-color",
                "-var-file=prod.tfvars",
                "-lock=false",
                "-lock-timeout=30s",
                "-parallelism=4",
                "-ignore-remote-version",
                "-var",
                "region=…",
                "aws_s3_bucket.b",
                "my-bucket"
            ]
        );
        // The real args carry the value.
        assert!(step.args.contains(&OsString::from("region=us-east-1")));
    }

    #[test]
    fn import_requires_address_and_id() {
        assert!(import_step(&req(serde_json::json!({"address": "", "id": "x"}))).is_err());
        assert!(import_step(&req(serde_json::json!({"address": "a.b", "id": " "}))).is_err());
        assert!(
            import_step(&req(serde_json::json!({"address": "a.b", "id": "x", "vars": [{"name": "a b", "value": "1"}]})))
                .is_err()
        );
    }
}
