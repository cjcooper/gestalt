//! Read a root module's configuration to drive the import form: declared
//! variables (and where each would get a value) and resource addresses.

use crate::shell_env::EnvMap;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VariableInfo {
    pub name: String,
    pub type_expr: Option<String>,
    pub description: Option<String>,
    /// Rendered default, or `None` if the variable has no default.
    /// Hidden for sensitive variables.
    pub default: Option<String>,
    pub has_default: bool,
    pub sensitive: bool,
    /// Where a value would come from if left blank: a `TF_VAR_*` variable or
    /// a tfvars file tofu loads automatically. Values are never exposed.
    pub provided_by: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfo {
    /// e.g. `aws_route53_record.www`
    pub address: String,
    /// "count" or "for_each" when the address needs an index/key.
    pub repeated: Option<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleInfo {
    pub variables: Vec<VariableInfo>,
    pub resources: Vec<ResourceInfo>,
    /// `*.tfvars` / `*.tfvars.json` files in the folder, for `-var-file`.
    pub var_files: Vec<String>,
    /// Files that failed to parse (name: error); the rest are still used.
    pub parse_errors: Vec<String>,
}

fn has_ext(name: &str, exts: &[&str]) -> bool {
    exts.iter().any(|e| name.ends_with(e))
}

/// tfvars files tofu loads without being asked.
fn is_auto_tfvars(name: &str) -> bool {
    name == "terraform.tfvars"
        || name == "terraform.tfvars.json"
        || name.ends_with(".auto.tfvars")
        || name.ends_with(".auto.tfvars.json")
}

fn string_attr(body: &hcl::Body, key: &str) -> Option<String> {
    body.attributes().find(|a| a.key() == key).map(|a| match a.expr() {
        hcl::Expression::String(s) => s.clone(),
        e => e.to_string(),
    })
}

fn names_in_tfvars(content: &str, json: bool) -> Vec<String> {
    if json {
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(content)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    } else {
        hcl::parse(content)
            .map(|b| b.attributes().map(|a| a.key().to_string()).collect())
            .unwrap_or_default()
    }
}

pub fn read(dir: &Path, env: &EnvMap) -> Result<ModuleInfo, String> {
    let mut files: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .filter(|e| std::fs::metadata(e.path()).is_ok_and(|m| m.is_file()))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    files.sort();

    let mut variables = Vec::new();
    let mut resources = BTreeMap::new();
    let mut parse_errors = Vec::new();
    // var name -> tfvars file that sets it (later files win, as in tofu).
    let mut tfvars_source: BTreeMap<String, String> = BTreeMap::new();
    let mut var_files = BTreeSet::new();

    for (name, path) in &files {
        if has_ext(name, &[".tfvars", ".tfvars.json"]) {
            var_files.insert(name.clone());
            if is_auto_tfvars(name) {
                let content = std::fs::read_to_string(path).unwrap_or_default();
                for var in names_in_tfvars(&content, name.ends_with(".json")) {
                    tfvars_source.insert(var, name.clone());
                }
            }
            continue;
        }
        if !has_ext(name, &[".tf", ".tofu"]) {
            continue;
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                parse_errors.push(format!("{name}: {e}"));
                continue;
            }
        };
        let body = match hcl::parse(&content) {
            Ok(b) => b,
            Err(e) => {
                parse_errors.push(format!("{name}: {e}"));
                continue;
            }
        };
        for block in body.blocks() {
            let labels: Vec<&str> = block.labels().iter().map(|l| l.as_str()).collect();
            match (block.identifier(), labels.as_slice()) {
                ("variable", [var]) => {
                    let b = block.body();
                    let sensitive = b
                        .attributes()
                        .any(|a| a.key() == "sensitive" && matches!(a.expr(), hcl::Expression::Bool(true)));
                    let default = b.attributes().find(|a| a.key() == "default").map(|a| a.expr().to_string());
                    variables.push(VariableInfo {
                        name: var.to_string(),
                        type_expr: b.attributes().find(|a| a.key() == "type").map(|a| a.expr().to_string()),
                        description: string_attr(b, "description"),
                        has_default: default.is_some(),
                        default: if sensitive { None } else { default },
                        sensitive,
                        provided_by: None,
                    });
                }
                ("resource", [ty, res]) => {
                    let b = block.body();
                    let repeated = if b.attributes().any(|a| a.key() == "for_each") {
                        Some("for_each")
                    } else if b.attributes().any(|a| a.key() == "count") {
                        Some("count")
                    } else {
                        None
                    };
                    let address = format!("{ty}.{res}");
                    resources.insert(address.clone(), ResourceInfo { address, repeated });
                }
                _ => {}
            }
        }
    }

    // Precedence in tofu: TF_VAR_* < auto tfvars files. Report the winner.
    for v in &mut variables {
        v.provided_by = tfvars_source.get(&v.name).cloned().or_else(|| {
            let key = format!("TF_VAR_{}", v.name);
            env.contains_key(OsStr::new(&key)).then(|| format!("{key} (environment)"))
        });
    }
    variables.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(ModuleInfo {
        variables,
        resources: resources.into_values().collect(),
        var_files: var_files.into_iter().collect(),
        parse_errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn reads_variables_resources_and_sources() {
        let dir = std::env::temp_dir().join(format!("gestalt-modinfo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("main.tf"),
            r#"
variable "region" {
  type = string
}
variable "zone_name" {
  default     = "example.com"
  description = "DNS zone"
}
variable "token" {
  type      = string
  default   = "s3cret"
  sensitive = true
}
variable "owner" {}
resource "aws_route53_record" "www" {
  name = "www"
}
resource "aws_s3_bucket" "logs" {
  for_each = toset(["a"])
}
data "aws_caller_identity" "me" {}
"#,
        )
        .unwrap();
        std::fs::write(dir.join("global.auto.tfvars"), "region = \"us-east-1\"\n").unwrap();
        std::fs::write(dir.join("extra.tfvars"), "owner = \"x\"\n").unwrap();
        std::fs::write(dir.join("broken.tf"), "resource {").unwrap();

        let env: EnvMap = [(OsString::from("TF_VAR_owner"), OsString::from("me"))].into_iter().collect();
        let info = read(&dir, &env).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        let v = |n: &str| info.variables.iter().find(|v| v.name == n).unwrap();
        assert_eq!(v("region").provided_by.as_deref(), Some("global.auto.tfvars"));
        assert_eq!(v("region").type_expr.as_deref(), Some("string"));
        assert!(!v("region").has_default);
        assert_eq!(v("zone_name").default.as_deref(), Some("\"example.com\""));
        assert_eq!(v("zone_name").description.as_deref(), Some("DNS zone"));
        assert!(v("token").sensitive && v("token").has_default && v("token").default.is_none());
        // extra.tfvars isn't auto-loaded, so the env var is what applies.
        assert_eq!(v("owner").provided_by.as_deref(), Some("TF_VAR_owner (environment)"));

        assert_eq!(
            info.resources,
            vec![
                ResourceInfo { address: "aws_route53_record.www".into(), repeated: None },
                ResourceInfo { address: "aws_s3_bucket.logs".into(), repeated: Some("for_each") },
            ]
        );
        assert_eq!(info.var_files, ["extra.tfvars", "global.auto.tfvars"]);
        assert_eq!(info.parse_errors.len(), 1);
        assert!(info.parse_errors[0].starts_with("broken.tf"));
    }
}

#[cfg(test)]
#[path = "probes/module_info.rs"]
mod probe;
