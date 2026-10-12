//! Tests against unmodified sample files from NVIDIA's usd-brep repository.

use usd_brep::convert::{ImportError, to_mini_brep};
use usd_brep::validate::{is_valid, validate};
use usd_brep::BrepArray;

fn load(name: &str) -> BrepArray {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap();
    let mut breps = BrepArray::read_all(&text).unwrap();
    assert_eq!(breps.len(), 1, "{name}");
    breps.remove(0)
}

fn codes(b: &BrepArray) -> Vec<&'static str> {
    validate(b).into_iter().map(|i| i.code).collect()
}

#[test]
fn nvidia_cube_is_valid_and_imports_as_unit_cube() {
    let b = load("Cube.usda");
    let issues = validate(&b);
    assert!(issues.is_empty(), "{issues:#?}");
    let solid = to_mini_brep(&b, 0).unwrap();
    assert_eq!(solid.counts(), (8, 12, 6));
    assert!((solid.volume() - 1.0).abs() < 1e-12);
    assert!((solid.surface_area() - 6.0).abs() < 1e-12);
}

#[test]
fn valid_models_outside_mini_brep_are_rejected_cleanly() {
    let b = load("nonManifoldCubes.usda");
    assert!(is_valid(&validate(&b)));
    assert!(matches!(to_mini_brep(&b, 0), Err(ImportError::Unsupported(_))));
    let b = load("cubeWithVoid.usda");
    assert!(is_valid(&validate(&b)));
    assert!(matches!(to_mini_brep(&b, 0), Err(ImportError::Unsupported(_))));
}

#[test]
fn defects_are_reported_with_nvidia_codes() {
    for (file, code) in [
        ("Test_BA_120_Wrong_face_array_size.usda", "BA_120"),
        ("Test_BA_580_Faceuse_pairing_violation.usda", "BA_580"),
        ("Test_BA_581_Broken_radial_edgeuse_chain.usda", "BA_581"),
        ("Test_BA_763_UV_trim_curves_do_not_close_loop.usda", "BA_763"),
    ] {
        let found = codes(&load(file));
        assert!(found.contains(&code), "{file}: expected {code}, got {found:?}");
    }
}

#[test]
fn nvidia_cube_round_trips_through_the_writer() {
    let b = load("Cube.usda");
    let again = BrepArray::read_all(&b.to_usda()).unwrap().remove(0);
    assert_eq!(again.edgeuse_next_radial, b.edgeuse_next_radial);
    assert_eq!(again.surface_nurbs, b.surface_nurbs);
    assert_eq!(again.vertex_points, b.vertex_points);
    assert!(validate(&again).is_empty());
}
