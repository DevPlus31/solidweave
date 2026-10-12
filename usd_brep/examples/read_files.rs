//! Parse every .usda file given on the command line and summarize what was read.
use usd_brep::{BrepArray, usda};

fn main() {
    let (mut ok, mut failed, mut breps) = (0, 0, 0);
    for path in std::env::args().skip(1) {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => { println!("SKIP {path}: {e}"); continue; }
        };
        match usda::parse(&text) {
            Ok(layer) => {
                ok += 1;
                for p in layer.all_prims().into_iter().filter(|p| p.type_name.as_deref() == Some("BrepArray")) {
                    breps += 1;
                    let b = BrepArray::from_prim(p);
                    if !b.read_issues.is_empty() {
                        println!("ISSUES {path} {}: {:?}", b.path, b.read_issues);
                    }
                }
            }
            Err(e) => { failed += 1; println!("FAIL {path}: {e}"); }
        }
    }
    println!("parsed {ok} files, {failed} failed, {breps} BrepArray prims");
}
