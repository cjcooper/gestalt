//! Manual probe against real data; not part of the coverage target.

use super::*;

/// Manual check over a real repo: how many modules' state locations resolve
/// and how many remote-state reads match a module in the repo.
/// `GESTALT_ORDER_DIR=/path/to/repo cargo test print_real_order -- --ignored --nocapture`
#[test]
#[ignore]
fn print_real_order() {
    let root = PathBuf::from(std::env::var("GESTALT_ORDER_DIR").expect("set GESTALT_ORDER_DIR"));
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        if d.join("main.tf").is_file() {
            out.push(d.to_path_buf());
        }
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let n = e.file_name();
            if e.path().is_dir() && !n.to_string_lossy().starts_with('.') {
                walk(&e.path(), out);
            }
        }
    }
    let mut mods = Vec::new();
    walk(&root, &mut mods);
    let start = std::time::Instant::now();
    let refs: Vec<(PathBuf, ModuleRefs)> = mods.iter().map(|m| (m.clone(), analyze(m))).collect();
    let producers: HashMap<&StateId, &PathBuf> = refs.iter().filter_map(|(p, r)| r.state.as_ref().map(|s| (s, p))).collect();
    let with_state = refs.iter().filter(|(_, r)| r.state.is_some()).count();
    let reads: Vec<(&PathBuf, &StateId)> = refs.iter().flat_map(|(p, r)| r.reads.iter().map(move |s| (p, s))).collect();
    let matched = reads.iter().filter(|(_, s)| producers.contains_key(s)).count();
    println!("{} modules analysed in {:?}; state resolved for {}", mods.len(), start.elapsed(), with_state);
    println!("{} remote-state reads; {} matched a module in the repo", reads.len(), matched);
    for (p, r) in refs.iter().filter(|(_, r)| r.state.is_none()).take(5) {
        println!("  no state: {} ({:?})", p.strip_prefix(&root).unwrap().display(), r);
    }
    for (p, s) in reads.iter().filter(|(_, s)| !producers.contains_key(s)).take(8) {
        println!("  unmatched read in {}: {}/{}", p.strip_prefix(&root).unwrap().display(), s.bucket, s.key);
    }
}
