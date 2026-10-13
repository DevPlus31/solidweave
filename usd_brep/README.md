# usd_brep

Pure-Rust support for **NVIDIA's OpenUSD B-rep format** (the `BrepArray` prim
from [NVIDIA-Omniverse/usd-brep](https://github.com/NVIDIA-Omniverse/usd-brep)),
with no C++ or OpenUSD dependency.

| Module | What it does |
|---|---|
| `usda` | Reads the USDA text format (prims, metadata, variants, relationships, dictionaries) and keeps what B-rep work needs |
| `model` | The `omniSolid` schema as Rust types: all topology arrays, all 12 geometry APIs, typed tokens, per-Brep ranges |
| `validate` | Structural checks modelled on NVIDIA's `brep_validator.py` and reimplemented in Rust, with the same `BA_xxx` codes and tolerances |
| `convert` | Export `mini_brep` solids to BrepArray (exact planes, cylinders, cones, spheres, tori, lines and circles), and import planar BrepArrays back |

## Usage

```rust
use usd_brep::{BrepArray, convert, validate};

// Read every BrepArray in a file
let breps = BrepArray::read_all(&std::fs::read_to_string("Cube.usda")?)?;
for issue in validate::validate(&breps[0]) {
    println!("{issue}");          // e.g. "FAIL BA_581: radial cycle from edgeuse 3 ..."
}

// mini_brep solid -> .usda
let solid = mini_brep::Brep::make_box(mini_brep::Vec3::new(0.0, 0.0, 0.0), 20.0, 10.0, 5.0);
let b = convert::from_mini_brep(&solid, "Box", &convert::ExportOptions::default());
std::fs::write("Box.usda", b.to_usda())?;

// planar BrepArray -> mini_brep solid
let back = convert::to_mini_brep(&b, 0)?;
assert!((back.volume() - 1000.0).abs() < 1e-9);
```

```
cargo test
cargo run --example demo -- out/            # export samples, then optionally import files
cargo run --example validate_files -- *.usda
```

## How it was checked

All results come from NVIDIA's own repository and tools, 2026-10-13. The
first four rows are reproduced by `tools/compare_with_nvidia/run.sh`; the
SMLib rows need a built SMLib (`smlib_import.py`, `brep_geometry_validator.exe`).

| Check | Result |
|---|---|
| Parse every `.usda` file in the repo | 137 / 137 files, 132 BrepArray prims |
| Read → write → read gives identical data | 130 / 130 (the 2 others are deliberately unreadable test files) |
| Same pass/fail verdict as `brep_validator.py` | 132 / 132 files |
| Reports the defect each `Test_BA_xxx` file was built to trigger | 107 / 107 |
| `demo` exports pass NVIDIA's validator | 11 / 11 (box, L-bracket, hex prism, cylinder, plate with holes, cone, sphere, torus, turned part, machined block, diamond cut) |
| Exports load into NVIDIA's SMLib kernel as manifold solids | 11 / 11, volumes within 1.7e-8 of exact |
| SMLib's full geometry checks (`AssertValid`, level 2, no healing) | 11 / 11, 0 checks failed |
| NVIDIA's `Cube.usda` imports into `mini_brep` | V8 E12 F6, volume 1, area 6 |

The two validators list exactly the same codes on 86 of the 132 files. On the
rest, which are broken test files, they report the same main defect but
different secondary errors that follow from it.

## Format notes

* **Radial edge structure.** Each face has two faceuses, one per side. Edgeuses
  around an edge form a cycle through `edgeuse:nextRadialEUIndex`, so an edge can
  be shared by any number of faces (non-manifold models are allowed).
* **Regions.** Region 0 of each Brep is the infinite void. A simple solid is
  `[voidRegion, solidRegion]`, with one shell each.
* **Indices are global.** Each Brep owns a consecutive range of every array;
  `BrepArray::spans()` computes the ranges the same way NVIDIA's validator does.
* **Geometry is packed per type.** The k-th plane face uses plane k, and so on
  (`face_surface_slots()`, `edge_curve_slots()`).
* **NURBS surface control points** are stored `u` major: `index = u * vCount + v`.
* **Export conventions** match NVIDIA's OpenCASCADE importer
  (`OcctStagingDriver.cpp`, `BrepTopologyBuilder.cpp`) and SMLib's own exports:
  * The void shell uses a face's `same` side when the surface normal points
    out of the solid, `opposite` when it points in (a round hole's wall).
  * Loops run counter-clockwise around the surface normal, holes clockwise.
  * An edgeuse with orientation `same` gets `bottomEntry`, and `opposite` gets `topEntry`.
  * A full circle is one edge from a vertex back to itself; a closed surface
    is cut open by a seam edge its face uses twice.
  * A face covering its whole (u, v) rectangle (full turn, sphere, torus) is
    written `rectangular`, with no UV curves.
  * Cone semiAngle is kept in (0, π/2): NVIDIA's validator requires that
    (BA_515), although the schema allows (−π/2, π/2).

## Limitations

* **Validation is structural, like NVIDIA's Python tool.** Passing it does not
  prove the geometry is a valid solid; that needs a kernel. Two extra warnings
  (`RS_001` loop chaining, `RS_002` vertex/curve agreement) cover part of the gap.
* **Text format only.** Binary `.usdc` is not supported. NVIDIA's asset
  validator suggests `.usdc` for large arrays; `usdcat` from OpenUSD can convert.
* **`to_mini_brep` handles planar, single-solid, manifold models only.** Curved
  faces, internal voids and non-manifold models return
  `ImportError::Unsupported`.
* **`from_mini_brep` writes one solid region with one shell.** A solid with
  several shells (separate pieces, or a hollow inside) is not exported
  correctly yet.
* **SMLib is used from Python only**, as a reference to check against; Rust
  does not call it.

## License and attribution

Not affiliated with or endorsed by NVIDIA. The format is NVIDIA's BrepArray
schema, implemented here in Rust. `validate.rs` is derived from NVIDIA's
`brep_validator.py` (Apache-2.0) and reimplemented in Rust. The files in
`tests/fixtures/` are unmodified copies from NVIDIA's repository under
Apache-2.0 (see `tests/fixtures/NOTICE.md`). The project's `NOTICE` file
lists exactly what comes from usd-brep and how it was changed.
