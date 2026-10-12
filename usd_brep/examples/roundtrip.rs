//! Read → write → read every BrepArray and check nothing changed.
use usd_brep::BrepArray;

fn strip(mut b: BrepArray) -> BrepArray {
    b.authored.clear();
    b.read_issues.clear();
    b.path.clear();
    b.api_schemas.clear();
    b
}

fn main() {
    let (mut same, mut diff, mut skipped) = (0, 0, 0);
    for path in std::env::args().skip(1) {
        let text = std::fs::read_to_string(&path).unwrap();
        for b in BrepArray::read_all(&text).unwrap() {
            if !b.read_issues.is_empty() { skipped += 1; continue; }
            let again = BrepArray::read_all(&b.to_usda()).unwrap().remove(0);
            assert!(again.read_issues.is_empty(), "{path}: {:?}", again.read_issues);
            let expect = b.clone();
            if strip(expect) == strip(again.clone()) || format!("{:?}", strip(b.clone())) == format!("{:?}", strip(again.clone())) { same += 1 } else { diff += 1; println!("DIFF {path} {}", b.path) }
        }
    }
    println!("round-trip identical: {same}, different: {diff}, skipped (bad input): {skipped}");
}
