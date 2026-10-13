//! Writes a set of boolean results as JSON (name, volume, area, V/E/F,
//! shells), so tools/compare_with_nvidia/smlib_boolean.py can run the same
//! operations in NVIDIA's SMLib and compare.
//!
//! The cases are boxes `[x, y, z, dx, dy, dz]`, matching SMLib's
//! create_box(origin, length, width, height).

use mini_brep::{BooleanOp, Brep, Vec3};

fn main() {
    let cube = |b: [f64; 6]| Brep::make_box(Vec3::new(b[0], b[1], b[2]), b[3], b[4], b[5]);
    let cases: [(&str, [f64; 6], [f64; 6], BooleanOp); 6] = [
        ("overlap_union", [0., 0., 0., 2., 2., 2.], [1., 1., 1., 2., 2., 2.], BooleanOp::Union),
        ("overlap_difference", [0., 0., 0., 2., 2., 2.], [1., 1., 1., 2., 2., 2.], BooleanOp::Difference),
        ("overlap_intersection", [0., 0., 0., 2., 2., 2.], [1., 1., 1., 2., 2., 2.], BooleanOp::Intersection),
        ("shared_face_union", [0., 0., 0., 2., 2., 2.], [2., 0., 0., 2., 2., 2.], BooleanOp::Union),
        ("flush_pocket", [0., 0., 0., 4., 4., 4.], [1., 1., 2., 2., 2., 2.], BooleanOp::Difference),
        ("through_hole", [0., 0., 0., 4., 4., 4.], [1., 1., -1., 2., 2., 6.], BooleanOp::Difference),
    ];
    let mut rows = Vec::new();
    for (name, a, b, op) in cases {
        let r = cube(a).boolean(&cube(b), op).expect(name);
        let (v, e, f) = r.counts();
        rows.push(format!(
            "  {{\"name\": \"{name}\", \"a\": {a:?}, \"b\": {b:?}, \"op\": \"{op:?}\", \"volume\": {:.12}, \"area\": {:.12}, \"V\": {v}, \"E\": {e}, \"F\": {f}, \"shells\": {}}}",
            r.volume(),
            r.surface_area(),
            r.shells.len()
        ));
    }
    println!("[\n{}\n]", rows.join(",\n"));
}
