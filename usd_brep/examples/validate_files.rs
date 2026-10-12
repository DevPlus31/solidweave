//! Validate every BrepArray in the given .usda files.
//! `--json ROOT` prints {relative file: {breps, codes}} for comparison scripts.
use std::collections::BTreeSet;
use usd_brep::{BrepArray, validate::{self, Severity}};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let json_root = if args.first().map(String::as_str) == Some("--json") {
        args.remove(0);
        Some(args.remove(0))
    } else {
        None
    };
    let mut entries = Vec::new();
    for path in &args {
        let text = std::fs::read_to_string(path).unwrap();
        let breps = match BrepArray::read_all(&text) {
            Ok(b) => b,
            Err(e) => { eprintln!("{path}: parse error {e}"); continue; }
        };
        let mut codes = BTreeSet::new();
        for b in &breps {
            for issue in validate::validate(b) {
                if issue.severity == Severity::Failure {
                    codes.insert(issue.code.replace('_', "."));
                }
                if json_root.is_none() {
                    println!("{path} {}: {issue}", b.path);
                }
            }
        }
        if let Some(root) = &json_root {
            let rel = path.strip_prefix(&format!("{root}/")).unwrap_or(path);
            let list: Vec<String> = codes.iter().map(|c| format!("\"{c}\"")).collect();
            entries.push(format!("  \"{rel}\": {{\"breps\": {}, \"codes\": [{}]}}", breps.len(), list.join(", ")));
        } else if breps.iter().all(|b| validate::is_valid(&validate::validate(b))) && !breps.is_empty() {
            println!("{path}: PASS ({} BrepArray)", breps.len());
        }
    }
    if json_root.is_some() {
        println!("{{\n{}\n}}", entries.join(",\n"));
    }
}
