use mini_brep::{Brep, Curve, Surface, Vec3};

fn main() {
    let b = Brep::make_box(Vec3::new(0.0, 0.0, 0.0), 20.0, 10.0, 5.0);

    let (v, e, f) = b.counts();
    println!("Box 20 x 10 x 5");
    println!("  vertices {v}, edges {e}, faces {f}");
    println!("  valid: {:?}", b.validate());
    println!("  volume: {}", b.volume());
    println!("  area:   {}", b.surface_area());

    println!("\nFaces:");
    for (i, face) in b.faces.iter().enumerate() {
        let Surface::Plane { normal, .. } = face.surface;
        let edges: Vec<usize> = face.outer.coedges.iter().map(|c| c.edge.0).collect();
        println!("  F{i}: normal ({:>2}, {:>2}, {:>2})  edges {:?}", normal.x, normal.y, normal.z, edges);
    }

    let Curve::Line { origin, dir } = &b.edges[0].curve;
    println!("\nEdge 0 lies on line origin={origin:?} dir={dir:?}");

    std::fs::write("box.stl", b.to_stl("box")).unwrap();
    println!("\nWrote box.stl ({} triangles)", b.tessellate().len());
}
