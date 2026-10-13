//! A small parametric "tower" definition, built the way a UI or an AI would:
//! through add_node / connect / set_number only.

use node_graph::*;

fn show(g: &Graph, label: &str, sum: NodeId) {
    println!("── {label} ──");
    print!("{}", g.to_text());
    let total: Vec<String> = g.output(sum, 0).iter().map(|v| v.to_string()).collect();
    println!("total volume: {:?}   (nodes recomputed: {})\n", total, g.last_solve_count);
}

fn main() {
    let mut g = Graph::new();

    // Inputs
    let radius = g.add_node(slider(5.0, 0.0, 20.0));
    let sides = g.add_node(slider(6.0, 3.0, 12.0));
    let floors = g.add_node(slider(3.0, 1.0, 50.0));
    let floor_h = g.add_node(slider(3.0, 1.0, 6.0));

    // Logic
    let profile = g.add_node(Box::new(Polygon));
    let heights = g.add_node(Box::new(Series));
    let solids = g.add_node(Box::new(Extrude));
    let props = g.add_node(Box::new(Volume));
    let total = g.add_node(Box::new(MassAddition));
    let export = g.add_node(Box::new(UsdExport));

    // Wiring
    g.connect(radius, 0, profile, 0).unwrap();
    g.connect(sides, 0, profile, 1).unwrap();
    g.connect(floor_h, 0, heights, 0).unwrap(); // start = one floor
    g.connect(floor_h, 0, heights, 1).unwrap(); // step  = one floor
    g.connect(floors, 0, heights, 2).unwrap(); // count = floors
    g.connect(profile, 0, solids, 0).unwrap();
    g.connect(heights, 0, solids, 1).unwrap();
    g.connect(solids, 0, props, 0).unwrap();
    g.connect(props, 0, total, 0).unwrap();
    g.connect(solids, 0, export, 0).unwrap();

    g.evaluate();
    show(&g, "initial", total);

    g.set_number(floors, 5.0).unwrap();
    g.evaluate();
    show(&g, "floors 3 → 5 (polygon is cached)", total);

    // The graph only produces the .usda text; writing the file is the caller's job.
    let path = std::env::args().nth(1).unwrap_or_else(|| "tower.usda".into());
    match g.output(export, 0).first().map(Value::as_text) {
        Some(Ok(text)) => {
            std::fs::write(&path, text).unwrap();
            println!("wrote {} solids to {path}\n", g.output(solids, 0).len());
        }
        _ => println!("export failed: {:?}\n", g.error(export)),
    }

    // A mistake an AI might make: wiring a solid into a number port.
    if let Err(e) = g.connect(solids, 0, heights, 0) {
        println!("rejected edit: {e}\n");
    }

    // Out-of-range input: the slider clamps it, so the graph stays valid.
    g.set_number(sides, 2.0).unwrap();
    g.evaluate();
    println!("sides set to 2 → clamped by slider range, solids: {}", g.output(solids, 0).len());

    println!("\nNode library (what an AI would be shown):");
    for n in library() {
        let ins: Vec<_> = n.inputs().iter().map(|p| format!("{}:{:?}", p.name, p.kind)).collect();
        let outs: Vec<_> = n.outputs().iter().map(|p| format!("{}:{:?}", p.name, p.kind)).collect();
        println!("  {:<10} ({}) -> ({})  — {}", n.name(), ins.join(", "), outs.join(", "), n.description());
    }
}
