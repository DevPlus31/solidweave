# Rust CAD with AI

A parametric 3D CAD system in Rust, designed to be driven by AI. Three crates:
mini_brep (B-rep kernel), node_graph (Grasshopper-style dataflow engine) and
usd_brep (NVIDIA OpenUSD BrepArray reader/writer/validator). Start from
`00_PROJECT_OVERVIEW.md`, which records the architecture, verified results,
BrepArray format facts and the roadmap.

## Layout and commands

- Cargo workspace at the root: `cad_app/` (egui editor, `cargo run -p cad_app`),
  `mini_brep/`, `node_graph/`, `usd_brep/`. Only `cad_app` has external
  dependencies (eframe/egui 0.36, egui-snarl 0.12); keep the other three
  dependency-free.
- `cargo test --workspace` runs everything, including tests against NVIDIA's
  sample files in `usd_brep/tests/fixtures/` (Apache-2.0, see NOTICE.md).
- Examples: `kernel_demo` (mini_brep), `graph_demo` (node_graph), `demo`
  (usd_brep). Example names must stay unique across the workspace, or their
  binaries overwrite each other in `target/`.
- NVIDIA's Python validator: `usd_brep/tools/compare_with_nvidia/run.sh`.
- NVIDIA's SMLib kernel, as a test oracle: a built checkout lives at
  `C:\Users\Mustapha\nvb`. Check exports with
  `python3 usd_brep/tools/compare_with_nvidia/smlib_import.py C:\Users\Mustapha\nvb FILE.usda…`
  (Python 3.12), booleans with `smlib_boolean.py`, and full SMLib geometry
  checks with `C:\Users\Mustapha\nvb\_build\windows-x86_64\release\brep_geometry_validator.exe --level 2 --no-healer FILE.usda…`
  (put that folder and its `extraLibs` on PATH and set
  `OMNISOLID_PLUGIN_PATH` to `nvb\_build\schema\omniSolid\resources`). To rebuild: in `repo.toml` set `msbuild.vs_version = "vs2022"`
  (this machine has no v142 toolset), then `prebuild.bat` and `repo.bat build -r`.
  The checkout must be at a short path: from a long one, cl.exe's `/MP` child
  command lines pass 32k characters (errors C1083 '' and D8040), and `subst`
  does not help because the build tool resolves real paths.
- `./flatten.ps1` writes flat copies (`usd_brep__src__model.rs`) to `flat/` for
  uploading to the claude.ai Project. Edit the real files, never `flat/`.
- `PROJECT_INSTRUCTIONS.md` is the claude.ai Project's instruction text; it
  describes the flat names because that is what the Project sees.

## How to work

- Write idiomatic, dependency-light Rust (edition 2024). Use arenas with typed ids
  for topology, never `Rc<RefCell>`.
- Keep the existing design rules. The AI edits the graph or feature tree, not raw
  geometry. Every edit goes through the command API. Geometry is validated after
  every change.
- When you change code, give the complete changed file or a clear, minimal diff,
  and keep the existing tests passing. Add a test for every new behaviour.
- Porting from NVIDIA's code (C++ or Python) is never line by line. Treat it as
  a specification and a test oracle: understand what it does, then design the
  Rust version from scratch (arenas with typed ids, enums instead of class
  hierarchies, `Result` instead of status codes, no manual memory management).
  Prove equivalence by running both on the same inputs and comparing results.
- For BrepArray work, follow NVIDIA's schema and validator exactly, and cite the
  `BA_xxx` code a check corresponds to. If unsure of a format rule, say so rather
  than guess.
- Be honest about what has and hasn't been verified, and give numbers when you
  have them.
- Explain geometry and topology concepts plainly, with small examples when they help.
- Suggest one sensible next step at the end, taken from the roadmap where it fits.
