//! Spawning OpenTofu and streaming its output to the UI.
//!
//! Events emitted:
//! - `tofu://output` — one per line of stdout/stderr ([`OutputEvent`])
//! - `tofu://exit`   — once when the process ends ([`ExitEvent`])

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
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
    Apply,
}

impl Action {
    fn args(self) -> &'static [&'static str] {
        match self {
            Action::Init => &["init", "-input=false", "-no-color"],
            Action::Plan => &["plan", "-input=false", "-no-color"],
            // Without a TTY tofu can't prompt for approval, so the UI confirms
            // with the user before invoking this.
            Action::Apply => &["apply", "-input=false", "-no-color", "-auto-approve"],
        }
    }
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
    cancelled: bool,
    error: Option<String>,
}

struct ActiveRun {
    dir: PathBuf,
    pid: Option<u32>,
    interrupted: bool,
    kill: Option<oneshot::Sender<()>>,
}

#[derive(Default)]
pub struct Runs {
    next_id: AtomicU64,
    active: Mutex<HashMap<u64, ActiveRun>>,
}

/// Find the `tofu` binary. Apps launched from Finder/Dock get a minimal PATH,
/// so fall back to common install locations. `GESTALT_TOFU_BIN` overrides.
fn tofu_bin() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("GESTALT_TOFU_BIN") {
        return Ok(PathBuf::from(p));
    }
    let exe = if cfg!(windows) { "tofu.exe" } else { "tofu" };
    let from_path = std::env::var_os("PATH")
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

    let run_id = {
        let mut active = runs.active.lock().unwrap();
        // Two runs in one folder would just fight over the state lock.
        if active.values().any(|r| r.dir == dir) {
            return Err(format!("A tofu command is already running in {}", dir.display()));
        }
        let id = runs.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        active.insert(id, ActiveRun { dir: dir.clone(), pid: None, interrupted: false, kill: None });
        id
    };

    let spawned = tofu_bin().and_then(|bin| {
        Command::new(bin)
            .args(action.args())
            .current_dir(&dir)
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

    let out = pump(app.clone(), run_id, "stdout", child.stdout.take().unwrap());
    let err = pump(app.clone(), run_id, "stderr", child.stderr.take().unwrap());

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
        let _ = app.emit("tofu://exit", ExitEvent { run_id, code, cancelled, error });
    });

    Ok(run_id)
}

/// First call sends SIGINT so tofu can stop gracefully and release its state
/// lock; a second call (or any call on Windows) kills the process outright.
#[tauri::command]
pub fn cancel_tofu(runs: State<'_, Runs>, run_id: u64) -> Result<(), String> {
    let mut active = runs.active.lock().unwrap();
    let run = active.get_mut(&run_id).ok_or("Run is not active")?;

    #[cfg(unix)]
    if !run.interrupted {
        if let Some(pid) = run.pid {
            run.interrupted = true;
            // SAFETY: plain signal send to a child pid we spawned.
            unsafe { libc::kill(pid as i32, libc::SIGINT) };
            return Ok(());
        }
    }

    run.interrupted = true;
    if let Some(kill) = run.kill.take() {
        let _ = kill.send(());
    }
    Ok(())
}

#[tauri::command]
pub fn tofu_version() -> Result<String, String> {
    let out = std::process::Command::new(tofu_bin()?)
        .arg("version")
        .output()
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().to_string())
}
