# Solidweave: project overview

Goal: a parametric 3D CAD system in Rust, built around a Grasshopper-style node
graph that an AI can read and edit, with NVIDIA's OpenUSD B-rep format as the
exchange format.

## File layout

The repository is a normal Cargo workspace (`cad_app/`, `mini_brep/`,
`node_graph/`, `usd_brep/`); run `cargo test --workspace` at the root.

## The crates

```
cad_app ──► node_graph ──► mini_brep ◄── usd_brep
(egui UI)   (visual        (B-rep        (NVIDIA BrepArray I/O;
            program)        kernel)       converts mini_brep solids)
                │                            ▲
                └────── exports through ─────┘
```

Only `cad_app` has external dependencies (eframe/egui 0.36, egui-snarl 0.12).
Sizes (src, with tests): mini_brep ~2,700 lines, node_graph ~1,300, usd_brep
~3,200, cad_app ~600.

### cad_app (egui 0.36 / eframe, egui-snarl 0.12)
The desktop editor: `cargo run -p cad_app`.
- Node canvas (egui-snarl) on the left. The `Graph` is the single source of
  truth: every canvas gesture calls the command API, and an edit the graph
  rejects (type mismatch, cycle) never appears; its reason shows in the
  status bar. Sliders edit through `set_number`; right-click adds or removes
  nodes from the registry (`node_graph::create`).
- 3D preview in the middle: every solid on any node output, tessellated,
  drawn with egui's painter (orbit camera, back-face culling, flat shading,
  painter's-algorithm sort). Fine for thousands of triangles; a wgpu depth
  buffer can replace it later.
- The text form (what the AI reads) and status at the bottom.
- Starts with a drilled plate driven by sliders.

### mini_brep
A small B-rep kernel (`lib.rs`, plus `boolean.rs`).
- Geometry: `Vec3`, `Curve::{Line, Circle}`,
  `Surface::{Plane, Cylinder, Cone, Sphere, Torus}`, with the BrepArray
  schema's own parameterisations. All but the plane are surfaces of
  revolution, S = origin + ρ(v)·r̂(u) + h(v)·axis; `Surface::profile(v)` gives
  ρ, ρ′, h, h′, and one code path serves them all. Normals point away from the
  axis (torus: from the tube's centre line).
- `Face::uv_rect`: a face that is its surface's whole (u, v) rectangle (full
  turns, spheres, tori). Its loops only list its edges, since a sphere's
  poles leave the loop open in (u, v); it is exported `rectangular`, as SMLib
  does.
- Topology: Vertex → Edge → CoEdge → Loop → Face → Shell, stored in flat arenas
  with typed ids. No `Rc<RefCell>`. `Face::sense` says whether the surface
  normal points out of the solid (false for a round hole's wall).
- A CoEdge is a face's directional use of an edge. In a closed solid, every edge
  is used once forward and once reversed. A full circle is one closed edge
  (start vertex = end vertex); a cylinder face is closed by a seam edge it uses
  twice, as in NVIDIA's SMLib output.
- `Profile { outer: Ring, holes: Vec<Ring> }`, a `Ring` being a polygon or a
  circle. `Profile::validate()` checks flatness, simple rings, holes strictly
  inside the outline and apart (touching counts as overlap), since a misplaced
  hole still gives valid topology.
- Builders: `extrude(&Profile, h)`; `make_box`, `make_cylinder`,
  `extrude_polygon`, `extrude_profile` build on it. `revolve(&[[r, z]])` spins
  a polygon round Z into discs/rings, cylinders and cones (tips on the axis
  allowed; `check_revolve_profile` rejects crossing the axis, pinches and
  enclosed hollows). `make_sphere` and `make_torus` copy SMLib's topology
  (sphere: 2 poles, 1 half-circle seam; torus: 1 vertex, 2 closed seams).
  Cones always point their axis to the wide end, keeping semiAngle in
  (0, π/2): NVIDIA's validator demands that (BA_515) though its schema
  allows (−π/2, π/2). Low level: `add_vertex`, `add_edge`,
  `add_face`, `add_shell`.
- `face_uv_loops()` gives each face's trim loops in (u, v) ("pcurves"); angles
  jump a full turn across a seam so loops close. Planar and trimmed faces use
  it for mass properties, tessellation and UV trim curves; `uv_rect` faces use
  their rectangle instead.
- `validate()` checks closed loops, edges on their face's surface (sampled),
  vertices on their edge's curve, edge use (twice, opposite directions) and
  Euler–Poincaré per shell, V − E + F − H = 2 − 2·genus (even and ≤ 2).
  `genus()` / `shell_genus()` report it. Euler cannot tell a real through-hole
  from a defect with the same counts.
- `volume()` and `surface_area()`: divergence theorem. Planes via Green's
  theorem round their (u, v) loops; surfaces of revolution in closed form in
  u and 8-point Gauss–Legendre in v: exact for cylinders and cones, to
  rounding for spheres and tori. `bounding_box()` is exact (reaches a
  sphere's equator, on no edge). `tessellate()` ear-clips planar faces and
  grids curved ones at 64 segments per turn; `to_stl()`.
- Limit: trimmed curved faces (no `uv_rect`) must be bounded by meridians and
  parallels, which is all the builders make.
- Booleans (`boolean.rs`): `a.boolean(&b, BooleanOp::{Union, Difference,
  Intersection})` for solids with planar faces. Faces are cut into convex
  polygons and clipped against BSP trees (csg.js's algorithm, reimplemented),
  which resolves coplanar faces; the soup is rebuilt into a B-rep (weld,
  close T-junctions, merge each plane's pieces into faces with holes, drop
  vertices mid-edge, group shells) and validated. Results may be empty or
  have several shells (pieces, inner hollows). Non-manifold results (solids
  touching along an edge) are reported as errors. Curved inputs are refused.
  Welding and T-junction search are O(n²): fine for parts, slow for meshes.

### node_graph
A Grasshopper-style dataflow engine on top of mini_brep (and usd_brep, for
export).
- Every port carries a list. Nodes define `solve` for one item, and the engine
  applies longest-list matching. List nodes override `solve_lists`.
- Nodes: Slider, Series, MassAdd, Rectangle, Polygon, Circle, Move,
  Difference, Extrude, Revolve, Sphere, SolidUnion, SolidDifference,
  SolidIntersection, Volume, UsdExport. Revolve spins a profile drawn in XY
  (x = distance from the axis, y = height) round Z; a circle clear of the
  axis gives a torus. The solid booleans refuse results with more than one
  shell, since BrepArray export writes one shell so far. Profiles travel as
  `Value::Profile` (outline plus holes). `Difference` cuts the whole holes
  list out of each outline and validates the result, so a hole off the
  plate fails there, at the node that caused it.
- Geometry goes through a `Kernel` trait (`kernel.rs`). The graph owns one kernel
  (default `MiniBrep`) and passes it to every solve. Solids on wires are opaque
  `Rc<dyn Solid>` and can measure themselves and export themselves as a
  BrepArray, which is the kernel-neutral exchange form.
- `UsdExport` turns all its input solids into one `.usda` layer, with one
  BrepArray prim per solid, and validates it after reading it back. It returns
  text; the caller writes the file.
- Edits go only through `add_node`, `connect`, `disconnect`, `set_number`,
  `remove_node` and `set_kernel`. These are also the AI's tool calls.
- `connect` rejects type mismatches and cycles. Errors stay local to the node
  that failed, and downstream nodes get empty data.
- Evaluation is incremental: only dirty nodes and their dependents recompute.
- `to_text()` produces the compact form an LLM reads, for example
  `n6 = Extrude(profile=n4.P, height=n5.S)`.
- Missing: data trees (nested lists), several wires into one input, a parser
  from text back to a graph.

### usd_brep
Pure-Rust support for NVIDIA's `BrepArray` prim
(github.com/NVIDIA-Omniverse/usd-brep, Apache-2.0, released 2026-10-08).
- `usda`: USDA text reader, plus writing helpers.
- `model`: the full omniSolid schema as Rust types. One `attribute_table!`
  drives reading, writing and type checks.
- `validate`: modelled on NVIDIA's `brep_validator.py`, reimplemented in Rust,
  with the same `BA_xxx` codes and tolerances, plus warnings `RS_001` (loop
  chaining) and `RS_002` (vertices on line, circle and NURBS edge curves).
- `convert`: `from_mini_brep` writes exact planes, cylinders, cones, spheres,
  tori, lines and circles (conventions below) and UV trim curves for trimmed
  faces (degree 1 for lines, rational quadratic for arcs). `to_mini_brep`
  imports planar, single-solid, manifold models only.

## Verified

All dated 2026-10-13. "Exports" are the 13 files written by the usd_brep
`demo` (box, L-bracket, hex prism, cylinder, plate with round and square
holes, cone, sphere, torus, turned part, machined block, diamond cut) and the
node_graph `graph_demo` (drilled plate, 5-solid tower): 17 solids.

| Check | Result |
|---|---|
| NVIDIA's `.usda` files parsed | 137 / 137 |
| Exact read → write → read round trips | 130 / 130 (the 2 others are deliberately unreadable) |
| Same pass/fail verdict as NVIDIA's Python validator | 132 / 132 files |
| Defect each `Test_BA_xxx` file targets is reported | 107 / 107 |
| NVIDIA's Python validator on our exports | 13 / 13 files pass |
| Exports loaded into NVIDIA's SMLib kernel (`smlib_import.py`) | 17 / 17 manifold solids; volumes within 1.7e-8 of exact where compared (SMLib's own integration error) |
| NVIDIA's `brep_geometry_validator.exe --level 2 --no-healer` (SMLib `AssertValid`) | 17 / 17 solids, 0 checks failed; it does fail NVIDIA's broken `coverage_limits` cylinders (7 checks each) |
| Booleans vs SMLib's own booleans (`smlib_boolean.py`) | 6 / 6 box cases: same volume to 2e-16 and identical V/E/F counts |
| NVIDIA's `Cube.usda` imported into mini_brep | V8 E12 F6, volume 1 |

NVIDIA's Python validator is structural: its own `coverage_limits` files with
broken cylinder seams pass it ("Seam validity needs the kernel"). SMLib does
check seams and closed edges, and accepts ours. SMLib's `area()` and
`volume()` are numerical: pass `relative_accuracy` (area defaults to 1e-3);
even then its area of its own cylinder is 1.2e-5 off, so compare areas loosely.

To reproduce: `usd_brep/tools/compare_with_nvidia/run.sh <usd-brep checkout>`
compares validators (bash; needs `pip install usd-core
"usd-validation-nvidia>=1.22.0,<2"`). `smlib_import.py`, `smlib_boolean.py`
and `brep_geometry_validator.exe` need a built SMLib; see `CLAUDE.md`.

## Key facts about the BrepArray format

- **Radial edge model.** Region 0 of each Brep is the infinite void, and a
  simple solid is `[voidRegion, solidRegion]`. Each face has two faceuses, one
  per side. Edgeuses around an edge form a cycle through `nextRadialEUIndex`,
  so non-manifold models are allowed.
- **Packing.** Arrays are flat and indices are global. Each Brep owns a
  consecutive range of every array. Faces = faceuses / 2.
- **Geometry slots.** Geometry is packed per type in order of use: the k-th
  plane face uses plane k.
- **NURBS surface control points** are u major: `index = u * vCount + v`.
- **Export conventions**, from NVIDIA's OpenCASCADE importer and SMLib's own
  exports:
  - Surface normals follow the schema (curved surfaces: away from the axis).
    The void shell's faceuse is `same` when that normal points out of the
    solid, `opposite` when it points in (a round hole's wall).
  - Loops run counter-clockwise around the surface normal, holes clockwise.
  - An edgeuse with orientation `same` → `bottomEntry`; `opposite` → `topEntry`.
  - A full circle is one edge from a vertex back to itself. A closed surface
    is cut open by a seam edge its face uses twice.
  - A face covering its whole (u, v) rectangle (full turn, sphere, torus) is
    `rectangular`, with no UV curves.
  - Cone semiAngle in (0, π/2): NVIDIA's validator requires it (BA_515)
    although the schema allows (−π/2, π/2).
- **Kernel size.** NVIDIA's repo is about 1M lines of C++, about 967k of which
  is the SMLib kernel. Porting it wholesale was rejected. The plan is:
  1. Port the data layer to Rust. Done: this is usd_brep.
  2. Call the kernel through FFI over its 12k-line `SM_API` C-style wrapper,
     behind a Rust `Kernel` trait. SMLib now builds here (Windows, VS 2022)
     and is driven from Python (`usd_brep` bindings) as a test oracle; FFI
     from Rust is not done.
  3. Later, reimplement modules one at a time. Not line by line: each module
     is designed from the start as idiomatic Rust, with the C++ used only as
     the specification and as a test oracle (same inputs, compare outputs).

## Design principles

- The AI edits the parametric graph or feature tree, never raw geometry.
- One command API serves the UI, scripts and the AI, so every AI action can be undone.
- A textual form of the model is the AI's native interface.
- A closed verification loop: propose → evaluate → validate → feed errors back.
- Show AI changes as reviewable diffs or ghost previews.
- Validity layers: Euler–Poincaré is necessary but not sufficient. It must be
  combined with edge-use, orientation, geometry-on-topology and
  self-intersection checks.

## Roadmap (next steps, roughly in order)

1. Done: a `UsdExport` node in node_graph that writes a graph's solids as BrepArray.
2. Done: a `Kernel` trait in node_graph so backends can be swapped (mini_brep now,
   SMLib through FFI later).
3. Done: mini_brep holes, genus in the Euler check, `Circle` and `Cylinder`;
   profiles with holes and circles in node_graph (`Kernel::extrude` takes a
   `Profile`); revolve, cones, spheres, tori; booleans of planar solids.
   Still to do: booleans with curved faces; multi-shell export; import of
   curved BrepArrays in `to_mini_brep`; revolving less than a full turn.
4. node_graph: a parser from text back to a graph, so the AI can write whole
   definitions.
5. LLM tool calling over the graph command API, with a validation feedback loop.
6. Started: `cad_app` with an egui-snarl node canvas and a painter-based 3D
   preview. Still to do: undo/redo over the command API, save/load, a wgpu
   viewport with a depth buffer, fitting the canvas view on start.
7. Binary `.usdc` support in usd_brep, or conversion through `usdcat`.
