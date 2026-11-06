//! Manual probe against real data; not part of the coverage target.

use super::*;

/// Manual check against a real repo:
/// `GESTALT_GIT_DIR=/path/to/repo cargo test print_real_log -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn print_real_log() {
    let dir = std::env::var("GESTALT_GIT_DIR").expect("set GESTALT_GIT_DIR");
    let start = std::time::Instant::now();
    let log = first_parent_log(Path::new(&dir), &EnvMap::new(), 0, 200).await.unwrap();
    println!("{} commits in {:?}, remote_has_changes={}", log.commits.len(), start.elapsed(), log.remote_has_changes);
    for c in log.commits.iter().take(12) {
        println!("{} {}", c.short_hash, c.subject);
        for m in &c.modules {
            println!("    {}{}{}", m.path, if m.deleted { "  (deleted)" } else { "" },
                if m.depends_on.is_empty() { String::new() } else { format!("   <- after {}", m.depends_on.join(", ")) });
        }
    }
    let deleted: Vec<_> = log.commits.iter().flat_map(|c| c.modules.iter().filter(|m| m.deleted).map(move |m| (&c.short_hash, &m.path))).collect();
    println!("deleted in page: {deleted:?}");
}
