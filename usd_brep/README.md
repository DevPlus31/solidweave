# usd_brep

Pure-Rust support for **NVIDIA's OpenUSD B-rep format** (the `BrepArray` prim
from [NVIDIA-Omniverse/usd-brep](https://github.com/NVIDIA-Omniverse/usd-brep)),
with no C++ or OpenUSD dependency.

| Module | What it does |
|---|---|
| `usda` | Reads the USDA text format (prims, metadata, variants, relationships, dictionaries) and keeps what B-rep work needs |
| `model` | The `omniSolid` schema as Rust types: all topology arrays, all 12 geometry APIs, typed tokens, per-Brep ranges |
| `validate` | Structural checks ported from NVIDIA's `brep_validator.py`, using the same `BA_xxx` codes and tolerances |
| `convert` | Export `mini_brep` solids to BrepArray (exact planes, cylinders, lines and circles), and import planar BrepArrays back |

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

All results come from NVIDIA's own repository and tools
(`tools/compare_with_nvidia/run.sh` reproduces them):

| Check | Result |
|---|---|
| Parse every `.usda` file in the repo | 137 / 137 files, 132 BrepArray prims |
| Read → write → read gives identical data | 130 / 130 (the 2 others are deliberately unreadable test files) |
| Same pass/fail verdict as `brep_validator.py` | 132 / 132 files |
| Reports the defect each `Test_BA_xxx` file was built to trigger | 107 / 107 |
| Exported `mini_brep` solids pass NVIDIA's validator | 5 / 5 (box, L-bracket, hexagonal prism, cylinder, plate with a round and a square hole) |
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
  (`OcctStagingDriver.cpp`, `BrepTopologyBuilder.cpp`):
  * Face normals point outward, so the void shell uses each face's `same` side.
  * Outer loops run counter-clockwise around the normal.
  * An edgeuse with orientation `same` gets `bottomEntry`, and `opposite` gets `topEntry`.

## Limitations

* **Validation is structural, like NVIDIA's Python tool.** Passing it does not
  prove the geometry is a valid solid; that needs a kernel. Two extra warnings
  (`RS_001` loop chaining, `RS_002` vertex/curve agreement) cover part of the gap.
* **Text format only.** Binary `.usdc` is not supported. NVIDIA's asset
  validator suggests `.usdc` for large arrays; `usdcat` from OpenUSD can convert.
* **`to_mini_brep` handles planar, single-solid, manifold models only.** Curved
  faces, internal voids and non-manifold models return
  `ImportError::Unsupported`.
* **Not tested against NVIDIA's C++ kernel (SMLib).** Building it needs NVIDIA's
  package server, which was not reachable from the build machine.

## License

Code in this crate: your choice. The files in `tests/fixtures/` are unmodified
copies from NVIDIA's repository under Apache-2.0 (see `tests/fixtures/NOTICE.md`).
