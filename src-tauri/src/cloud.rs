//! Per-folder cloud accounts: which AWS profile, Google Cloud configuration
//! or Azure subscription tofu uses in a folder and every folder below it.
//!
//! Settings live in the app's config folder ([`FILE_NAME`]), keyed by absolute
//! folder path, never in the repo, so each person can use their own profile
//! names. Each setting is inherited from the nearest folder above that has
//! it, so a repo root can name the profile and one subfolder a region.
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

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Store {
    #[serde(default)]
    folders: BTreeMap<String, Settings>,
}

/// All folders' settings, loaded once and written back on every change.
pub struct CloudSettings {
    file: PathBuf,
    pub(crate) store: Mutex<Store>,
}

/// A setting in effect for a folder, and the folder it was set on.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolved {
    pub key: String,
    pub value: String,
    pub source: String,
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
        let store = match std::fs::read_to_string(&file) {
            Err(_) => Store::default(),
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                let mut bad = file.clone().into_os_string();
                bad.push(".bad");
                eprintln!("{}: {e}; moved to {}", file.display(), Path::new(&bad).display());
                let _ = std::fs::rename(&file, &bad);
                Store::default()
            }),
        };
        CloudSettings { file, store: Mutex::new(store) }
    }

    pub fn own(&self, dir: &Path) -> Settings {
        self.store.lock().unwrap().folders.get(&path_key(dir)).cloned().unwrap_or_default()
    }

    pub fn folders(&self) -> BTreeMap<String, Settings> {
        self.store.lock().unwrap().folders.clone()
    }

    /// Replace `dir`'s own settings (blank values dropped) and save.
    pub fn set(&self, dir: &Path, settings: Settings) -> Result<(), String> {
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
        let mut store = self.store.lock().unwrap();
        let key = path_key(dir);
        let previous = if clean.is_empty() { store.folders.remove(&key) } else { store.folders.insert(key.clone(), clean) };
        let saved = self.save(&store);
        if saved.is_err() {
            // Keep memory and disk in agreement.
            match previous {
                Some(p) => store.folders.insert(key, p),
                None => store.folders.remove(&key),
            };
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
    /// folder at or above `dir` that has one. In [`FIELDS`] order.
    pub fn resolve(&self, dir: &Path) -> Vec<Resolved> {
        let dir = canon(dir);
        let store = self.store.lock().unwrap();
        let mut found: BTreeMap<&str, Resolved> = BTreeMap::new();
        for d in dir.ancestors() {
            let Some(own) = store.folders.get(&path_key(d)) else { continue };
            for (k, v) in own {
                if let Some(f) = field(k) {
                    found.entry(f.key).or_insert_with(|| Resolved { key: k.clone(), value: v.clone(), source: path_key(d) });
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
        set(field(&r.key).unwrap(), &r.value, format!("from {}", r.source), env);
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudContext {
    /// The folder, as settings are keyed.
    dir: String,
    own: Settings,
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
        own: settings.own(&dir),
        inherited,
        available: discover(&base_env(&shell).await),
    })
}

#[tauri::command]
pub fn cloud_set<R: Runtime>(
    app: AppHandle<R>,
    settings: State<'_, CloudSettings>,
    dir: String,
    values: Settings,
) -> Result<(), String> {
    settings.set(&existing_dir(&dir)?, values)?;
    let _ = app.emit(CHANGED_EVENT, ());
    Ok(())
}

/// Settings in effect in `dir`.
#[tauri::command]
pub fn cloud_effective(settings: State<'_, CloudSettings>, dir: String) -> Vec<Resolved> {
    settings.resolve(Path::new(&dir))
}

/// Every folder with settings of its own, for marking them in the tree.
#[tauri::command]
pub fn cloud_folders(settings: State<'_, CloudSettings>) -> BTreeMap<String, Settings> {
    settings.folders()
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
        Resolved { key: key.into(), value: value.into(), source: path_key(source) }
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

        cs.set(&repo, s(&[("aws.profile", " dev "), ("aws.region", "us-east-1"), ("gcp.project", "  ")])).unwrap();
        cs.set(&leaf, s(&[("aws.profile", "prd"), ("azure.subscription", "sub-1")])).unwrap();
        assert_eq!(cs.own(&repo), s(&[("aws.profile", "dev"), ("aws.region", "us-east-1")]));
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
        assert_eq!(again.folders(), cs.folders());
        assert_eq!(again.folders().len(), 2);

        // Clearing every value removes the folder.
        cs.set(&leaf, s(&[("aws.profile", "")])).unwrap();
        assert_eq!(cs.folders().keys().collect::<Vec<_>>(), [&path_key(&repo)]);
        assert!(cs.own(&leaf).is_empty());

        assert_eq!(cs.set(&repo, s(&[("aws.nope", "x")])).unwrap_err(), "Unknown cloud setting `aws.nope`");
        // An entry for an unknown key (e.g. from a newer version) is ignored.
        cs.store.lock().unwrap().folders.get_mut(&path_key(&repo)).unwrap().insert("future".into(), "x".into());
        assert_eq!(cs.resolve(&repo).len(), 2);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn unreadable_settings_are_moved_aside() {
        let base = temp_dir("cloud-bad");
        let file = base.join("cloud.json");
        std::fs::write(&file, "{ not json").unwrap();
        let cs = CloudSettings::load(file.clone());
        assert!(cs.folders().is_empty());
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
        let err = cs.set(&dir, s(&[("aws.profile", "x")])).unwrap_err();
        assert!(err.starts_with("Could not save cloud settings to "), "{err}");
        assert!(cs.folders().is_empty());

        // The rename fails (target is a folder): no temp file left behind, old value kept.
        let file = base.join("cfg/cloud.json");
        let cs = CloudSettings::load(file.clone());
        cs.set(&dir, s(&[("aws.profile", "a")])).unwrap();
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir_all(file.join("x")).unwrap();
        assert!(cs.set(&dir, s(&[("aws.profile", "b")])).is_err());
        assert!(cs.set(&dir, Settings::new()).is_err());
        assert_eq!(cs.own(&dir), s(&[("aws.profile", "a")]));
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
        assert_eq!(ctx["inherited"], json!([{"key": "aws.profile", "value": "dev", "source": p(&base)}]));
        assert_eq!(ctx["available"]["awsProfiles"].as_array().unwrap().len(), 4);

        let eff = ipc(&view, "cloud_effective", json!({"dir": sub})).unwrap();
        assert_eq!(eff.as_array().unwrap().len(), 2);
        let folders = ipc(&view, "cloud_folders", json!({})).unwrap();
        assert_eq!(folders, json!({p(&base): {"aws.profile": "dev"}, p(&sub): {"aws.region": "eu-west-1"}}));

        // The filesystem root has nothing above it.
        let ctx = block_on_get(&app, "/");
        assert_eq!(ctx["inherited"], json!([]));

        // Not a folder; bad values.
        let missing = base.join("missing");
        assert_eq!(ipc(&view, "cloud_get", json!({"dir": missing})).unwrap_err(), json!(format!("{} is not a folder", p(&missing))));
        assert!(ipc(&view, "cloud_set", json!({"dir": missing, "values": {}})).is_err());
        assert!(ipc(&view, "cloud_set", json!({"dir": base, "values": {"bad": "x"}})).is_err());
        assert_eq!(changes.load(std::sync::atomic::Ordering::SeqCst), 2);
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
            let err = ipc(&view, cmd, json!({"dir": "/", "values": {}})).unwrap_err();
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
        for (cmd, key) in [("cloud_get", "dir"), ("cloud_set", "dir"), ("cloud_effective", "dir")] {
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
