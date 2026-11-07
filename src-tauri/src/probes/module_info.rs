//! Manual probe against real data; not part of the coverage target.

use super::*;

/// Manual check against a real module:
/// `GESTALT_MODINFO_DIR=/path/to/module cargo test print_real_module -- --ignored --nocapture`
#[test]
#[ignore]
fn print_real_module() {
    let dir = std::env::var("GESTALT_MODINFO_DIR").expect("set GESTALT_MODINFO_DIR");
    let info = read(Path::new(&dir), &EnvMap::new()).unwrap();
    for v in &info.variables {
        println!("var {:<24} default={:<5} provided_by={:?}", v.name, v.has_default, v.provided_by);
    }
    for r in &info.resources {
        println!("res {} {:?}", r.address, r.repeated);
    }
    println!("var_files={:?} parse_errors={:?}", info.var_files, info.parse_errors);
}
