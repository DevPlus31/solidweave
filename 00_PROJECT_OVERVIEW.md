# Rust CAD with AI: project overview

Goal: a parametric 3D CAD system in Rust, built around a Grasshopper-style node
graph that an AI can read and edit, with NVIDIA's OpenUSD B-rep format as the
exchange format.

## File layout

The repository is a normal Cargo workspace (`mini_brep/`, `node_graph/`,
`usd_brep/`); run `cargo test` at the root. `flatten.ps1` writes a flat copy to
`flat/` for a claude.ai Project knowledge base, where `usd_brep__src__model.rs`
is `usd_brep/src/model.rs`.

## The three crates

```
node_graph  ──uses──►  mini_brep  ◄──converts──  usd_brep
(visual program)       (B-rep kernel)            (NVIDIA BrepArray I/O)
```

### mini_brep (~400 lines, no dependencies)
A small B-rep kernel.
- Geometry: `Vec3`, `Curve::Line`, `Surface::Plane`. The enums leave room for
  circles, cylinders and NURBS.
- Topology: Vertex → Edge → CoEdge → Loop → Face → Shell, stored in flat arenas
  with typed ids. No `Rc<RefCell>`.
- A CoEdge is a face's directional use of an edge. In a closed solid, every edge
  is used once forward and once reversed.
- Builders: `make_box`, `extrude_polygon`, `extrude_profile` (outer loop plus
  holes, giving through-holes; concave loops work; loop direction is normalised).
- `validate()` checks closed loops, planarity, edge use (twice, opposite
  directions) and Euler–Poincaré per shell, V − E + F − H = 2 − 2·genus (must
  be even and ≤ 2). `genus()` / `shell_genus()` report it. Euler cannot tell a
  real through-hole from a defect with the same counts.
- `volume()` (divergence theorem), `surface_area()` (both hole-aware),
  `tessellate()` (ear clipping with hole bridges, outward winding), `to_stl()`.

### node_graph (~600 lines)
A Grasshopper-style dataflow engine on top of mini_brep.
- Every port carries a list. Nodes define `solve` for one item, and the engine
  applies longest-list matching. List nodes override `solve_lists`.
- Nodes: Slider, Series, MassAdd, Rectangle, Polygon, Extrude, Volume, UsdExport.
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
- Missing: data trees (nested lists), several wires into one input, a visual
  canvas, a parser from text back to a graph.

### usd_brep (~2,900 lines, depends on mini_brep only)
Pure-Rust support for NVIDIA's `BrepArray` prim
(github.com/NVIDIA-Omniverse/usd-brep, Apache-2.0, released 2026-10-08).
- `usda`: USDA text reader, plus writing helpers.
- `model`: the full omniSolid schema as Rust types. One `attribute_table!`
  drives reading, writing and type checks.
- `validate`: a port of NVIDIA's `brep_validator.py` using the same `BA_xxx`
  codes and tolerances, plus warnings `RS_001` and `RS_002`.
- `convert`: `from_mini_brep` (exact planes, lines, UV trim curves) and
  `to_mini_brep` (planar, single solid, manifold models only).

Verified against NVIDIA's repository:

| Check | Result |
|---|---|
| Files parsed | 137 / 137 |
| Exact read → write → read round-trips | 130 / 130 |
| Same pass/fail verdict as the Python validator | 132 / 132 |
| Defect each test file targets is reported | 107 / 107 |
| Exported solids pass NVIDIA's validator | all |
| `Cube.usda` imported | volume 1 |

Not tested against NVIDIA's C++ kernel (SMLib): it could not be built here.

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
- **Export conventions**, matching NVIDIA's OpenCASCADE importer:
  - Normals point outward.
  - The void shell's faceuses are `same`.
  - Outer loops run counter-clockwise around the normal.
  - An edgeuse with orientation `same` → `bottomEntry`; `opposite` → `topEntry`.
- **Kernel size.** NVIDIA's repo is about 1M lines of C++, about 967k of which
  is the SMLib kernel. Porting it wholesale was rejected. The plan is:
  1. Port the data layer to Rust. Done: this is usd_brep.
  2. Call the kernel through FFI over its 12k-line `SM_API` C-style wrapper,
     behind a Rust `Kernel` trait.
  3. Later, port modules one at a time, testing each against the C++ original.

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
3. mini_brep: holes and genus in the Euler check (done). Still to do: `Circle`
   and `Cylinder` geometry.
4. node_graph: a parser from text back to a graph, so the AI can write whole
   definitions.
5. LLM tool calling over the graph command API, with a validation feedback loop.
6. A visual node canvas (`egui-snarl`) and a `wgpu` viewport.
7. Binary `.usdc` support in usd_brep, or conversion through `usdcat`.
