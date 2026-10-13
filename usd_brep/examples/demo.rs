//! End-to-end demo:
//!   1. build solids with mini_brep and export them as BrepArray .usda files
//!   2. read NVIDIA sample files, validate them, and import the planar ones
//!
//! Usage: cargo run --example demo -- <output dir> [NVIDIA .usda files...]

use mini_brep::{Brep, Ring, Vec3};
use usd_brep::convert::{ExportOptions, from_mini_brep, to_mini_brep};
use usd_brep::validate::{is_valid, validate};
use usd_brep::BrepArray;

fn main() {
    let mut args = std::env::args().skip(1);
    let out_dir = args.next().unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&out_dir).unwrap();

    // ── 1. Export ──
    let p = |x, y| Vec3::new(x, y, 0.0);
    let hexagon: Vec<Vec3> = (0..6)
        .map(|k| {
            let a = std::f64::consts::TAU * k as f64 / 6.0;
            Vec3::new(20.0 * a.cos(), 20.0 * a.sin(), 0.0)
        })
        .collect();
    let solids = [
        ("Box", Brep::make_box(Vec3::new(0.0, 0.0, 0.0), 20.0, 10.0, 5.0)),
        ("LBracket", Brep::extrude_polygon(&[p(0.0, 0.0), p(30.0, 0.0), p(30.0, 5.0), p(5.0, 5.0), p(5.0, 25.0), p(0.0, 25.0)], 12.0)),
        ("HexPrism", Brep::extrude_polygon(&hexagon, 8.0)),
        ("Cylinder", Brep::make_cylinder(Vec3::new(0.0, 0.0, 0.0), 10.0, 15.0)),
        ("HolePlate", Brep::extrude(
            &Ring::Polygon(vec![p(0.0, 0.0), p(40.0, 0.0), p(40.0, 20.0), p(0.0, 20.0)]),
            &[Ring::Circle { center: p(12.0, 10.0), radius: 5.0 }, Ring::Polygon(vec![p(25.0, 6.0), p(33.0, 6.0), p(33.0, 14.0), p(25.0, 14.0)])],
            6.0,
        )),
    ];
    println!("Export");
    for (name, solid) in &solids {
        let b = from_mini_brep(solid, name, &ExportOptions::default());
        let path = format!("{out_dir}/{name}.usda");
        std::fs::write(&path, b.to_usda()).unwrap();
        // Validate what is on disk, not the in-memory value.
        let back = BrepArray::read_all(&std::fs::read_to_string(&path).unwrap()).unwrap().remove(0);
        let issues = validate(&back);
        let (v, e, f) = solid.counts();
        println!(
            "  {name:<9} V{v:<3} E{e:<3} F{f:<3} volume {:>8.1}  -> {path}  [{}]",
            solid.volume(),
            if issues.is_empty() { "valid".to_string() } else { format!("{} issues", issues.len()) }
        );
        for i in issues {
            println!("      {i}");
        }
    }

    // ── 2. Read, validate and import NVIDIA files ──
    let files: Vec<String> = args.collect();
    if !files.is_empty() {
        println!("\nImport");
    }
    for path in files {
        let name = path.rsplit('/').next().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for b in BrepArray::read_all(&text).unwrap() {
            let issues = validate(&b);
            let verdict = if is_valid(&issues) { "valid" } else { "INVALID" };
            print!("  {name:<28} {} breps, {} faces, {} edges: {verdict}", b.brep_count(), b.face_loop_count.len(), b.edge_curve_type.len());
            match to_mini_brep(&b, 0) {
                Ok(solid) => {
                    let (v, e, f) = solid.counts();
                    println!(" -> mini_brep V{v} E{e} F{f}, volume {:.4}, area {:.4}", solid.volume(), solid.surface_area());
                }
                Err(err) => println!(" -> not imported ({err})"),
            }
        }
    }
}
