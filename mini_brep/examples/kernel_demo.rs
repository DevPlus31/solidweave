use mini_brep::{Brep, Profile, Ring, Surface, Vec3};

fn report(name: &str, b: &Brep) {
    let (v, e, f) = b.counts();
    println!("{name}");
    println!("  vertices {v}, edges {e}, faces {f}, genus {:?}", b.genus());
    println!("  valid: {:?}", b.validate());
    println!("  volume: {:.6}", b.volume());
    println!("  area:   {:.6}", b.surface_area());
}

fn main() {
    let b = Brep::make_box(Vec3::new(0.0, 0.0, 0.0), 20.0, 10.0, 5.0);
    report("Box 20 x 10 x 5", &b);

    println!("\nFaces:");
    for (i, face) in b.faces.iter().enumerate() {
        if let Surface::Plane { normal, .. } = face.surface {
            let edges: Vec<usize> = face.outer.coedges.iter().map(|c| c.edge.0).collect();
            println!("  F{i}: normal ({:>2}, {:>2}, {:>2})  edges {:?}", normal.x, normal.y, normal.z, edges);
        }
    }

    let cyl = Brep::make_cylinder(Vec3::new(0.0, 0.0, 0.0), 5.0, 10.0);
    println!();
    report("Cylinder r 5, h 10 (exact volume 250π = 785.398163)", &cyl);

    let outer = Ring::Polygon(vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(20.0, 0.0, 0.0),
        Vec3::new(20.0, 10.0, 0.0),
        Vec3::new(0.0, 10.0, 0.0),
    ]);
    let profile = Profile { outer, holes: vec![Ring::Circle { center: Vec3::new(10.0, 5.0, 0.0), radius: 3.0 }] };
    println!("\nprofile check: {:?}", profile.validate());
    let plate = Brep::extrude(&profile, 5.0);
    println!();
    report("Plate 20 x 10 x 5 with a hole of radius 3", &plate);

    std::fs::write("box.stl", b.to_stl("box")).unwrap();
    std::fs::write("plate.stl", plate.to_stl("plate")).unwrap();
    println!("\nWrote box.stl ({} triangles), plate.stl ({} triangles)", b.tessellate().len(), plate.tessellate().len());
}
