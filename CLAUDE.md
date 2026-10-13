# Rust CAD with AI

A parametric 3D CAD system in Rust, designed to be driven by AI. Four crates:
mini_brep (B-rep kernel), node_graph (Grasshopper-style dataflow engine),
usd_brep (NVIDIA OpenUSD BrepArray reader/writer/validator) and cad_app (egui
editor). Start from `00_PROJECT_OVERVIEW.md`, which records the architecture,
verified results, BrepArray format facts and the roadmap.

## Layout and commands

- Cargo workspace at the root: `cad_app/` (egui editor, `cargo run -p cad_app`),
  `mini_brep/`, `node_graph/`, `usd_brep/`. Only `cad_app` has external
  dependencies (eframe/egui 0.36, egui-snarl 0.12); keep the other three
  dependency-free.
- `cargo test --workspace` runs everything, including tests against NVIDIA's
  sample files in `usd_brep/tests/fixtures/` (Apache-2.0, see NOTICE.md).
- Examples: `kernel_demo` and `boolean_cases` (mini_brep), `graph_demo`
  (node_graph), `demo`, `validate_files`, `read_files`, `roundtrip`
  (usd_brep). Example names must stay unique across the workspace, or their
  binaries overwrite each other in `target/`.

## Checking against NVIDIA

Tools in `usd_brep/tools/compare_with_nvidia/`. After any change to export or
geometry, export with `cargo run -p usd_brep --example demo -- DIR` (and
`graph_demo`) and run all three NVIDIA checks below.

- **Python validator:** `run.sh <usd-brep checkout>` (bash) compares our
  validator with NVIDIA's on their files; `validate_runner.py` checks single
  files. Needs `pip install usd-core "usd-validation-nvidia>=1.22.0,<2"` and
  `OMNISOLID_PLUGIN_PATH` set to the checkout's
  `source/schema/omniSolid/resources`.
- **SMLib kernel**, a built checkout at `C:\Users\Mustapha\nvb` (2.7 GB),
  with Python 3.12:
  - `python3 smlib_import.py C:\Users\Mustapha\nvb FILE.usda…`: loads files
    into SMLib; manifold, volume, area.
  - `cargo run -q -p mini_brep --example boolean_cases > cases.json`, then
    `python3 smlib_boolean.py C:\Users\Mustapha\nvb cases.json`: our booleans
    vs SMLib's.
  - `nvb\_build\windows-x86_64\release\brep_geometry_validator.exe --level 2
    --no-healer FILE.usda…`: SMLib's full geometry checks. Put that folder
    and its `extraLibs` on PATH and set `OMNISOLID_PLUGIN_PATH` to
    `nvb\_build\schema\omniSolid\resources`.
  - Rebuilding: in `repo.toml` set `msbuild.vs_version = "vs2022"` (this
    machine has no v142 toolset), then `prebuild.bat` and `repo.bat build -r`.
    The checkout must be at a short path: from a long one, cl.exe's `/MP`
    child command lines pass 32k characters (errors C1083 '' and D8040), and
    `subst` does not help because the build tool resolves real paths.
- If `OMNISOLID_PLUGIN_PATH` is set wrongly, SMLib imports fail with
  SM_ERR (1001) for every file; unset it and the scripts set it themselves.

## Other files
- `PROJECT_INSTRUCTIONS.md` holds the same working rules as instruction text
  for a claude.ai Project.

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
