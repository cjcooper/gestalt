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
//! Events, sent only to the window that started the run:
//! - `tofu://output` — one per output line ([`OutputEvent`])
//! - `tofu://exit`   — once when the run ends ([`ExitEvent`])

use crate::cloud::{self, CloudSettings};
use crate::module_info::{self, ModuleInfo};
use crate::shell_env::{EnvMap, ShellEnv};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Emitter, Manager, Runtime, State, Webview};
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
    /// Label of the window that started it.
    window: String,
    pid: Option<u32>,
    interrupted: bool,
    kill: Option<oneshot::Sender<()>>,
}

struct SavedPlan {
    dir: PathBuf,
    window: String,
    file: PathBuf,
}

#[derive(Default)]
pub struct Runs {
    next_id: AtomicU64,
    active: Mutex<HashMap<u64, ActiveRun>>,
    /// Plans with pending changes, keyed by the run id that produced them.
    plans: Mutex<HashMap<u64, SavedPlan>>,
}

impl Runs {
    /// Folders tofu is running in right now.
    pub fn active_dirs(&self) -> Vec<PathBuf> {
        self.active.lock().unwrap().values().map(|r| r.dir.clone()).collect()
    }
}

/// Look up a variable in the resolved env, falling back to our own process env.
fn env_var(env: &EnvMap, key: &str) -> Option<OsString> {
    env.get(OsStr::new(key)).cloned().or_else(|| std::env::var_os(key))
}

/// Common install locations, searched after PATH. None in tests, so they
/// behave the same whether or not tofu is installed.
#[cfg(not(test))]
const FALLBACK_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];
#[cfg(test)]
const FALLBACK_DIRS: &[&str] = &[];

/// Find the `tofu` binary on the resolved PATH, then common install locations.
/// `GESTALT_TOFU_BIN` overrides.
fn tofu_bin(env: &EnvMap) -> Result<PathBuf, String> {
    if let Some(p) = env_var(env, "GESTALT_TOFU_BIN") {
        return Ok(PathBuf::from(p));
    }
    let exe = format!("tofu{}", std::env::consts::EXE_SUFFIX);
    let from_path = env_var(env, "PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    from_path
        .into_iter()
        .chain(FALLBACK_DIRS.iter().map(PathBuf::from))
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
        .ok_or_else(|| "Could not find `tofu` on PATH. Set GESTALT_TOFU_BIN to its location.".into())
}

/// Saved plan files live in the app cache, never in the user's repo: they can
/// contain secrets and must not be committed.
fn plans_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app.path().app_cache_dir().map_err(|e| e.to_string())?.join("plans");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Remove plan files left over from a previous session.
pub fn clear_stale_plans<R: Runtime>(app: &AppHandle<R>) {
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

/// Former per-folder env file. No longer read: refuse to run rather than
/// silently drop an `AWS_PROFILE` someone relied on.
const LEGACY_ENV_FILE: &str = ".gestalt.env";

fn legacy_env_file(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().map(|d| d.join(LEGACY_ENV_FILE)).find(|f| f.exists())
}

/// The environment tofu runs with in `dir`, lowest to highest precedence:
/// app process env, login-shell env, then the folder's cloud settings (see
/// [`cloud`]). Notes say which settings applied.
async fn effective_env<R: Runtime>(app: &AppHandle<R>, dir: &Path) -> Result<ResolvedEnv, String> {
    let shell = app.state::<ShellEnv>().get().await;
    let mut env: EnvMap = std::env::vars_os().collect();
    env.extend(shell.iter().map(|(k, v)| (k.clone(), v.clone())));

    if let Some(file) = legacy_env_file(dir) {
        return Err(format!(
            "{} is no longer read. Choose the cloud account with Cloud Settings… (right-click the folder), then delete the file.",
            file.display()
        ));
    }
    let resolved = app.state::<CloudSettings>().resolve(dir);
    let notes = cloud::apply(&resolved, &mut env);
    Ok(ResolvedEnv { vars: env, notes })
}

/// Where a run's events go: the window that started it.
struct Sink<R: Runtime> {
    app: AppHandle<R>,
    window: String,
    run_id: u64,
}

impl<R: Runtime> Clone for Sink<R> {
    fn clone(&self) -> Self {
        Sink { app: self.app.clone(), window: self.window.clone(), run_id: self.run_id }
    }
}

impl<R: Runtime> Sink<R> {
    fn new(webview: &Webview<R>, run_id: u64) -> Self {
        Sink { app: webview.app_handle().clone(), window: webview.label().to_string(), run_id }
    }

    fn line(&self, stream: &'static str, line: impl Into<String>) {
        let ev = OutputEvent { run_id: self.run_id, stream, line: line.into() };
        let _ = self.app.emit_to(self.window.as_str(), "tofu://output", ev);
    }

    fn exit(&self, ev: ExitEvent) {
        let _ = self.app.emit_to(self.window.as_str(), "tofu://exit", ev);
    }
}

fn pump<R: Runtime, T: AsyncRead + Unpin + Send + 'static>(
    sink: Sink<R>,
    stream: &'static str,
    reader: T,
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
                    sink.line(stream, line);
                }
            }
        }
    })
}

fn next_id(runs: &Runs) -> u64 {
    runs.next_id.fetch_add(1, Ordering::Relaxed) + 1
}

fn interrupted<R: Runtime>(sink: &Sink<R>) -> bool {
    sink.app.state::<Runs>().active.lock().unwrap().get(&sink.run_id).is_some_and(|r| r.interrupted)
}

/// Run one step to completion, wiring it up for cancellation.
async fn run_step<R: Runtime>(
    sink: &Sink<R>,
    bin: &Path,
    dir: &Path,
    env: &EnvMap,
    step: &Step,
) -> Result<Option<i32>, String> {
    let mut child = Command::new(bin)
        .args(&step.args)
        .current_dir(dir)
        // `env` is the complete environment, so variables cloud settings removed stay removed.
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
    if let Some(run) = sink.app.state::<Runs>().active.lock().unwrap().get_mut(&sink.run_id) {
        run.pid = child.id();
        run.kill = Some(kill_tx);
    }

    let out = pump(sink.clone(), "stdout", child.stdout.take().unwrap());
    let err = pump(sink.clone(), "stderr", child.stderr.take().unwrap());
    let status = tokio::select! {
        s = child.wait() => s,
        _ = kill_rx => {
            let _ = child.start_kill();
            child.wait().await
        }
    };
    let _ = out.await;
    let _ = err.await;
    status.as_ref().map(ExitStatus::code).map_err(ToString::to_string)
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

    /// Cancelled before the next step started.
    fn cancelled() -> Self {
        Outcome { code: None, error: None, main_ran: false }
    }
}

async fn execute_steps<R: Runtime>(sink: &Sink<R>, dir: &Path, kind: &RunKind, main: Step) -> Outcome {
    let (app, run_id) = (&sink.app, sink.run_id);
    {
        let runs = app.state::<Runs>();
        let mut active = runs.active.lock().unwrap();
        // Two runs in one folder would just fight over the state lock.
        if active.values().any(|r| r.dir == dir) {
            return Outcome::failed(format!("A tofu command is already running in {}", dir.display()));
        }
        let window = sink.window.clone();
        active.insert(run_id, ActiveRun { dir: dir.to_path_buf(), window, pid: None, interrupted: false, kill: None });
    }

    let env = match effective_env(app, dir).await {
        Ok(env) => env,
        Err(e) => return Outcome::failed(e),
    };
    for note in &env.notes {
        sink.line("gestalt", note);
    }
    let bin = match tofu_bin(&env.vars) {
        Ok(b) => b,
        Err(e) => return Outcome::failed(e),
    };

    if kind.auto_init() {
        let init = Step::new(&["init", "-input=false"]);
        let Some((code, error)) = run_echoed(sink, &bin, dir, &env.vars, &init).await else {
            return Outcome::cancelled();
        };
        if code != Some(0) || error.is_some() {
            return Outcome { code, error, main_ran: false };
        }
        sink.line("gestalt", "");
    }
    let Some((code, error)) = run_echoed(sink, &bin, dir, &env.vars, &main).await else {
        return Outcome::cancelled();
    };
    Outcome { code, error, main_ran: true }
}

/// Echo and run one step: its exit code, or why it couldn't run. `None` if
/// the run was cancelled before the step started.
async fn run_echoed<R: Runtime>(
    sink: &Sink<R>,
    bin: &Path,
    dir: &Path,
    env: &EnvMap,
    step: &Step,
) -> Option<(Option<i32>, Option<String>)> {
    if interrupted(sink) {
        return None;
    }
    sink.line("gestalt", format!("$ tofu {}", step.shown.join(" ")));
    Some(match run_step(sink, bin, dir, env, step).await {
        Ok(code) => (code, None),
        Err(e) => (None, Some(e)),
    })
}

/// Run `main` (after an init, except for applies) in `dir`, then report the exit.
async fn execute<R: Runtime>(sink: Sink<R>, dir: PathBuf, kind: RunKind, main: Step) {
    let outcome = execute_steps(&sink, &dir, &kind, main).await;

    let run_id = sink.run_id;
    let runs = sink.app.state::<Runs>();
    let cancelled = runs.active.lock().unwrap().remove(&run_id).is_some_and(|r| r.interrupted);
    let Outcome { code, error, main_ran } = outcome;
    let ok = !cancelled && error.is_none();

    let (success, has_changes) = match kind {
        // -detailed-exitcode: 0 = no changes, 1 = error, 2 = changes present.
        RunKind::Plan { file } => {
            let has_changes = ok && main_ran && code == Some(2);
            if has_changes {
                runs.plans.lock().unwrap().insert(run_id, SavedPlan { dir, window: sink.window.clone(), file });
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

    sink.exit(ExitEvent { run_id, code, success: success && ok, cancelled, has_changes, error, started: main_ran });
}

fn require_dir(dir: &str) -> Result<PathBuf, String> {
    let dir = PathBuf::from(dir);
    if dir.is_dir() { Ok(dir) } else { Err(format!("{} is not a directory", dir.display())) }
}

/// Report a run that couldn't even be set up, in its output pane.
fn fail_run<R: Runtime>(sink: &Sink<R>, error: String) {
    sink.exit(ExitEvent {
        run_id: sink.run_id,
        code: None,
        success: false,
        cancelled: false,
        has_changes: None,
        error: Some(error),
        started: false,
    });
}

/// `tofu plan` in `dir`, saving the plan for a later apply. Returns the run id;
/// progress and any failure arrive as events.
#[tauri::command]
pub fn run_plan<R: Runtime>(webview: Webview<R>, runs: State<'_, Runs>, dir: String) -> u64 {
    let run_id = next_id(&runs);
    let sink = Sink::new(&webview, run_id);
    let setup = require_dir(&dir).and_then(|dir| Ok((dir, plans_dir(&sink.app)?.join(format!("{run_id}.tfplan")))));
    match setup {
        Ok((dir, file)) => {
            let mut out = OsString::from("-out=");
            out.push(&file);
            let step =
                Step::new(&["plan", "-input=false", "-detailed-exitcode"]).arg(out, "-out=<saved plan>");
            tauri::async_runtime::spawn(execute(sink, dir, RunKind::Plan { file }, step));
        }
        Err(e) => fail_run(&sink, e),
    }
    run_id
}

/// Apply the saved plan produced by run `plan_run_id`, continuing that run:
/// output and the exit event arrive under `plan_run_id`, in the same pane.
#[tauri::command]
pub fn apply_plan<R: Runtime>(webview: Webview<R>, runs: State<'_, Runs>, plan_run_id: u64) {
    let sink = Sink::new(&webview, plan_run_id);
    // Take the plan out now so it can't be applied twice.
    match runs.plans.lock().unwrap().remove(&plan_run_id) {
        Some(plan) => {
            sink.line("gestalt", "");
            sink.line("gestalt", "Applying the reviewed plan.");
            let step =
                Step::new(&["apply", "-input=false"]).arg(plan.file.clone(), "<saved plan from above>");
            let dir = plan.dir.clone();
            let kind = RunKind::Apply { plan };
            tauri::async_runtime::spawn(execute(sink, dir, kind, step));
        }
        None => fail_run(&sink, "That plan is no longer available. Run a new plan.".into()),
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

    let mut step = Step::new(&["import", "-input=false"]);
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
pub fn run_import<R: Runtime>(webview: Webview<R>, runs: State<'_, Runs>, dir: String, request: ImportRequest) -> u64 {
    let run_id = next_id(&runs);
    let sink = Sink::new(&webview, run_id);
    match require_dir(&dir).and_then(|dir| Ok((dir, import_step(&request)?))) {
        Ok((dir, step)) => {
            tauri::async_runtime::spawn(execute(sink, dir, RunKind::Import, step));
        }
        Err(e) => fail_run(&sink, e),
    }
    run_id
}

/// Variables, resources and var files of the module in `dir`, for the import form.
#[tauri::command]
pub async fn module_info<R: Runtime>(app: AppHandle<R>, dir: String) -> Result<ModuleInfo, String> {
    let dir = require_dir(&dir)?;
    // A leftover .gestalt.env shouldn't block the form; it'll be reported on run.
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
    interrupt(active.get_mut(&run_id).ok_or("Run is not active")?);
    Ok(())
}

fn interrupt(run: &mut ActiveRun) {
    #[cfg(unix)]
    if !run.interrupted
        && let Some(pid) = run.pid
    {
        run.interrupted = true;
        // SAFETY: plain signal send to a child pid we spawned.
        unsafe { libc::kill(pid as i32, libc::SIGINT) };
        return;
    }

    run.interrupted = true;
    if let Some(kill) = run.kill.take() {
        let _ = kill.send(());
    }
}

/// A window closed: stop its runs, as Cancel does, and drop its saved plans.
pub fn window_closed(runs: &Runs, window: &str) {
    for run in runs.active.lock().unwrap().values_mut().filter(|r| r.window == window) {
        interrupt(run);
    }
    runs.plans.lock().unwrap().retain(|_, plan| {
        let keep = plan.window != window;
        if !keep {
            let _ = std::fs::remove_file(&plan.file);
        }
        keep
    });
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
        assert_eq!(step.shown, ["import", "-input=false", "aws_s3_bucket.b", "my-bucket"]);
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

/// Runs end to end, against a fake `tofu` script.
#[cfg(all(test, unix))]
mod run_tests {
    use super::*;
    use crate::shell_env::testing::{fake_shell, ipc, script, temp_dir, webview};
    use crate::cloud::Settings;
    use serde_json::{Value, json};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use tauri::Listener;
    use tauri::async_runtime::block_on;
    use tauri::test::MockRuntime;
    use tauri::utils::config::AppDirectoriesOverride;

    /// Echoes what it was run with, then behaves per `FAKE_<command>`:
    /// `exit<N>`, or wait to be interrupted and then exit 130 (`graceful`),
    /// exit 0 (`finish`) or carry on (`stubborn`).
    const FAKE_TOFU: &str = r#"
if [ "$1" = version ]; then echo "OpenTofu v1.2.3"; echo "on test"; exit 0; fi
for a; do case "$a" in -out=*) : > "${a#-out=}";; esac; done
echo "$1 in $(pwd -P) auto=$TF_IN_AUTOMATION foo=${FOO:-} args=$*"
echo "$1 says hi" >&2
eval "mode=\${FAKE_$1:-}"
case "$mode" in
  exit*) exit "${mode#exit}";;
  graceful) trap 'kill $! 2>/dev/null; echo interrupted; exit 130' INT;;
  finish) trap 'kill $! 2>/dev/null; exit 0' INT;;
  stubborn) trap 'kill $! 2>/dev/null; echo ignoring' INT;;
  *) exit 0;;
esac
: > ready
while :; do sleep 30 >/dev/null 2>&1 & wait; done
"#;

    struct Fx {
        app: tauri::App<MockRuntime>,
        /// The window runs are started from; their events go only there.
        view: tauri::WebviewWindow<MockRuntime>,
        /// Holds the fake tofu and shell; usable as a workspace root.
        base: PathBuf,
        /// The module folder runs happen in.
        dir: PathBuf,
        events: mpsc::Receiver<(&'static str, Value)>,
    }

    /// An app whose login shell points `GESTALT_TOFU_BIN` at the fake tofu,
    /// plus `vars` (later entries win).
    fn fixture(name: &str, vars: &[&str]) -> Fx {
        let base = temp_dir(&format!("tofu-{name}"));
        let tofu = script(&base.join("tofu"), FAKE_TOFU);
        let mut all = vec![format!("GESTALT_TOFU_BIN={}", tofu.display()), "PATH=/bin:/usr/bin".into()];
        all.extend(vars.iter().map(|v| v.to_string()));
        let all: Vec<&str> = all.iter().map(String::as_str).collect();
        let shell = ShellEnv::with_shell(fake_shell(&base, &all));
        let app_dir = base.join("app");
        fixture_with(base, shell, app_dir)
    }

    fn fixture_with(base: PathBuf, shell: ShellEnv, app_dir: PathBuf) -> Fx {
        let dir = base.join("mod");
        std::fs::create_dir_all(&dir).unwrap();
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        // Keep saved plans out of the real app cache.
        context.config_mut().app.app_directories_override = Some(AppDirectoriesOverride::Root(app_dir));
        let app = tauri::test::mock_builder()
            .manage(Runs::default())
            .manage(shell)
            .manage(CloudSettings::load(base.join("cloud.json")))
            .build(context)
            .unwrap();
        let view = webview(&app);
        let (tx, events) = mpsc::channel();
        for name in ["tofu://output", "tofu://exit"] {
            let tx = tx.clone();
            // Only what's sent to the run's own window.
            view.listen(name, move |e| {
                let _ = tx.send((name, serde_json::from_str(e.payload()).unwrap()));
            });
        }
        Fx { app, view, base, dir, events }
    }

    impl Fx {
        fn handle(&self) -> Webview<MockRuntime> {
            self.view.as_ref().clone()
        }

        fn dir_arg(&self) -> String {
            self.dir.to_string_lossy().into_owned()
        }

        fn runs(&self) -> State<'_, Runs> {
            self.app.state()
        }

        fn set_cloud(&self, dir: &Path, values: &[(&str, &str)]) {
            let values: Settings = values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            self.app.state::<CloudSettings>().set(dir, values).unwrap();
        }

        fn plan_file(&self, run_id: u64) -> PathBuf {
            self.app.path().app_cache_dir().unwrap().join("plans").join(format!("{run_id}.tfplan"))
        }

        /// Output lines (`stream: line`) up to the run's exit event, and that event.
        fn wait_exit(&self, run_id: u64) -> (Vec<String>, Value) {
            let mut lines = Vec::new();
            loop {
                let (name, ev) = self.events.recv_timeout(Duration::from_secs(20)).expect("no exit event");
                assert_eq!(ev["runId"], run_id);
                if name == "tofu://exit" {
                    return (lines, ev);
                }
                lines.push(format!("{}: {}", ev["stream"].as_str().unwrap(), ev["line"].as_str().unwrap()));
            }
        }

        /// Wait until a step that waits to be interrupted is running.
        fn wait_running(&self, run_id: u64) {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !(self.dir.join("ready").exists() && self.has_pid(run_id)) {
                assert!(Instant::now() < deadline, "step never started");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn has_pid(&self, run_id: u64) -> bool {
            self.runs().active.lock().unwrap().get(&run_id).is_some_and(|r| r.pid.is_some())
        }
    }

    fn req(json: Value) -> ImportRequest {
        serde_json::from_value(json).unwrap()
    }

    /// The app's own lines (notes and command echoes).
    fn gestalt(lines: &[String]) -> Vec<&str> {
        lines.iter().filter_map(|l| l.strip_prefix("gestalt: ")).collect()
    }

    fn exit(run_id: u64, code: Value, success: bool, has_changes: Value, started: bool) -> Value {
        json!({
            "runId": run_id, "code": code, "success": success, "cancelled": false,
            "hasChanges": has_changes, "error": null, "started": started,
        })
    }

    fn failed(run_id: u64, error: &str) -> Value {
        json!({
            "runId": run_id, "code": null, "success": false, "cancelled": false,
            "hasChanges": null, "error": error, "started": false,
        })
    }

    #[test]
    fn plan_with_changes_then_apply_it() {
        let fx = fixture("plan-apply", &["FAKE_plan=exit2"]);
        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        let (lines, ev) = fx.wait_exit(id);
        let plan = fx.plan_file(id);
        assert_eq!(
            gestalt(&lines),
            ["$ tofu init -input=false", "", "$ tofu plan -input=false -detailed-exitcode -out=<saved plan>"]
        );
        let dir = fx.dir.display();
        assert!(lines.contains(&format!("stdout: init in {dir} auto=1 foo= args=init -input=false")), "{lines:?}");
        assert!(lines.contains(&"stderr: init says hi".to_string()), "{lines:?}");
        let planned =
            format!("stdout: plan in {dir} auto=1 foo= args=plan -input=false -detailed-exitcode -out={}", plan.display());
        assert!(lines.contains(&planned), "{lines:?}");
        assert_eq!(ev, exit(id, json!(2), true, json!(true), true));
        assert!(plan.is_file());

        apply_plan(fx.handle(), fx.runs(), id);
        let (lines, ev) = fx.wait_exit(id);
        // Continues the plan's run, without another init.
        assert_eq!(gestalt(&lines), ["", "Applying the reviewed plan.", "$ tofu apply -input=false <saved plan from above>"]);
        let applied = format!("stdout: apply in {dir} auto=1 foo= args=apply -input=false {}", plan.display());
        assert!(lines.contains(&applied), "{lines:?}");
        assert_eq!(ev, exit(id, json!(0), true, Value::Null, true));
        // A plan is single use.
        assert!(!plan.exists());
        apply_plan(fx.handle(), fx.runs(), id);
        assert_eq!(fx.wait_exit(id).1, failed(id, "That plan is no longer available. Run a new plan."));
    }

    #[test]
    fn plan_without_changes_or_failing_keeps_nothing() {
        for (mode, code, success) in [("exit0", 0, true), ("exit1", 1, false)] {
            let fx = fixture(&format!("plan-{mode}"), &[&format!("FAKE_plan={mode}")]);
            let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
            assert_eq!(fx.wait_exit(id).1, exit(id, json!(code), success, json!(false), true));
            assert!(!fx.plan_file(id).exists());
            assert!(fx.runs().plans.lock().unwrap().is_empty());
            assert!(fx.runs().active_dirs().is_empty());
        }
    }

    #[test]
    fn failed_init_stops_the_run() {
        let fx = fixture("init-fails", &["FAKE_init=exit1"]);
        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        let (lines, ev) = fx.wait_exit(id);
        assert_eq!(gestalt(&lines), ["$ tofu init -input=false"]);
        assert_eq!(ev, exit(id, json!(1), false, json!(false), false));
    }

    #[test]
    fn runs_that_cannot_be_set_up() {
        let fx = fixture("setup-fails", &[]);
        let missing = fx.base.join("missing");
        let not_dir = format!("{} is not a directory", missing.display());
        let id = run_plan(fx.handle(), fx.runs(), missing.to_string_lossy().into());
        assert_eq!(fx.wait_exit(id).1, failed(id, &not_dir));

        let request = req(json!({"address": "a.b", "id": "x"}));
        let id = run_import(fx.handle(), fx.runs(), missing.to_string_lossy().into(), request);
        assert_eq!(fx.wait_exit(id).1, failed(id, &not_dir));

        let request = req(json!({"address": "a.b", "id": ""}));
        let id = run_import(fx.handle(), fx.runs(), fx.dir_arg(), request);
        assert_eq!(fx.wait_exit(id).1, failed(id, "Import needs the ID of the existing resource."));
        // Ids keep counting up.
        assert_eq!(id, 3);
    }

    #[test]
    fn plan_needs_a_plans_folder() {
        // An app cache dir that can't be resolved.
        let base = temp_dir("tofu-no-cache-dir");
        let fx = fixture_with(base.clone(), ShellEnv::preloaded(EnvMap::new()), "$NOPE/x".into());
        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        let ev = fx.wait_exit(id).1;
        assert!(ev["error"].as_str().unwrap().contains("$NOPE"), "{ev}");
        assert_eq!(ev["started"], false);
        clear_stale_plans(fx.app.handle()); // nothing to clear, no panic

        // One that can't be created (a file is in the way).
        let app_dir = base.join("app-file");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join("caches"), "").unwrap();
        let fx = fixture_with(base, ShellEnv::preloaded(EnvMap::new()), app_dir);
        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        let ev = fx.wait_exit(id).1;
        assert!(ev["error"].is_string(), "{ev}");
        assert_eq!(ev["started"], false);
    }

    #[test]
    fn clears_plans_from_a_previous_session() {
        let fx = fixture("clear-plans", &[]);
        let stale = fx.plan_file(7);
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(&stale, "old").unwrap();
        clear_stale_plans(fx.app.handle());
        assert!(!stale.exists());
        assert!(stale.parent().unwrap().is_dir());
    }

    #[test]
    fn import_masks_variable_values_in_the_echo() {
        let fx = fixture("import", &["FAKE_import=exit0"]);
        let request = req(json!({"address": "a.b", "id": "x", "vars": [{"name": "token", "value": "s3cret"}]}));
        let id = run_import(fx.handle(), fx.runs(), fx.dir_arg(), request);
        let (lines, ev) = fx.wait_exit(id);
        assert_eq!(gestalt(&lines), ["$ tofu init -input=false", "", "$ tofu import -input=false -var token=… a.b x"]);
        let dir = fx.dir.display();
        let imported = format!("stdout: import in {dir} auto=1 foo= args=import -input=false -var token=s3cret a.b x");
        assert!(lines.contains(&imported), "{lines:?}");
        assert_eq!(ev, exit(id, json!(0), true, Value::Null, true));
    }

    #[test]
    fn cloud_settings_apply_and_are_noted() {
        let fx = fixture("cloud", &["AWS_PROFILE=shell", "AWS_SESSION_TOKEN=t", "FOO=shell"]);
        fx.set_cloud(&fx.base, &[("aws.profile", "dev"), ("aws.region", "us-east-1")]);
        fx.set_cloud(&fx.dir, &[("aws.profile", "prd")]);
        let id = run_import(fx.handle(), fx.runs(), fx.dir_arg(), req(json!({"address": "a.b", "id": "x"})));
        let (lines, ev) = fx.wait_exit(id);
        let (base, dir) = (fx.base.display(), fx.dir.display());
        assert_eq!(
            gestalt(&lines)[..2],
            [
                format!("cloud: AWS profile prd (from {dir}; sets AWS_PROFILE; removes AWS_SESSION_TOKEN)"),
                format!("cloud: AWS region us-east-1 (from {base}; sets AWS_REGION, AWS_DEFAULT_REGION)"),
            ]
        );
        assert!(lines.iter().any(|l| l.starts_with("stdout: init") && l.contains(" foo=shell ")), "{lines:?}");
        assert_eq!(ev["success"], true);
    }

    #[test]
    fn leftover_env_files_fail_the_run() {
        let fx = fixture("legacy-env", &[]);
        let file = fx.base.join(".gestalt.env");
        std::fs::write(&file, "AWS_PROFILE=dev\n").unwrap();
        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        let ev = fx.wait_exit(id).1;
        let msg = format!(
            "{} is no longer read. Choose the cloud account with Cloud Settings… (right-click the folder), then delete the file.",
            file.display()
        );
        assert_eq!((&ev["error"], &ev["started"], &ev["success"]), (&json!(msg), &json!(false), &json!(false)));
    }

    /// Events for a run reach only the window that started it.
    #[test]
    fn events_go_to_the_starting_window() {
        let fx = fixture("windows", &[]);
        let other = tauri::WebviewWindowBuilder::new(&fx.app, "other", Default::default()).build().unwrap();
        let (tx, rx) = mpsc::channel::<()>();
        other.listen("tofu://exit", move |_| {
            let _ = tx.send(());
        });
        // The listener hears what is sent to its window...
        fx.app.emit_to("other", "tofu://exit", ()).unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
        // ...but not another window's run.
        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        assert_eq!(fx.wait_exit(id).1["success"], true);
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn reports_tofu_that_is_missing_or_will_not_start() {
        let empty = temp_dir("tofu-empty-path");
        let shell = ShellEnv::preloaded(EnvMap::from([("PATH".into(), empty.into())]));
        let base = temp_dir("tofu-no-tofu");
        let fx = fixture_with(base.clone(), shell, base.join("app"));
        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        let missing = "Could not find `tofu` on PATH. Set GESTALT_TOFU_BIN to its location.";
        assert_eq!(fx.wait_exit(id).1["error"], missing);

        let fx = fixture("bad-tofu", &["GESTALT_TOFU_BIN=/nonexistent/tofu"]);
        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        let ev = fx.wait_exit(id).1;
        assert!(ev["error"].as_str().unwrap().starts_with("Failed to start tofu: "), "{ev}");
        assert_eq!((&ev["code"], &ev["started"]), (&Value::Null, &json!(false)));
    }

    #[test]
    fn one_run_per_folder() {
        let fx = fixture("busy", &[]);
        let busy = ActiveRun { dir: fx.dir.clone(), window: "main".into(), pid: None, interrupted: false, kill: None };
        fx.runs().active.lock().unwrap().insert(999, busy);
        assert_eq!(fx.runs().active_dirs(), std::slice::from_ref(&fx.dir));
        let msg = format!("A tofu command is already running in {}", fx.dir.display());

        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        let ev = fx.wait_exit(id).1;
        assert_eq!((&ev["error"], &ev["started"], &ev["hasChanges"]), (&json!(msg), &json!(false), &json!(false)));

        // An apply that never started keeps its plan.
        let file = fx.base.join("saved.tfplan");
        std::fs::write(&file, "").unwrap();
        let plan = SavedPlan { dir: fx.dir.clone(), window: "main".into(), file: file.clone() };
        fx.runs().plans.lock().unwrap().insert(500, plan);
        apply_plan(fx.handle(), fx.runs(), 500);
        let (lines, ev) = fx.wait_exit(500);
        assert_eq!(gestalt(&lines), ["", "Applying the reviewed plan."]);
        assert_eq!(ev, failed(500, &msg));
        assert!(file.exists());
        assert!(fx.runs().plans.lock().unwrap().contains_key(&500));

        discard_plan(fx.runs(), 500);
        assert!(!file.exists());
        assert!(fx.runs().plans.lock().unwrap().is_empty());
        discard_plan(fx.runs(), 500); // already gone: no-op
    }

    #[test]
    fn cancel_interrupts_gracefully() {
        let fx = fixture("cancel-graceful", &["FAKE_plan=graceful"]);
        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        fx.wait_running(id);
        cancel_tofu(fx.runs(), id).unwrap();
        let (lines, ev) = fx.wait_exit(id);
        assert!(lines.contains(&"stdout: interrupted".to_string()), "{lines:?}");
        let expected = json!({
            "runId": id, "code": 130, "success": false, "cancelled": true,
            "hasChanges": false, "error": null, "started": true,
        });
        assert_eq!(ev, expected);
        assert!(!fx.plan_file(id).exists());
        assert_eq!(cancel_tofu(fx.runs(), id), Err("Run is not active".into()));
    }

    #[test]
    fn second_cancel_kills() {
        let fx = fixture("cancel-kill", &["FAKE_plan=stubborn"]);
        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        fx.wait_running(id);
        cancel_tofu(fx.runs(), id).unwrap();
        cancel_tofu(fx.runs(), id).unwrap();
        let ev = fx.wait_exit(id).1;
        assert_eq!((&ev["cancelled"], &ev["code"], &ev["started"]), (&json!(true), &Value::Null, &json!(true)));
    }

    #[test]
    fn closing_a_window_cancels_its_runs_and_drops_its_plans() {
        let fx = fixture("window-closed", &["FAKE_plan=graceful"]);
        let saved = |n: u64, window: &str| {
            let file = fx.base.join(format!("{n}.tfplan"));
            std::fs::write(&file, "").unwrap();
            fx.runs().plans.lock().unwrap().insert(n, SavedPlan { dir: fx.dir.clone(), window: window.into(), file: file.clone() });
            file
        };
        let (mine, theirs) = (saved(800, "main"), saved(801, "other"));
        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        fx.wait_running(id);

        window_closed(&fx.runs(), "other");
        assert!(fx.has_pid(id) && !fx.runs().active.lock().unwrap()[&id].interrupted);
        assert!(!theirs.exists() && mine.exists());

        window_closed(&fx.runs(), "main");
        let ev = fx.wait_exit(id).1;
        assert_eq!((&ev["cancelled"], &ev["code"]), (&json!(true), &json!(130)));
        assert!(!mine.exists());
        assert!(fx.runs().plans.lock().unwrap().is_empty());
    }

    #[test]
    fn cancel_during_init_skips_the_command() {
        let fx = fixture("cancel-init", &["FAKE_init=finish"]);
        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        fx.wait_running(id);
        cancel_tofu(fx.runs(), id).unwrap();
        let (lines, ev) = fx.wait_exit(id);
        assert_eq!(gestalt(&lines), ["$ tofu init -input=false", ""]);
        let expected = json!({
            "runId": id, "code": null, "success": false, "cancelled": true,
            "hasChanges": false, "error": null, "started": false,
        });
        assert_eq!(ev, expected);
    }

    #[test]
    fn cancel_before_anything_ran() {
        let fx = fixture("cancel-early", &[]);
        // Hold the run up while it resolves its environment.
        let settings = fx.app.state::<CloudSettings>();
        let guard = settings.store.lock().unwrap();
        let id = run_plan(fx.handle(), fx.runs(), fx.dir_arg());
        let deadline = Instant::now() + Duration::from_secs(20);
        while !fx.runs().active.lock().unwrap().contains_key(&id) {
            assert!(Instant::now() < deadline, "run never became active");
            std::thread::sleep(Duration::from_millis(10));
        }
        cancel_tofu(fx.runs(), id).unwrap();
        drop(guard);
        let (lines, ev) = fx.wait_exit(id);
        assert!(gestalt(&lines).is_empty(), "{lines:?}");
        assert_eq!((&ev["cancelled"], &ev["started"], &ev["error"]), (&json!(true), &json!(false), &Value::Null));
    }

    #[test]
    fn module_info_reads_the_folder() {
        let fx = fixture("module-info", &[]);
        let missing = fx.base.join("missing");
        let err = block_on(module_info(fx.app.handle().clone(), missing.to_string_lossy().into())).unwrap_err();
        assert_eq!(err, format!("{} is not a directory", missing.display()));

        std::fs::write(fx.dir.join("main.tf"), "variable \"region\" {}\n").unwrap();
        let info = block_on(module_info(fx.app.handle().clone(), fx.dir_arg())).unwrap();
        assert_eq!(info.variables.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["region"]);
        // A leftover .gestalt.env doesn't block it.
        std::fs::write(fx.dir.join(".gestalt.env"), "broken\n").unwrap();
        assert_eq!(block_on(module_info(fx.app.handle().clone(), fx.dir_arg())).unwrap().variables.len(), 1);
    }

    #[test]
    fn version_is_the_first_line() {
        let fx = fixture("version", &[]);
        assert_eq!(block_on(tofu_version(fx.app.state())).unwrap(), "OpenTofu v1.2.3");

        let fx = fixture("version-bad", &["GESTALT_TOFU_BIN=/nonexistent/tofu"]);
        assert!(block_on(tofu_version(fx.app.state())).is_err());

        let empty = temp_dir("tofu-version-empty-path");
        let shell = ShellEnv::preloaded(EnvMap::from([("PATH".into(), empty.into())]));
        let fx = fixture_with(temp_dir("tofu-version-missing"), shell, "/unused".into());
        let missing = "Could not find `tofu` on PATH. Set GESTALT_TOFU_BIN to its location.";
        assert_eq!(block_on(tofu_version(fx.app.state())), Err(missing.into()));
    }

    #[test]
    fn finds_tofu_on_path() {
        let base = temp_dir("tofu-bin");
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        script(&b.join("tofu"), "");
        let path = std::env::join_paths([&a, &b]).unwrap();
        let env = EnvMap::from([("PATH".into(), path.clone())]);
        assert_eq!(tofu_bin(&env), Ok(b.join("tofu")));
        // The override wins without checking.
        let env = EnvMap::from([("PATH".into(), path), ("GESTALT_TOFU_BIN".into(), "/x/tofu".into())]);
        assert_eq!(tofu_bin(&env), Ok(PathBuf::from("/x/tofu")));
        let env = EnvMap::from([("PATH".into(), a.into())]);
        assert!(tofu_bin(&env).is_err());
    }

    #[test]
    fn async_commands_report_bad_invocations() {
        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![module_info, tofu_version])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let view = webview(&app);
        let err = ipc(&view, "module_info", json!({})).unwrap_err();
        assert!(err.as_str().unwrap().contains("missing required key dir"), "{err}");
        let err = ipc(&view, "tofu_version", json!({})).unwrap_err();
        assert!(err.as_str().unwrap().contains("state not managed"), "{err}");
    }

    #[test]
    fn import_config_option() {
        let step = import_step(&req(json!({"address": "a.b", "id": "x", "config": " ../mod "}))).unwrap();
        assert_eq!(step.shown, ["import", "-input=false", "-config=../mod", "a.b", "x"]);
    }

    #[test]
    fn env_var_falls_back_to_the_process_env() {
        let env = EnvMap::from([("HOME".into(), "/elsewhere".into())]);
        assert_eq!(env_var(&env, "HOME"), Some("/elsewhere".into()));
        assert_eq!(env_var(&EnvMap::new(), "PATH"), std::env::var_os("PATH"));
    }
}
