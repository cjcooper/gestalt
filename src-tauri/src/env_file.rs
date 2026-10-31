//! Hierarchical `.gestalt.env` files.
//!
//! For a run in `<root>/a/b`, the files `<root>/.gestalt.env`,
//! `<root>/a/.gestalt.env` and `<root>/a/b/.gestalt.env` are applied in that
//! order on top of the base (app + login shell) environment, so deeper files
//! override shallower ones. Lookup never goes above the opened root.
//!
//! Format (dotenv-like):
//! ```text
//! # comment
//! AWS_PROFILE=dev
//! export TF_VAR_region=us-east-1      # `export ` prefix is optional
//! TF_VAR_name=${TF_VAR_env}-app       # $VAR / ${VAR} expand from values so far
//! PATH="$PATH:/opt/tools/bin"         # double quotes: expansion + \n \t \" \\ \$
//! LITERAL='no $expansion here'        # single quotes: literal
//! unset AWS_SESSION_TOKEN             # remove an inherited variable
//! ```

use crate::shell_env::EnvMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = ".gestalt.env";

/// What one file did, for reporting (names only, never values).
#[derive(Debug, Default, PartialEq)]
pub struct FileEffect {
    pub set: Vec<String>,
    pub unset: Vec<String>,
}

/// `.gestalt.env` files that apply to `dir`, outermost first. `dir` must be
/// `root` or inside it; otherwise only `dir` itself is considered.
pub fn chain(root: Option<&Path>, dir: &Path) -> Vec<PathBuf> {
    let stop = root.filter(|r| dir.starts_with(r));
    let mut files = Vec::new();
    for d in dir.ancestors() {
        let f = d.join(FILE_NAME);
        if f.is_file() {
            files.push(f);
        }
        if stop.is_none_or(|r| d == r) {
            break;
        }
    }
    files.reverse();
    files
}

/// Apply one file's contents to `env` in place.
pub fn apply(content: &str, env: &mut EnvMap) -> Result<FileEffect, String> {
    let mut effect = FileEffect::default();
    for (i, raw) in content.lines().enumerate() {
        let lineno = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let err = |msg: &str| format!("line {lineno}: {msg}");

        if let Some(rest) = line.strip_prefix("unset ") {
            for key in rest.split_whitespace() {
                if key.starts_with('#') {
                    break;
                }
                if !valid_key(key) {
                    return Err(err(&format!("invalid variable name `{key}`")));
                }
                env.remove(&OsString::from(key));
                effect.unset.push(key.to_string());
            }
            continue;
        }

        let line = line.strip_prefix("export ").map(str::trim_start).unwrap_or(line);
        let (key, value) = line.split_once('=').ok_or_else(|| err("expected KEY=value"))?;
        let key = key.trim_end();
        if !valid_key(key) {
            return Err(err(&format!("invalid variable name `{key}`")));
        }
        let value = parse_value(value.trim_start(), env).map_err(|m| err(&m))?;
        env.insert(key.into(), value.into());
        effect.set.push(key.to_string());
    }
    Ok(effect)
}

fn valid_key(k: &str) -> bool {
    let mut chars = k.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse_value(v: &str, env: &EnvMap) -> Result<String, String> {
    if let Some(rest) = v.strip_prefix('\'') {
        let end = rest.find('\'').ok_or("unterminated single quote")?;
        trailing_ok(&rest[end + 1..])?;
        return Ok(rest[..end].to_string());
    }
    if let Some(rest) = v.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    trailing_ok(&rest[i + 1..])?;
                    return Ok(out);
                }
                '\\' => match chars.next().map(|(_, c)| c) {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(c @ ('"' | '\\' | '$')) => out.push(c),
                    Some(c) => {
                        out.push('\\');
                        out.push(c);
                    }
                    None => return Err("unterminated double quote".into()),
                },
                '$' => expand(&mut chars, rest, env, &mut out)?,
                c => out.push(c),
            }
        }
        return Err("unterminated double quote".into());
    }

    // Unquoted: an inline comment starts at whitespace followed by `#`.
    let v = match v.find(" #").or_else(|| v.find("\t#")) {
        Some(i) => &v[..i],
        None => v,
    }
    .trim_end();
    let mut out = String::new();
    let mut chars = v.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c == '$' {
            expand(&mut chars, v, env, &mut out)?;
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

/// Only whitespace or a comment may follow a closing quote.
fn trailing_ok(rest: &str) -> Result<(), String> {
    let rest = rest.trim_start();
    if rest.is_empty() || rest.starts_with('#') {
        Ok(())
    } else {
        Err(format!("unexpected text after closing quote: `{rest}`"))
    }
}

/// Called just after a `$`. Expands `${NAME}` or `$NAME`; a `$` not followed
/// by either is kept literally. Unset variables expand to "" like a shell.
fn expand(
    chars: &mut std::iter::Peekable<std::str::CharIndices>,
    s: &str,
    env: &EnvMap,
    out: &mut String,
) -> Result<(), String> {
    let lookup = |name: &str| {
        env.get(&OsString::from(name)).map(|v| v.to_string_lossy().into_owned()).unwrap_or_default()
    };
    match chars.peek().copied() {
        Some((i, '{')) => {
            chars.next();
            let end = s[i + 1..].find('}').ok_or("unterminated ${")? + i + 1;
            let name = &s[i + 1..end];
            if !valid_key(name) {
                return Err(format!("invalid variable name `${{{name}}}`"));
            }
            while chars.peek().is_some_and(|(j, _)| *j <= end) {
                chars.next();
            }
            out.push_str(&lookup(name));
        }
        Some((i, c)) if c.is_ascii_alphabetic() || c == '_' => {
            let mut end = i;
            while let Some(&(j, c)) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '_' {
                    end = j + c.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            out.push_str(&lookup(&s[i..end]));
        }
        _ => out.push('$'),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> EnvMap {
        pairs.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))).collect()
    }

    fn get(env: &EnvMap, k: &str) -> Option<String> {
        env.get(&OsString::from(k)).map(|v| v.to_string_lossy().into_owned())
    }

    #[test]
    fn parses_values_and_expands() {
        let mut env = env_of(&[("PATH", "/usr/bin"), ("HOME", "/home/me")]);
        let effect = apply(
            r#"
# comment
A=plain value   # trailing comment
export B=${A}-x
C="$HOME/bin:\$literal\t\"q\""
D='$HOME stays'  # ok
PATH=$PATH:/opt/bin
E=
F=price$ 5 and a#hash
"#,
            &mut env,
        )
        .unwrap();
        assert_eq!(get(&env, "A").unwrap(), "plain value");
        assert_eq!(get(&env, "B").unwrap(), "plain value-x");
        assert_eq!(get(&env, "C").unwrap(), "/home/me/bin:$literal\t\"q\"");
        assert_eq!(get(&env, "D").unwrap(), "$HOME stays");
        assert_eq!(get(&env, "PATH").unwrap(), "/usr/bin:/opt/bin");
        assert_eq!(get(&env, "E").unwrap(), "");
        assert_eq!(get(&env, "F").unwrap(), "price$ 5 and a#hash");
        assert_eq!(effect.set, ["A", "B", "C", "D", "PATH", "E", "F"]);
    }

    #[test]
    fn unset_removes_inherited() {
        let mut env = env_of(&[("AWS_SESSION_TOKEN", "x"), ("KEEP", "y")]);
        let effect = apply("unset AWS_SESSION_TOKEN MISSING # gone", &mut env).unwrap();
        assert!(get(&env, "AWS_SESSION_TOKEN").is_none());
        assert_eq!(get(&env, "KEEP").unwrap(), "y");
        assert_eq!(effect.unset, ["AWS_SESSION_TOKEN", "MISSING"]);
    }

    #[test]
    fn later_files_override_earlier() {
        let mut env = EnvMap::new();
        apply("ENV=dev\nREGION=us-east-1\nNAME=${ENV}-app", &mut env).unwrap();
        apply("ENV=prod\nNAME=${ENV}-${REGION}", &mut env).unwrap();
        assert_eq!(get(&env, "ENV").unwrap(), "prod");
        assert_eq!(get(&env, "NAME").unwrap(), "prod-us-east-1");
    }

    #[test]
    fn reports_errors_with_line_numbers() {
        let mut env = EnvMap::new();
        assert_eq!(apply("A=1\nnot a pair", &mut env).unwrap_err(), "line 2: expected KEY=value");
        assert!(apply("1BAD=x", &mut env).unwrap_err().contains("invalid variable name"));
        assert!(apply("A=\"open", &mut env).unwrap_err().contains("unterminated"));
        assert!(apply("A='x' junk", &mut env).unwrap_err().contains("after closing quote"));
        assert!(apply("A=${B", &mut env).unwrap_err().contains("unterminated ${"));
    }

    #[test]
    fn examples_tree_resolves_with_overrides() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples").canonicalize().unwrap();
        let resolve = |rel: &str| {
            let mut env = EnvMap::new();
            for f in chain(Some(&root), &root.join(rel)) {
                apply(&std::fs::read_to_string(f).unwrap(), &mut env).unwrap();
            }
            (get(&env, "TF_VAR_owner").unwrap(), get(&env, "TF_VAR_environment").unwrap(), get(&env, "TF_VAR_label").unwrap())
        };
        assert_eq!(resolve("dev/app"), ("platform-team".into(), "dev".into(), "platform-team/dev".into()));
        assert_eq!(resolve("prod/app"), ("app-team".into(), "prod".into(), "app-team/prod".into()));
    }

    #[test]
    fn chain_stops_at_root() {
        let tmp = std::env::temp_dir().join(format!("gestalt-chain-{}", std::process::id()));
        let leaf = tmp.join("root/a/b");
        std::fs::create_dir_all(&leaf).unwrap();
        for d in [tmp.clone(), tmp.join("root"), leaf.clone()] {
            std::fs::write(d.join(FILE_NAME), "X=1").unwrap();
        }
        let root = tmp.join("root");
        assert_eq!(chain(Some(&root), &leaf), vec![root.join(FILE_NAME), leaf.join(FILE_NAME)]);
        // Outside the root: only the folder itself.
        assert_eq!(chain(Some(&leaf), &root), vec![root.join(FILE_NAME)]);
        assert_eq!(chain(None, &leaf), vec![leaf.join(FILE_NAME)]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
