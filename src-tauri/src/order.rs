//! Run order for module folders, from remote-state dependencies.
//!
//! A module's state lives where its `backend` block says (e.g. an S3 bucket
//! and key). Another module that reads it through
//! `data "terraform_remote_state"` with the same location depends on it, so
//! the producer must run first. Locations are computed by evaluating the HCL
//! (variables from defaults and auto-loaded tfvars, then locals), which is
//! how names like `eg-${var.major_environment}-…-infra-state` resolve.
//!
//! Anything that can't be evaluated simply contributes no ordering.

use hcl::eval::{Context, Evaluate};
use hcl::{Body, Expression, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Where a module's state lives: backend type plus its identifying settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StateId {
    backend: String,
    bucket: String,
    /// `key` for s3, `prefix` for gcs, `path` for local.
    key: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ModuleRefs {
    /// This module's own state location.
    pub state: Option<StateId>,
    /// State locations it reads via `terraform_remote_state`.
    pub reads: Vec<StateId>,
}

fn read_bodies(dir: &Path, exts: &[&str], filter: impl Fn(&str) -> bool) -> Vec<(String, Body)> {
    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(n, _)| exts.iter().any(|x| n.ends_with(x)) && filter(n))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|(n, p)| Some((n, hcl::parse(&std::fs::read_to_string(p).ok()?).ok()?)))
        .collect()
}

fn as_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn state_id(backend: &str, get: impl Fn(&str) -> Option<String>) -> Option<StateId> {
    let (bucket, key) = match backend {
        "s3" => (get("bucket")?, get("key")?),
        "gcs" => (get("bucket")?, get("prefix").unwrap_or_default()),
        "local" => (String::new(), get("path").unwrap_or_else(|| "terraform.tfstate".into())),
        _ => return None,
    };
    Some(StateId { backend: backend.to_string(), bucket, key })
}

/// Analyze the module in `dir` (reads `*.tf`, `*.tofu`, auto-loaded tfvars).
pub fn analyze(dir: &Path) -> ModuleRefs {
    let tf = read_bodies(dir, &[".tf", ".tofu"], |_| true);

    // ---- Variables: defaults, then terraform.tfvars, then *.auto.tfvars (lexical) ----
    let empty = Context::new();
    let mut vars: hcl::Map<String, Value> = hcl::Map::new();
    let mut locals_exprs: BTreeMap<String, Expression> = BTreeMap::new();
    for (_, body) in &tf {
        for block in body.blocks() {
            match block.identifier() {
                "variable" => {
                    let Some(name) = block.labels().first().map(|l| l.as_str().to_string()) else { continue };
                    if let Some(v) = block
                        .body()
                        .attributes()
                        .find(|a| a.key() == "default")
                        .and_then(|a| a.expr().evaluate(&empty).ok())
                    {
                        vars.insert(name, v);
                    }
                }
                "locals" => {
                    for a in block.body().attributes() {
                        locals_exprs.insert(a.key().to_string(), a.expr().clone());
                    }
                }
                _ => {}
            }
        }
    }
    let tfvars = read_bodies(dir, &[".tfvars"], |n| n == "terraform.tfvars" || n.ends_with(".auto.tfvars"));
    let (main, auto): (Vec<_>, Vec<_>) = tfvars.into_iter().partition(|(n, _)| n == "terraform.tfvars");
    for (_, body) in main.iter().chain(auto.iter()) {
        for a in body.attributes() {
            if let Ok(v) = a.expr().evaluate(&empty) {
                vars.insert(a.key().to_string(), v);
            }
        }
    }

    // ---- Locals: evaluate in passes until nothing new resolves ----
    let mut locals: hcl::Map<String, Value> = hcl::Map::new();
    loop {
        let mut ctx = Context::new();
        ctx.declare_var("var", Value::Object(vars.clone()));
        ctx.declare_var("local", Value::Object(locals.clone()));
        let before = locals.len();
        for (k, e) in &locals_exprs {
            if !locals.contains_key(k)
                && let Ok(v) = e.evaluate(&ctx)
            {
                locals.insert(k.clone(), v);
            }
        }
        if locals.len() == before {
            break;
        }
    }
    let mut ctx = Context::new();
    ctx.declare_var("var", Value::Object(vars));
    ctx.declare_var("local", Value::Object(locals));
    let eval_str = |body: &hcl::Body, key: &str| {
        body.attributes().find(|a| a.key() == key).and_then(|a| a.expr().evaluate(&ctx).ok()).as_ref().and_then(as_str)
    };

    // ---- Own backend and remote-state reads ----
    let mut refs = ModuleRefs::default();
    for (_, body) in &tf {
        for block in body.blocks() {
            match (block.identifier(), block.labels().first().map(|l| l.as_str())) {
                ("terraform", _) => {
                    for b in block.body().blocks().filter(|b| b.identifier() == "backend") {
                        if let Some(kind) = b.labels().first() {
                            refs.state = state_id(kind.as_str(), |k| eval_str(b.body(), k));
                        }
                    }
                }
                ("data", Some("terraform_remote_state")) => {
                    let b = block.body();
                    let Some(kind) = eval_str(b, "backend") else { continue };
                    let config = b
                        .attributes()
                        .find(|a| a.key() == "config")
                        .and_then(|a| a.expr().evaluate(&ctx).ok());
                    if let Some(Value::Object(cfg)) = config
                        && let Some(id) = state_id(&kind, |k| cfg.get(k).and_then(as_str))
                    {
                        refs.reads.push(id);
                    }
                }
                _ => {}
            }
        }
    }
    refs
}

/// Order `paths` (folders under `top`) so producers come before consumers.
/// Ties, and anything in a cycle, fall back to path order. Returns the
/// ordered paths and, for each, the listed paths it depends on.
pub fn order(top: &Path, paths: &[String], cache: &mut HashMap<PathBuf, ModuleRefs>) -> Vec<(String, Vec<String>)> {
    let refs: Vec<ModuleRefs> = paths
        .iter()
        .map(|p| {
            let dir = if p == "." { top.to_path_buf() } else { top.join(p) };
            cache.entry(dir.clone()).or_insert_with(|| analyze(&dir)).clone()
        })
        .collect();

    let producer: HashMap<&StateId, usize> =
        refs.iter().enumerate().filter_map(|(i, r)| r.state.as_ref().map(|s| (s, i))).collect();
    let mut deps: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); paths.len()];
    for (i, r) in refs.iter().enumerate() {
        for read in &r.reads {
            if let Some(&j) = producer.get(read)
                && j != i
            {
                deps[i].insert(j);
            }
        }
    }

    // Kahn's algorithm, always taking the alphabetically first ready folder.
    let mut remaining: Vec<BTreeSet<usize>> = deps.clone();
    let mut done = vec![false; paths.len()];
    let mut out = Vec::with_capacity(paths.len());
    while out.len() < paths.len() {
        let next = (0..paths.len())
            .filter(|&i| !done[i] && remaining[i].is_empty())
            .min_by(|&a, &b| paths[a].cmp(&paths[b]))
            // Cycle: break it at the alphabetically first remaining folder.
            .or_else(|| (0..paths.len()).filter(|&i| !done[i]).min_by(|&a, &b| paths[a].cmp(&paths[b])))
            .unwrap();
        done[next] = true;
        for r in remaining.iter_mut() {
            r.remove(&next);
        }
        out.push((paths[next].clone(), deps[next].iter().map(|&j| paths[j].clone()).collect()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BACKEND: &str = r#"
locals {
  state_bucket  = "eg-${var.env}-${local.region}-state"
  shared_bucket = "eg-${var.env}-shared-state"
  region        = var.region == "global" ? "us-east-1" : var.region
}
terraform {
  backend "s3" {
    bucket = local.state_bucket
    key    = var.s3_key
    region = local.region
  }
}
variable "env" {}
variable "region" {}
variable "s3_key" {}
"#;

    fn module(base: &Path, rel: &str, key: &str, extra: &str) {
        let d = base.join(rel);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("backend.tf"), BACKEND).unwrap();
        std::fs::write(d.join("main.tf"), extra).unwrap();
        std::fs::write(d.join("env.auto.tfvars"), "env = \"prd\"\nregion = \"global\"\n").unwrap();
        std::fs::write(d.join("module.auto.tfvars"), format!("s3_key = \"{key}\"\n")).unwrap();
    }

    fn reads(key: &str) -> String {
        format!(
            r#"data "terraform_remote_state" "x" {{
  backend = "s3"
  config = {{
    bucket = local.state_bucket
    key    = "{key}"
    region = local.region
  }}
}}
"#
        )
    }

    #[test]
    fn evaluates_backend_and_remote_state() {
        let base = std::env::temp_dir().join(format!("gestalt-order-a-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        module(&base, "lb", "lb/terraform.tfstate", "");
        module(&base, "app", "app/terraform.tfstate", &reads("lb/terraform.tfstate"));

        let lb = analyze(&base.join("lb"));
        let expected = StateId { backend: "s3".into(), bucket: "eg-prd-us-east-1-state".into(), key: "lb/terraform.tfstate".into() };
        assert_eq!(lb.state.as_ref(), Some(&expected));
        assert!(lb.reads.is_empty());
        let app = analyze(&base.join("app"));
        assert_eq!(app.reads, [expected]);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn producers_run_before_consumers() {
        let base = std::env::temp_dir().join(format!("gestalt-order-b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // Chain: a-app reads m-lb, which reads z-network. Alphabetical order is
        // the reverse of the run order. x-other is independent.
        module(&base, "z-network", "net.tfstate", "");
        module(&base, "m-lb", "lb.tfstate", &reads("net.tfstate"));
        module(&base, "a-app", "app.tfstate", &reads("lb.tfstate"));
        module(&base, "x-other", "other.tfstate", &reads("not-in-this-list.tfstate"));

        let paths: Vec<String> = ["a-app", "m-lb", "x-other", "z-network"].map(String::from).to_vec();
        let ordered = order(&base, &paths, &mut HashMap::new());
        let names: Vec<&str> = ordered.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(names, ["x-other", "z-network", "m-lb", "a-app"]);
        assert_eq!(ordered[3].1, ["m-lb"]);
        assert_eq!(ordered[2].1, ["z-network"]);

        // Only within the given list: without z-network, m-lb has no deps here.
        let ordered = order(&base, &["a-app".into(), "m-lb".into()], &mut HashMap::new());
        assert_eq!(ordered.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["m-lb", "a-app"]);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn cycles_and_unparseable_fall_back_to_path_order() {
        let base = std::env::temp_dir().join(format!("gestalt-order-c-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        module(&base, "b", "b.tfstate", &reads("c.tfstate"));
        module(&base, "c", "c.tfstate", &reads("b.tfstate"));
        std::fs::create_dir_all(base.join("a")).unwrap();
        std::fs::write(base.join("a/main.tf"), "this is { not hcl").unwrap();
        let ordered = order(&base, &["a".into(), "b".into(), "c".into()], &mut HashMap::new());
        assert_eq!(ordered.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        std::fs::remove_dir_all(&base).unwrap();
    }

    fn analyze_tf(name: &str, tf: &str) -> ModuleRefs {
        let d = std::env::temp_dir().join(format!("gestalt-order-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("main.tf"), tf).unwrap();
        let refs = analyze(&d);
        std::fs::remove_dir_all(&d).unwrap();
        refs
    }

    fn id(backend: &str, bucket: &str, key: &str) -> StateId {
        StateId { backend: backend.into(), bucket: bucket.into(), key: key.into() }
    }

    #[test]
    fn backend_kinds_and_variable_defaults() {
        let refs = analyze_tf(
            "kinds",
            r#"
variable "bucket" { default = "from-default" }
variable { default = "no label: ignored" }
terraform {
  backend "gcs" {
    bucket = var.bucket
    prefix = "p"
  }
}
data "terraform_remote_state" "gcs_no_prefix" {
  backend = "gcs"
  config  = { bucket = "b" }
}
data "terraform_remote_state" "local_default" {
  backend = "local"
  config  = {}
}
data "terraform_remote_state" "local_path" {
  backend = "local"
  config  = { path = "../x/terraform.tfstate" }
}
data "terraform_remote_state" "unsupported" {
  backend = "http"
  config  = { address = "https://example.com" }
}
data "terraform_remote_state" "s3_numeric_key" {
  backend = "s3"
  config  = { bucket = "b", key = 5 }
}
data "terraform_remote_state" "unknown_backend_expr" {
  backend = var.missing
  config  = { bucket = "b", key = "k" }
}
data "terraform_remote_state" "config_not_an_object" {
  backend = "s3"
  config  = "nope"
}
"#,
        );
        assert_eq!(refs.state, Some(id("gcs", "from-default", "p")));
        assert_eq!(refs.reads, [id("gcs", "b", ""), id("local", "", "terraform.tfstate"), id("local", "", "../x/terraform.tfstate")]);
    }

    #[test]
    fn unlabelled_backend_is_ignored() {
        let refs = analyze_tf("nolabel", "terraform {\n  backend {\n    path = \"x\"\n  }\n}\n");
        assert_eq!(refs, ModuleRefs::default());
    }
}

#[cfg(test)]
#[path = "probes/order.rs"]
mod probe;
