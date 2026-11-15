//! Per-folder cloud accounts: which AWS profile, Google Cloud configuration
//! or Azure subscription tofu uses in a folder and every folder below it.
//!
//! Settings live in the app's config folder ([`FILE_NAME`]), never in the
//! repo, so each person can use their own profile names. Each setting is
//! inherited from the nearest folder above that has it, so a repo root can
//! name the profile and one subfolder a region.
//!
//! Inside a git repository, settings are kept by repository and path within
//! it, so every checkout of the repo — the main one and each worktree opened
//! to try a change before it's merged — uses the same folder → account
//! mapping. One checkout (workspace) can override a folder's settings for
//! itself only, e.g. to point a test worktree at a sandbox account; at any
//! one folder its override wins over the shared setting. Folders outside a
//! repository are kept by absolute path.
//!
//! The choices offered come from the providers' own CLI config: AWS profiles
//! from `~/.aws/config` and `~/.aws/credentials`, gcloud configurations from
//! `~/.config/gcloud/configurations`, Azure subscriptions from
//! `~/.azure/azureProfile.json` (each honouring its usual override variable).

use crate::shell_env::{EnvMap, ShellEnv};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Runtime, State};

pub const FILE_NAME: &str = "cloud.json";

/// Emitted to every window when any folder's settings change.
pub const CHANGED_EVENT: &str = "cloud://changed";

/// One setting a folder can have and the variables it gives tofu.
pub struct Field {
    pub key: &'static str,
    /// Shown in run output, e.g. "AWS profile".
    pub label: &'static str,
    pub vars: &'static [&'static str],
    /// Inherited variables that would override this setting, removed when it applies.
    pub clears: &'static [&'static str],
}

pub const FIELDS: &[Field] = &[
    Field {
        key: "aws.profile",
        label: "AWS profile",
        vars: &["AWS_PROFILE"],
        // Static keys in the environment win over a profile in every AWS SDK.
        clears: &["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN"],
    },
    Field { key: "aws.region", label: "AWS region", vars: &["AWS_REGION", "AWS_DEFAULT_REGION"], clears: &[] },
    Field { key: "gcp.configuration", label: "gcloud configuration", vars: &["CLOUDSDK_ACTIVE_CONFIG_NAME"], clears: &[] },
    Field { key: "gcp.project", label: "Google Cloud project", vars: &["GOOGLE_PROJECT", "CLOUDSDK_CORE_PROJECT"], clears: &[] },
    Field { key: "azure.subscription", label: "Azure subscription", vars: &["ARM_SUBSCRIPTION_ID"], clears: &[] },
    Field { key: "azure.tenant", label: "Azure tenant", vars: &["ARM_TENANT_ID"], clears: &[] },
];

fn field(key: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|f| f.key == key)
}

/// A folder's own settings, by [`Field::key`].
pub type Settings = BTreeMap<String, String>;

/// Settings by folder path within a repository or checkout (`.` is its top).
type ByPath = BTreeMap<String, Settings>;

/// Which settings a folder's own values are: shared by every checkout of
/// its repository (or, outside a repository, simply the folder's), or this
/// checkout's override.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
    #[default]
    Shared,
    Workspace,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct Store {
    /// By repository (its git common folder), then path within it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    repos: BTreeMap<String, ByPath>,
    /// Overrides, by checkout (its top folder), then path within it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    workspaces: BTreeMap<String, ByPath>,
    /// Folders outside any repository, by absolute path.
    #[serde(default)]
    folders: BTreeMap<String, Settings>,
}

/// A git checkout: the repository it belongs to and its top folder.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Checkout {
    /// The folder every checkout of the repo shares (`git rev-parse --git-common-dir`).
    repo: PathBuf,
    top: PathBuf,
}

/// The checkout `dir` is in, from the nearest `.git` at or above it: a
/// folder in a plain clone, or in a worktree a file naming its git folder,
/// whose `commondir` leads to the shared one. Read from disk, not by running
/// git, because every tofu run resolves settings.
pub(crate) fn checkout(dir: &Path) -> Option<Checkout> {
    for d in dir.ancestors() {
        let dot = d.join(".git");
        if dot.is_dir() {
            return Some(Checkout { repo: canon(&dot), top: d.to_path_buf() });
        }
        if let Ok(text) = std::fs::read_to_string(&dot) {
            let gitdir = d.join(text.trim().strip_prefix("gitdir:")?.trim());
            let repo = match std::fs::read_to_string(gitdir.join("commondir")) {
                Ok(common) => gitdir.join(common.trim()),
                // A submodule's git folder is its own repository.
                Err(_) => gitdir,
            };
            return Some(Checkout { repo: canon(&repo), top: d.to_path_buf() });
        }
    }
    None
}

impl Checkout {
    /// `dir`'s path within the checkout (`dir` is at or below `top`).
    fn rel(&self, dir: &Path) -> String {
        let parts: Vec<_> =
            dir.strip_prefix(&self.top).unwrap_or(Path::new("")).components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        if parts.is_empty() { ".".into() } else { parts.join("/") }
    }

    fn abs(&self, rel: &str) -> PathBuf {
        if rel == "." { self.top.clone() } else { self.top.join(rel) }
    }

    /// The repository's name: its folder's, or the one holding `.git` / `.bare`.
    fn name(&self) -> String {
        let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let own = name(&self.repo);
        if own.starts_with('.') {
            self.repo.parent().map(name).unwrap_or(own)
        } else {
            own.strip_suffix(".git").map(String::from).unwrap_or(own)
        }
    }

    fn slot(&self, scope: Scope, dir: &Path) -> Slot {
        let (owner, rel) = match scope {
            Scope::Shared => (path_key(&self.repo), self.rel(dir)),
            Scope::Workspace => (path_key(&self.top), self.rel(dir)),
        };
        Slot { scope, owner: Some(owner), path: rel }
    }
}

/// Where one folder's settings of one scope are kept.
struct Slot {
    scope: Scope,
    /// The repository or checkout; `None` for a folder outside any repository.
    owner: Option<String>,
    path: String,
}

impl Store {
    fn table(&self, slot: &Slot) -> Option<&ByPath> {
        match (&slot.owner, slot.scope) {
            (None, _) => Some(&self.folders),
            (Some(o), Scope::Shared) => self.repos.get(o),
            (Some(o), Scope::Workspace) => self.workspaces.get(o),
        }
    }

    fn get(&self, slot: &Slot) -> Option<&Settings> {
        self.table(slot)?.get(&slot.path)
    }

    /// Replace a slot's settings; empty ones remove it.
    fn put(&mut self, slot: &Slot, settings: Settings) {
        let (outer, owner) = match (&slot.owner, slot.scope) {
            (None, _) => {
                if settings.is_empty() {
                    self.folders.remove(&slot.path);
                } else {
                    self.folders.insert(slot.path.clone(), settings);
                }
                return;
            }
            (Some(o), Scope::Shared) => (&mut self.repos, o),
            (Some(o), Scope::Workspace) => (&mut self.workspaces, o),
        };
        let table = outer.entry(owner.clone()).or_default();
        if settings.is_empty() {
            table.remove(&slot.path);
        } else {
            table.insert(slot.path.clone(), settings);
        }
        if table.is_empty() {
            outer.remove(owner);
        }
    }

    /// Move folders kept by absolute path (as before repositories were
    /// recognised) that are in a repository to it. One whose repo path
    /// already has settings stays put. Returns whether anything moved.
    fn migrate(&mut self) -> bool {
        let mut moved = false;
        for (path, settings) in std::mem::take(&mut self.folders) {
            let dir = Path::new(&path);
            if let Some(co) = checkout(dir) {
                let slot = co.slot(Scope::Shared, dir);
                if self.get(&slot).is_none() {
                    self.put(&slot, settings);
                    moved = true;
                    continue;
                }
            }
            self.folders.insert(path, settings);
        }
        moved
    }
}

/// All folders' settings, loaded once and written back on every change.
pub struct CloudSettings {
    file: PathBuf,
    pub(crate) store: Mutex<Store>,
}

/// A setting in effect for a folder, the folder it was set on (in this
/// checkout) and whether it's this checkout's override.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolved {
    pub key: String,
    pub value: String,
    pub source: String,
    pub scope: Scope,
}

/// A folder's own settings of each scope, for the tree.
#[derive(Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Own {
    shared: Settings,
    workspace: Settings,
}

fn path_key(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

impl CloudSettings {
    /// Load from `file`. A missing file means no settings; an unreadable one
    /// is moved aside (to `<file>.bad`) rather than overwritten later.
    pub fn load(file: PathBuf) -> Self {
        let mut store = match std::fs::read_to_string(&file) {
            Err(_) => Store::default(),
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                let mut bad = file.clone().into_os_string();
                bad.push(".bad");
                eprintln!("{}: {e}; moved to {}", file.display(), Path::new(&bad).display());
                let _ = std::fs::rename(&file, &bad);
                Store::default()
            }),
        };
        let cs = CloudSettings { file, store: Mutex::new(Store::default()) };
        if store.migrate()
            && let Err(e) = cs.save(&store)
        {
            eprintln!("{e}");
        }
        *cs.store.lock().unwrap() = store;
        cs
    }

    fn slot(dir: &Path, scope: Scope) -> Result<Slot, String> {
        match (checkout(dir), scope) {
            (Some(co), _) => Ok(co.slot(scope, dir)),
            (None, Scope::Shared) => Ok(Slot { scope, owner: None, path: path_key(dir) }),
            (None, Scope::Workspace) => Err(format!("{} isn't in a git repository, so it has no workspace settings", dir.display())),
        }
    }

    /// `dir`'s own settings of `scope`.
    pub fn own(&self, dir: &Path, scope: Scope) -> Settings {
        let store = self.store.lock().unwrap();
        let get = |slot: Slot| store.get(&slot).cloned();
        let found = match Self::slot(dir, scope) {
            Err(_) => None,
            // Not yet moved to its repository (see `Store::migrate`).
            Ok(slot) if slot.owner.is_some() && scope == Scope::Shared => {
                get(slot).or_else(|| get(Slot { scope, owner: None, path: path_key(dir) }))
            }
            Ok(slot) => get(slot),
        };
        found.unwrap_or_default()
    }

    /// Every folder with settings of its own that the checkout `root` is in
    /// shows (as paths in that checkout), and every folder outside one.
    pub fn folders(&self, root: &Path) -> BTreeMap<String, Own> {
        let store = self.store.lock().unwrap();
        let mut out: BTreeMap<String, Own> = BTreeMap::new();
        for (path, s) in &store.folders {
            out.entry(path.clone()).or_default().shared = s.clone();
        }
        if let Some(co) = checkout(&canon(root)) {
            let tables = [(Scope::Shared, store.repos.get(&path_key(&co.repo))), (Scope::Workspace, store.workspaces.get(&path_key(&co.top)))];
            for (scope, table) in tables {
                for (rel, s) in table.into_iter().flatten() {
                    let own = out.entry(path_key(&co.abs(rel))).or_default();
                    *(if scope == Scope::Shared { &mut own.shared } else { &mut own.workspace }) = s.clone();
                }
            }
        }
        out
    }

    /// Replace `dir`'s own settings of `scope` (blank values dropped) and save.
    pub fn set(&self, dir: &Path, scope: Scope, settings: Settings) -> Result<(), String> {
        let mut clean = Settings::new();
        for (k, v) in settings {
            if field(&k).is_none() {
                return Err(format!("Unknown cloud setting `{k}`"));
            }
            let v = v.trim();
            if !v.is_empty() {
                clean.insert(k, v.to_string());
            }
        }
        let slot = Self::slot(dir, scope)?;
        let mut store = self.store.lock().unwrap();
        let before = store.clone();
        if slot.owner.is_some() && scope == Scope::Shared {
            // Replaces any it had from before repositories were recognised.
            store.put(&Slot { scope, owner: None, path: path_key(dir) }, Settings::new());
        }
        store.put(&slot, clean);
        let saved = self.save(&store);
        if saved.is_err() {
            // Keep memory and disk in agreement.
            *store = before;
        }
        saved
    }

    /// Write-then-rename so a crash never leaves a half-written file.
    fn save(&self, store: &Store) -> Result<(), String> {
        let err = |e: std::io::Error| format!("Could not save cloud settings to {}: {e}", self.file.display());
        std::fs::create_dir_all(self.file.parent().unwrap_or(Path::new(""))).map_err(err)?;
        let mut tmp = self.file.clone().into_os_string();
        tmp.push(".tmp");
        let json = serde_json::to_string_pretty(store).unwrap();
        std::fs::write(&tmp, json + "\n").and_then(|()| std::fs::rename(&tmp, &self.file)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            err(e)
        })
    }

    /// Settings in effect in `dir`: for each field, the value on the nearest
    /// folder at or above `dir` that has one; at one folder, the checkout's
    /// override first, then the shared setting. In [`FIELDS`] order.
    pub fn resolve(&self, dir: &Path) -> Vec<Resolved> {
        let dir = canon(dir);
        let co = checkout(&dir);
        let store = self.store.lock().unwrap();
        let mut found: BTreeMap<&str, Resolved> = BTreeMap::new();
        for d in dir.ancestors() {
            let inside = co.as_ref().filter(|c| d.starts_with(&c.top));
            let layers = [
                inside.map(|c| c.slot(Scope::Workspace, d)),
                inside.map(|c| c.slot(Scope::Shared, d)),
                Some(Slot { scope: Scope::Shared, owner: None, path: path_key(d) }),
            ];
            for slot in layers.into_iter().flatten() {
                let Some(own) = store.get(&slot) else { continue };
                for (k, v) in own {
                    if let Some(f) = field(k) {
                        found.entry(f.key).or_insert_with(|| Resolved { key: k.clone(), value: v.clone(), source: path_key(d), scope: slot.scope });
                    }
                }
            }
        }
        FIELDS.iter().filter_map(|f| found.remove(f.key)).collect()
    }
}

/// Apply `resolved` settings to a run's environment. Returns notes for the
/// run output. Where a project or tenant isn't set but the chosen gcloud
/// configuration or Azure subscription names one, that is used.
pub fn apply(resolved: &[Resolved], env: &mut EnvMap) -> Vec<String> {
    let mut notes = Vec::new();
    let mut set = |f: &Field, value: &str, from: String, env: &mut EnvMap| {
        let cleared: Vec<_> = f.clears.iter().filter(|v| env.remove(&OsString::from(v)).is_some()).copied().collect();
        for var in f.vars {
            env.insert(var.into(), value.into());
        }
        let mut what = format!("{from}; sets {}", f.vars.join(", "));
        if !cleared.is_empty() {
            what.push_str(&format!("; removes {}", cleared.join(", ")));
        }
        notes.push(format!("cloud: {} {value} ({what})", f.label));
    };
    let get = |key: &str| resolved.iter().find(|r| r.key == key).map(|r| r.value.as_str());
    let snapshot = env.clone();

    for r in resolved {
        // Keys come from FIELDS (see `resolve`).
        let only = if r.scope == Scope::Workspace { ", this workspace only" } else { "" };
        set(field(&r.key).unwrap(), &r.value, format!("from {}{only}", r.source), env);
    }
    if let (Some(name), None) = (get("gcp.configuration"), get("gcp.project"))
        && let Some(project) = gcp_configurations(&snapshot).0.into_iter().find(|c| c.name == name).and_then(|c| c.project)
    {
        set(field("gcp.project").unwrap(), &project, format!("from gcloud configuration {name}"), env);
    }
    if let (Some(id), None) = (get("azure.subscription"), get("azure.tenant"))
        && let Some(sub) = azure_subscriptions(&snapshot).0.into_iter().find(|s| s.id == id)
    {
        set(field("azure.tenant").unwrap(), &sub.tenant_id, format!("from subscription {}", sub.name), env);
    }
    notes
}

// ---- Discovery ---------------------------------------------------------------

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AwsProfile {
    name: String,
    region: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GcpConfiguration {
    name: String,
    project: Option<String>,
    account: Option<String>,
    active: bool,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AzureSubscription {
    id: String,
    name: String,
    tenant_id: String,
    is_default: bool,
}

/// What the providers' CLI config offers to pick from.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Available {
    aws_profiles: Vec<AwsProfile>,
    gcp_configurations: Vec<GcpConfiguration>,
    azure_subscriptions: Vec<AzureSubscription>,
    /// Config files that exist but couldn't be read.
    errors: Vec<String>,
}

pub fn discover(env: &EnvMap) -> Available {
    let (aws_profiles, mut errors) = aws_profiles(env);
    let (gcp_configurations, e) = gcp_configurations(env);
    errors.extend(e);
    let (azure_subscriptions, e) = azure_subscriptions(env);
    errors.extend(e);
    Available { aws_profiles, gcp_configurations, azure_subscriptions, errors }
}

fn var(env: &EnvMap, name: &str) -> Option<PathBuf> {
    env.get(&OsString::from(name)).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `$<override>` if set, else `$HOME/<default>`.
fn config_path(env: &EnvMap, override_var: &str, default: &str) -> Option<PathBuf> {
    var(env, override_var).or_else(|| var(env, "HOME").map(|h| h.join(default)))
}

/// File content; `None` if missing, an error note if unreadable.
fn read_optional(path: &Path, errors: &mut Vec<String>) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(c) => Some(c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            errors.push(format!("{}: {e}", path.display()));
            None
        }
    }
}

/// INI sections (`[name]`) and their `key = value` pairs.
fn ini(content: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut section = String::new();
    for line in content.lines().map(str::trim) {
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_string();
            out.entry(section.clone()).or_default();
        } else if let Some((k, v)) = line.split_once('=') {
            out.entry(section.clone()).or_default().insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

fn aws_profiles(env: &EnvMap) -> (Vec<AwsProfile>, Vec<String>) {
    let mut errors = Vec::new();
    let mut profiles: BTreeMap<String, Option<String>> = BTreeMap::new();
    let config = config_path(env, "AWS_CONFIG_FILE", ".aws/config").and_then(|p| read_optional(&p, &mut errors));
    for (section, keys) in ini(&config.unwrap_or_default()) {
        let name = if section == "default" { Some(section.as_str()) } else { section.strip_prefix("profile ") };
        if let Some(name) = name.map(str::trim) {
            profiles.insert(name.to_string(), keys.get("region").cloned());
        }
    }
    let creds = config_path(env, "AWS_SHARED_CREDENTIALS_FILE", ".aws/credentials").and_then(|p| read_optional(&p, &mut errors));
    for section in ini(&creds.unwrap_or_default()).into_keys().filter(|s| !s.is_empty()) {
        profiles.entry(section).or_default();
    }
    (profiles.into_iter().map(|(name, region)| AwsProfile { name, region }).collect(), errors)
}

fn gcp_configurations(env: &EnvMap) -> (Vec<GcpConfiguration>, Vec<String>) {
    let mut errors = Vec::new();
    let Some(dir) = config_path(env, "CLOUDSDK_CONFIG", ".config/gcloud") else { return (Vec::new(), errors) };
    let active = read_optional(&dir.join("active_config"), &mut errors).map(|s| s.trim().to_string());
    let mut configs: Vec<GcpConfiguration> = std::fs::read_dir(dir.join("configurations"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().strip_prefix("config_")?.to_string();
            let content = read_optional(&e.path(), &mut errors)?;
            let core = ini(&content).remove("core").unwrap_or_default();
            let active = active.as_deref() == Some(name.as_str());
            Some(GcpConfiguration { project: core.get("project").cloned(), account: core.get("account").cloned(), active, name })
        })
        .collect();
    configs.sort_by(|a, b| a.name.cmp(&b.name));
    (configs, errors)
}

fn azure_subscriptions(env: &EnvMap) -> (Vec<AzureSubscription>, Vec<String>) {
    let mut errors = Vec::new();
    let Some(path) = config_path(env, "AZURE_CONFIG_DIR", ".azure").map(|d| d.join("azureProfile.json")) else {
        return (Vec::new(), errors);
    };
    let Some(content) = read_optional(&path, &mut errors) else { return (Vec::new(), errors) };
    // The az CLI writes this file with a byte-order mark.
    let json: serde_json::Value = match serde_json::from_str(content.trim_start_matches('\u{feff}')) {
        Ok(v) => v,
        Err(e) => {
            errors.push(format!("{}: {e}", path.display()));
            return (Vec::new(), errors);
        }
    };
    let text = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
    let mut subs: Vec<AzureSubscription> = json["subscriptions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["id"].is_string())
        .map(|s| AzureSubscription {
            id: text(s, "id"),
            name: text(s, "name"),
            tenant_id: text(s, "tenantId"),
            is_default: s["isDefault"].as_bool().unwrap_or(false),
        })
        .collect();
    subs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    (subs, errors)
}

// ---- Commands ----------------------------------------------------------------

/// The repository a folder is in, for the settings dialog.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoInfo {
    name: String,
    /// This checkout's top folder.
    top: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudContext {
    /// The folder, as settings are keyed.
    dir: String,
    /// `None` outside a git repository: no workspace overrides there.
    repo: Option<RepoInfo>,
    /// Shared by every checkout of the repository.
    own: Settings,
    /// This checkout's override.
    workspace: Settings,
    /// What applies here from folders above (what `own` overrides).
    inherited: Vec<Resolved>,
    available: Available,
}

fn existing_dir(dir: &str) -> Result<PathBuf, String> {
    Path::new(dir).canonicalize().ok().filter(|p| p.is_dir()).ok_or_else(|| format!("{dir} is not a folder"))
}

/// Base environment (app + login shell), where provider config paths come from.
async fn base_env(shell: &ShellEnv) -> EnvMap {
    let mut env: EnvMap = std::env::vars_os().collect();
    env.extend(shell.get().await.iter().map(|(k, v)| (k.clone(), v.clone())));
    env
}

/// Everything the settings dialog shows for `dir`.
#[tauri::command]
pub async fn cloud_get(
    settings: State<'_, CloudSettings>,
    shell: State<'_, ShellEnv>,
    dir: String,
) -> Result<CloudContext, String> {
    let dir = existing_dir(&dir)?;
    let inherited = dir.parent().map(|p| settings.resolve(p)).unwrap_or_default();
    Ok(CloudContext {
        dir: path_key(&dir),
        repo: checkout(&dir).map(|co| RepoInfo { name: co.name(), top: path_key(&co.top) }),
        own: settings.own(&dir, Scope::Shared),
        workspace: settings.own(&dir, Scope::Workspace),
        inherited,
        available: discover(&base_env(&shell).await),
    })
}

/// Replace `dir`'s own settings: shared (the default) or this checkout's override.
#[tauri::command]
pub fn cloud_set<R: Runtime>(
    app: AppHandle<R>,
    settings: State<'_, CloudSettings>,
    dir: String,
    values: Settings,
    scope: Option<Scope>,
) -> Result<(), String> {
    settings.set(&existing_dir(&dir)?, scope.unwrap_or_default(), values)?;
    let _ = app.emit(CHANGED_EVENT, ());
    Ok(())
}

/// Settings in effect in `dir`.
#[tauri::command]
pub fn cloud_effective(settings: State<'_, CloudSettings>, dir: String) -> Vec<Resolved> {
    settings.resolve(Path::new(&dir))
}

/// Every folder with settings of its own, as seen from the workspace
/// `root`, for marking them in the tree.
#[tauri::command]
pub fn cloud_folders(settings: State<'_, CloudSettings>, root: String) -> BTreeMap<String, Own> {
    settings.folders(Path::new(&root))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::shell_env::testing::{ipc, temp_dir, webview};
    use serde_json::json;
    use tauri::{Listener, Manager};

    fn s(pairs: &[(&str, &str)]) -> Settings {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn env_of(pairs: &[(&str, &Path)]) -> EnvMap {
        pairs.iter().map(|(k, v)| (OsString::from(k), v.as_os_str().to_os_string())).collect()
    }

    fn get(env: &EnvMap, k: &str) -> Option<String> {
        env.get(&OsString::from(k)).map(|v| v.to_string_lossy().into_owned())
    }

    fn r(key: &str, value: &str, source: &Path) -> Resolved {
        Resolved { key: key.into(), value: value.into(), source: path_key(source), scope: Scope::Shared }
    }

    #[test]
    fn settings_inherit_per_field_and_persist() {
        let base = temp_dir("cloud-inherit");
        let leaf = base.join("repo/accounts/prd");
        std::fs::create_dir_all(&leaf).unwrap();
        let repo = base.join("repo");
        let file = base.join("config/cloud.json");
        let cs = CloudSettings::load(file.clone());
        assert!(cs.resolve(&leaf).is_empty());

        cs.set(&repo, Scope::Shared, s(&[("aws.profile", " dev "), ("aws.region", "us-east-1"), ("gcp.project", "  ")])).unwrap();
        cs.set(&leaf, Scope::Shared, s(&[("aws.profile", "prd"), ("azure.subscription", "sub-1")])).unwrap();
        assert_eq!(cs.own(&repo, Scope::Shared), s(&[("aws.profile", "dev"), ("aws.region", "us-east-1")]));
        assert_eq!(
            cs.resolve(&leaf),
            [r("aws.profile", "prd", &leaf), r("aws.region", "us-east-1", &repo), r("azure.subscription", "sub-1", &leaf)]
        );
        // Relative spellings resolve to the same folder.
        assert_eq!(cs.resolve(&leaf.join("../prd")), cs.resolve(&leaf));
        assert_eq!(cs.resolve(&repo.join("accounts")), [r("aws.profile", "dev", &repo), r("aws.region", "us-east-1", &repo)]);
        // A folder that's gone still inherits from those above it.
        assert_eq!(cs.resolve(&repo.join("gone")).len(), 2);

        // Saved, and loaded back by a fresh instance.
        let again = CloudSettings::load(file.clone());
        assert_eq!(again.folders(&base), cs.folders(&base));
        assert_eq!(again.folders(&base).len(), 2);

        // Clearing every value removes the folder.
        cs.set(&leaf, Scope::Shared, s(&[("aws.profile", "")])).unwrap();
        assert_eq!(cs.folders(&base).keys().collect::<Vec<_>>(), [&path_key(&repo)]);
        assert!(cs.own(&leaf, Scope::Shared).is_empty());

        assert_eq!(cs.set(&repo, Scope::Shared, s(&[("aws.nope", "x")])).unwrap_err(), "Unknown cloud setting `aws.nope`");
        // An entry for an unknown key (e.g. from a newer version) is ignored.
        cs.store.lock().unwrap().folders.get_mut(&path_key(&repo)).unwrap().insert("future".into(), "x".into());
        assert_eq!(cs.resolve(&repo).len(), 2);
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// A clone at `base/main` with a worktree at `base/wt` (as git lays them
    /// out); returns (main, worktree, the shared git folder).
    fn clone_with_worktree(base: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let main = base.join("main");
        let git = main.join(".git");
        let wt_git = git.join("worktrees/wt");
        std::fs::create_dir_all(&wt_git).unwrap();
        std::fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        let wt = base.join("wt");
        std::fs::create_dir_all(wt.join("accounts/prd")).unwrap();
        std::fs::create_dir_all(main.join("accounts/prd")).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        (main, wt, git)
    }

    #[test]
    fn finds_the_checkout_and_its_repository() {
        let base = temp_dir("cloud-checkout");
        let (main, wt, git) = clone_with_worktree(&base);
        let deep = wt.join("accounts/prd");
        assert_eq!(checkout(&main.join("accounts")), Some(Checkout { repo: git.clone(), top: main.clone() }));
        let co = checkout(&deep).unwrap();
        assert_eq!(co, Checkout { repo: git.clone(), top: wt.clone() });
        assert_eq!((co.rel(&deep), co.rel(&wt), co.rel(Path::new("/elsewhere"))), ("accounts/prd".into(), ".".into(), ".".into()));
        assert_eq!((co.abs("."), co.abs("accounts/prd")), (wt.clone(), deep.clone()));
        assert_eq!(co.name(), "main");

        // A submodule: its git folder (relative here) has no commondir.
        let sm = base.join("sm");
        std::fs::create_dir_all(base.join("modules/sm")).unwrap();
        std::fs::create_dir(&sm).unwrap();
        std::fs::write(sm.join(".git"), "gitdir: ../modules/sm").unwrap();
        assert_eq!(checkout(&sm).unwrap().repo, base.join("modules/sm"));
        // Not a git file, and not in a repository at all.
        let odd = base.join("odd");
        std::fs::create_dir(&odd).unwrap();
        std::fs::write(odd.join(".git"), "junk").unwrap();
        assert_eq!(checkout(&odd), None);
        assert_eq!(checkout(&base), None);

        // Names: a bare repo's folder, `.bare` and `.git` folders, a filesystem root.
        let named = |repo: &str| Checkout { repo: repo.into(), top: "/t".into() }.name();
        assert_eq!(named("/src/infra.git"), "infra");
        assert_eq!(named("/src/infra/.bare"), "infra");
        assert_eq!(named("/src/infra/.git"), "infra");
        assert_eq!(named("/src/plain"), "plain");
        assert_eq!(named("/.git"), "");
        assert_eq!(named("/"), "");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn checkouts_share_settings_and_a_workspace_can_override_them() {
        let base = temp_dir("cloud-shared");
        let (main, wt, git) = clone_with_worktree(&base);
        let file = base.join("cfg/cloud.json");
        let cs = CloudSettings::load(file.clone());
        let ws = |key: &str, value: &str, source: &Path| Resolved { scope: Scope::Workspace, ..r(key, value, source) };

        cs.set(&main, Scope::Shared, s(&[("aws.profile", "dev"), ("aws.region", "us-east-1")])).unwrap();
        cs.set(&main.join("accounts/prd"), Scope::Shared, s(&[("aws.profile", "prd")])).unwrap();
        // Set in the main checkout, in effect in the worktree, as its own paths.
        assert_eq!(cs.resolve(&wt.join("accounts/prd")), [r("aws.profile", "prd", &wt.join("accounts/prd")), r("aws.region", "us-east-1", &wt)]);
        assert_eq!(cs.own(&wt, Scope::Shared), s(&[("aws.profile", "dev"), ("aws.region", "us-east-1")]));

        // The worktree's override applies there only, winning at its own folder.
        cs.set(&wt, Scope::Workspace, s(&[("aws.profile", "sandbox")])).unwrap();
        assert_eq!(cs.resolve(&wt), [ws("aws.profile", "sandbox", &wt), r("aws.region", "us-east-1", &wt)]);
        assert_eq!(cs.resolve(&wt.join("accounts/prd"))[0], r("aws.profile", "prd", &wt.join("accounts/prd")));
        assert_eq!(cs.resolve(&main)[0], r("aws.profile", "dev", &main));
        assert_eq!(cs.own(&wt, Scope::Workspace), s(&[("aws.profile", "sandbox")]));
        assert!(cs.own(&main, Scope::Workspace).is_empty());
        assert!(cs.own(&base, Scope::Workspace).is_empty());

        // The tree of each checkout shows its own paths, overrides apart.
        let own = |shared: &[(&str, &str)], workspace: &[(&str, &str)]| Own { shared: s(shared), workspace: s(workspace) };
        let in_wt = cs.folders(&wt.join("accounts"));
        assert_eq!(in_wt.len(), 2);
        assert_eq!(in_wt[&path_key(&wt)], own(&[("aws.profile", "dev"), ("aws.region", "us-east-1")], &[("aws.profile", "sandbox")]));
        assert_eq!(cs.folders(&main)[&path_key(&main)].workspace, Settings::new());

        let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            saved,
            json!({
                "repos": {path_key(&git): {".": {"aws.profile": "dev", "aws.region": "us-east-1"}, "accounts/prd": {"aws.profile": "prd"}}},
                "workspaces": {path_key(&wt): {".": {"aws.profile": "sandbox"}}},
                "folders": {},
            })
        );
        // Clearing removes empty tables.
        cs.set(&wt, Scope::Workspace, Settings::new()).unwrap();
        assert!(cs.store.lock().unwrap().workspaces.is_empty());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn folders_saved_by_path_move_to_their_repository() {
        let base = temp_dir("cloud-migrate");
        let (main, wt, git) = clone_with_worktree(&base);
        let outside = base.join("outside");
        std::fs::create_dir(&outside).unwrap();
        let file = base.join("cloud.json");
        let prd = main.join("accounts/prd");
        let old = json!({
            "folders": {
                path_key(&main): {"aws.profile": "dev"},
                path_key(&outside): {"aws.profile": "out"},
                path_key(&prd): {"aws.profile": "old"},
            },
            "repos": {path_key(&git): {"accounts/prd": {"aws.profile": "prd"}}},
        });
        std::fs::write(&file, old.to_string()).unwrap();
        let cs = CloudSettings::load(file.clone());
        let store = cs.store.lock().unwrap().clone();
        assert_eq!(store.repos[&path_key(&git)]["."], s(&[("aws.profile", "dev")]));
        // Outside a repo, or already set in it: left where they were.
        assert_eq!(store.folders.keys().collect::<Vec<_>>(), [&path_key(&prd), &path_key(&outside)]);
        // Written back.
        assert!(std::fs::read_to_string(&file).unwrap().contains("\"repos\""));
        assert_eq!(cs.resolve(&wt)[0], r("aws.profile", "dev", &wt));
        assert_eq!(cs.resolve(&prd)[0], r("aws.profile", "prd", &prd));

        // A leftover shows as the folder's own until saved, which replaces it.
        cs.store.lock().unwrap().repos.clear();
        assert_eq!(cs.own(&prd, Scope::Shared), s(&[("aws.profile", "old")]));
        cs.set(&prd, Scope::Shared, s(&[("aws.region", "eu-west-1")])).unwrap();
        assert_eq!(cs.own(&prd, Scope::Shared), s(&[("aws.region", "eu-west-1")]));
        assert!(!cs.store.lock().unwrap().folders.contains_key(&path_key(&prd)));

        // Nothing to move: the file isn't rewritten. Moved, but can't be saved: still used.
        std::fs::write(&file, "{}").unwrap();
        CloudSettings::load(file.clone());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{}");
        let ro = base.join("ro");
        std::fs::create_dir(&ro).unwrap();
        std::fs::write(ro.join("cloud.json"), old.to_string()).unwrap();
        std::fs::create_dir(ro.join("cloud.json.tmp")).unwrap(); // blocks the write
        let cs = CloudSettings::load(ro.join("cloud.json"));
        assert_eq!(cs.resolve(&main)[0], r("aws.profile", "dev", &main));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn workspace_overrides_are_noted_as_such() {
        let mut env = EnvMap::new();
        let notes = apply(&[Resolved { scope: Scope::Workspace, ..r("aws.profile", "sb", Path::new("/w")) }], &mut env);
        assert_eq!(notes, ["cloud: AWS profile sb (from /w, this workspace only; sets AWS_PROFILE)"]);
    }

    #[test]
    fn unreadable_settings_are_moved_aside() {
        let base = temp_dir("cloud-bad");
        let file = base.join("cloud.json");
        std::fs::write(&file, "{ not json").unwrap();
        let cs = CloudSettings::load(file.clone());
        assert!(cs.folders(&base).is_empty());
        assert!(!file.exists());
        assert_eq!(std::fs::read_to_string(base.join("cloud.json.bad")).unwrap(), "{ not json");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn failed_saves_leave_settings_unchanged() {
        let base = temp_dir("cloud-save-fail");
        let dir = base.join("d");
        std::fs::create_dir(&dir).unwrap();
        // The config "folder" is a file: nothing can be written.
        std::fs::write(base.join("blocked"), "").unwrap();
        let cs = CloudSettings::load(base.join("blocked/cloud.json"));
        let err = cs.set(&dir, Scope::Shared, s(&[("aws.profile", "x")])).unwrap_err();
        assert!(err.starts_with("Could not save cloud settings to "), "{err}");
        assert!(cs.folders(&base).is_empty());

        // The rename fails (target is a folder): no temp file left behind, old value kept.
        let file = base.join("cfg/cloud.json");
        let cs = CloudSettings::load(file.clone());
        cs.set(&dir, Scope::Shared, s(&[("aws.profile", "a")])).unwrap();
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir_all(file.join("x")).unwrap();
        assert!(cs.set(&dir, Scope::Shared, s(&[("aws.profile", "b")])).is_err());
        assert!(cs.set(&dir, Scope::Shared, Settings::new()).is_err());
        assert_eq!(cs.own(&dir, Scope::Shared), s(&[("aws.profile", "a")]));
        assert_eq!(std::fs::read_dir(base.join("cfg")).unwrap().count(), 1);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn apply_sets_variables_and_clears_static_aws_keys() {
        let root = Path::new("/r");
        let mut env: EnvMap = [("AWS_ACCESS_KEY_ID", "k"), ("AWS_SESSION_TOKEN", "t"), ("KEEP", "1")]
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect();
        let notes = apply(&[r("aws.profile", "prd", root), r("aws.region", "eu-west-1", root)], &mut env);
        assert_eq!(get(&env, "AWS_PROFILE").unwrap(), "prd");
        assert_eq!(get(&env, "AWS_REGION").unwrap(), "eu-west-1");
        assert_eq!(get(&env, "AWS_DEFAULT_REGION").unwrap(), "eu-west-1");
        assert!(get(&env, "AWS_ACCESS_KEY_ID").is_none() && get(&env, "AWS_SESSION_TOKEN").is_none());
        assert_eq!(get(&env, "KEEP").unwrap(), "1");
        assert_eq!(
            notes,
            [
                "cloud: AWS profile prd (from /r; sets AWS_PROFILE; removes AWS_ACCESS_KEY_ID, AWS_SESSION_TOKEN)",
                "cloud: AWS region eu-west-1 (from /r; sets AWS_REGION, AWS_DEFAULT_REGION)",
            ]
        );
        assert!(apply(&[], &mut env).is_empty());
    }

    /// A home folder with every provider's CLI config.
    fn fake_home(name: &str) -> PathBuf {
        let home = temp_dir(name);
        let w = |rel: &str, content: &str| {
            let p = home.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        };
        w(".aws/config", "# c\n[default]\nregion = us-east-1\n\n[profile prd]\nregion=eu-west-1\n[sso-session corp]\nsso_region = x\n[profile  dev ]\n");
        w(".aws/credentials", "; c\n[default]\naws_access_key_id = x\n[legacy]\nkey=1\nstray line\n");
        w(".config/gcloud/active_config", "work\n");
        w(".config/gcloud/configurations/config_work", "[core]\nproject = work-123\naccount = me@example.com\n");
        w(".config/gcloud/configurations/config_default", "[compute]\nregion = x\n");
        w(".config/gcloud/configurations/other", "[core]\nproject = ignored\n");
        w(
            ".azure/azureProfile.json",
            "\u{feff}{\"subscriptions\": [{\"id\": \"s2\", \"name\": \"zeta\", \"tenantId\": \"t2\"}, {\"id\": \"s1\", \"name\": \"Alpha\", \"tenantId\": \"t1\", \"isDefault\": true}, {\"name\": \"no id\"}]}",
        );
        home
    }

    #[test]
    fn discovers_profiles_configurations_and_subscriptions() {
        let home = fake_home("cloud-discover");
        let found = discover(&env_of(&[("HOME", &home)]));
        let json = serde_json::to_value(&found).unwrap();
        assert_eq!(
            json,
            json!({
                "awsProfiles": [
                    {"name": "default", "region": "us-east-1"},
                    {"name": "dev", "region": null},
                    {"name": "legacy", "region": null},
                    {"name": "prd", "region": "eu-west-1"},
                ],
                "gcpConfigurations": [
                    {"name": "default", "project": null, "account": null, "active": false},
                    {"name": "work", "project": "work-123", "account": "me@example.com", "active": true},
                ],
                "azureSubscriptions": [
                    {"id": "s1", "name": "Alpha", "tenantId": "t1", "isDefault": true},
                    {"id": "s2", "name": "zeta", "tenantId": "t2", "isDefault": false},
                ],
                "errors": [],
            })
        );

        // Override variables point elsewhere; nothing there.
        let empty = home.join("empty");
        let found = discover(&env_of(&[
            ("HOME", &home),
            ("AWS_CONFIG_FILE", &empty),
            ("AWS_SHARED_CREDENTIALS_FILE", &empty),
            ("CLOUDSDK_CONFIG", &empty),
            ("AZURE_CONFIG_DIR", &empty),
        ]));
        assert!(found.aws_profiles.is_empty() && found.gcp_configurations.is_empty() && found.azure_subscriptions.is_empty());
        assert!(found.errors.is_empty());
        // No HOME: nothing to look at.
        let found = discover(&EnvMap::new());
        assert!(found.aws_profiles.is_empty() && found.gcp_configurations.is_empty() && found.azure_subscriptions.is_empty());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn discovery_reports_unreadable_config() {
        let home = temp_dir("cloud-discover-bad");
        for d in [".aws/config", ".aws/credentials", ".config/gcloud/active_config", ".config/gcloud/configurations/config_x"] {
            std::fs::create_dir_all(home.join(d)).unwrap(); // a folder where a file belongs
        }
        std::fs::create_dir_all(home.join(".azure")).unwrap();
        std::fs::write(home.join(".azure/azureProfile.json"), "nope").unwrap();
        let found = discover(&env_of(&[("HOME", &home)]));
        assert_eq!(found.errors.len(), 5, "{:?}", found.errors);
        assert!(found.errors[4].starts_with(&*home.join(".azure/azureProfile.json").to_string_lossy()));
        assert!(found.gcp_configurations.is_empty());

        // A profile file without a subscriptions list.
        std::fs::write(home.join(".azure/azureProfile.json"), "{}").unwrap();
        assert!(azure_subscriptions(&env_of(&[("HOME", &home)])).0.is_empty());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn apply_fills_project_and_tenant_from_the_chosen_config() {
        let home = fake_home("cloud-derive");
        let root = Path::new("/r");
        let mut env = env_of(&[("HOME", &home)]);
        let notes = apply(&[r("gcp.configuration", "work", root), r("azure.subscription", "s1", root)], &mut env);
        assert_eq!(get(&env, "CLOUDSDK_ACTIVE_CONFIG_NAME").unwrap(), "work");
        assert_eq!(get(&env, "GOOGLE_PROJECT").unwrap(), "work-123");
        assert_eq!(get(&env, "CLOUDSDK_CORE_PROJECT").unwrap(), "work-123");
        assert_eq!(get(&env, "ARM_SUBSCRIPTION_ID").unwrap(), "s1");
        assert_eq!(get(&env, "ARM_TENANT_ID").unwrap(), "t1");
        assert_eq!(notes[2], "cloud: Google Cloud project work-123 (from gcloud configuration work; sets GOOGLE_PROJECT, CLOUDSDK_CORE_PROJECT)");
        assert_eq!(notes[3], "cloud: Azure tenant t1 (from subscription Alpha; sets ARM_TENANT_ID)");

        // Explicit values win; unknown or project-less choices fill nothing.
        let mut env = env_of(&[("HOME", &home)]);
        apply(&[r("gcp.configuration", "work", root), r("gcp.project", "mine", root), r("azure.subscription", "s2", root), r("azure.tenant", "tx", root)], &mut env);
        assert_eq!((get(&env, "GOOGLE_PROJECT").unwrap(), get(&env, "ARM_TENANT_ID").unwrap()), ("mine".into(), "tx".into()));
        let mut env = env_of(&[("HOME", &home)]);
        assert_eq!(apply(&[r("gcp.configuration", "default", root), r("azure.subscription", "gone", root)], &mut env).len(), 2);
        assert!(get(&env, "GOOGLE_PROJECT").is_none() && get(&env, "ARM_TENANT_ID").is_none());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn commands_read_and_write_settings() {
        let base = temp_dir("cloud-commands");
        let home = fake_home("cloud-commands-home");
        let sub = base.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![cloud_get, cloud_set, cloud_effective, cloud_folders])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(CloudSettings::load(base.join("cfg/cloud.json")));
        app.manage(ShellEnv::preloaded(env_of(&[("HOME", &home)])));
        let changes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = changes.clone();
        app.listen_any(CHANGED_EVENT, move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let view = webview(&app);
        let p = |d: &Path| d.to_string_lossy().into_owned();

        ipc(&view, "cloud_set", json!({"dir": base, "values": {"aws.profile": "dev"}})).unwrap();
        ipc(&view, "cloud_set", json!({"dir": sub, "values": {"aws.region": "eu-west-1"}})).unwrap();
        assert_eq!(changes.load(std::sync::atomic::Ordering::SeqCst), 2);

        let ctx = ipc(&view, "cloud_get", json!({"dir": sub.join("..").join("sub")})).unwrap();
        assert_eq!(ctx["dir"], p(&sub));
        assert_eq!(ctx["own"], json!({"aws.region": "eu-west-1"}));
        assert_eq!(ctx["workspace"], json!({}));
        assert_eq!(ctx["repo"], json!(null));
        assert_eq!(ctx["inherited"], json!([{"key": "aws.profile", "value": "dev", "source": p(&base), "scope": "shared"}]));
        assert_eq!(ctx["available"]["awsProfiles"].as_array().unwrap().len(), 4);

        let eff = ipc(&view, "cloud_effective", json!({"dir": sub})).unwrap();
        assert_eq!(eff.as_array().unwrap().len(), 2);
        let folders = ipc(&view, "cloud_folders", json!({"root": base})).unwrap();
        assert_eq!(
            folders,
            json!({
                p(&base): {"shared": {"aws.profile": "dev"}, "workspace": {}},
                p(&sub): {"shared": {"aws.region": "eu-west-1"}, "workspace": {}},
            })
        );

        // In a repository: overrides for this checkout only.
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        ipc(&view, "cloud_set", json!({"dir": repo, "values": {"aws.profile": "sandbox"}, "scope": "workspace"})).unwrap();
        let ctx = ipc(&view, "cloud_get", json!({"dir": repo})).unwrap();
        assert_eq!(ctx["repo"], json!({"name": "repo", "top": p(&repo)}));
        assert_eq!((&ctx["own"], &ctx["workspace"]), (&json!({}), &json!({"aws.profile": "sandbox"})));
        // Outside one there's nothing to override.
        let err = ipc(&view, "cloud_set", json!({"dir": sub, "values": {}, "scope": "workspace"})).unwrap_err();
        assert_eq!(err, json!(format!("{} isn't in a git repository, so it has no workspace settings", p(&sub))));

        // The filesystem root has nothing above it.
        let ctx = block_on_get(&app, "/");
        assert_eq!(ctx["inherited"], json!([]));

        // Not a folder; bad values.
        let missing = base.join("missing");
        assert_eq!(ipc(&view, "cloud_get", json!({"dir": missing})).unwrap_err(), json!(format!("{} is not a folder", p(&missing))));
        assert!(ipc(&view, "cloud_set", json!({"dir": missing, "values": {}})).is_err());
        assert!(ipc(&view, "cloud_set", json!({"dir": base, "values": {"bad": "x"}})).is_err());
        assert_eq!(changes.load(std::sync::atomic::Ordering::SeqCst), 3);
        std::fs::remove_dir_all(&base).unwrap();
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// Calls missing app state fail instead of running.
    #[test]
    fn commands_need_their_state() {
        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![cloud_get, cloud_set, cloud_effective, cloud_folders])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let view = webview(&app);
        let fails = |cmd: &str| {
            let err = ipc(&view, cmd, json!({"dir": "/", "root": "/", "values": {}})).unwrap_err();
            assert!(err.as_str().unwrap().contains("state not managed"), "{cmd}: {err}");
        };
        for cmd in ["cloud_get", "cloud_set", "cloud_effective", "cloud_folders"] {
            fails(cmd);
        }
        // Settings, but no shell environment.
        app.manage(CloudSettings::load(PathBuf::from("/nonexistent/cloud.json")));
        fails("cloud_get");
        // Both, but missing arguments.
        app.manage(ShellEnv::preloaded(EnvMap::new()));
        for (cmd, key) in [("cloud_get", "dir"), ("cloud_set", "dir"), ("cloud_effective", "dir"), ("cloud_folders", "root")] {
            let err = ipc(&view, cmd, json!({})).unwrap_err();
            assert!(err.as_str().unwrap().contains(&format!("missing required key {key}")), "{cmd}: {err}");
        }
        let err = ipc(&view, "cloud_set", json!({"dir": "/"})).unwrap_err();
        assert!(err.as_str().unwrap().contains("missing required key values"), "{err}");
    }

    fn block_on_get(app: &tauri::App<tauri::test::MockRuntime>, dir: &str) -> serde_json::Value {
        let ctx = tauri::async_runtime::block_on(cloud_get(app.state(), app.state(), dir.into())).unwrap();
        serde_json::to_value(ctx).unwrap()
    }
}
