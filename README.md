# Rust CAD with AI

An original parametric CAD system in Rust, built to be driven by AI, with
native support for NVIDIA's OpenUSD B-rep (BrepArray) format.

| Crate | What it is |
|---|---|
| `mini_brep` | B-rep kernel: planes, cylinders, cones, spheres, tori; extrude and revolve; through-holes; exact volume and area; validation. No dependencies. |
| `node_graph` | Grasshopper-style dataflow graph. Every edit goes through one command API, which is also the AI's interface. No dependencies. |
| `usd_brep` | Reads, writes and validates BrepArray `.usda` files. No dependencies. |
| `cad_app` | Desktop editor (egui): node canvas plus live 3D preview. |

```
cargo test --workspace
cargo run -p cad_app
```

See `00_PROJECT_OVERVIEW.md` for the architecture, verified results and
roadmap.

## Relationship to NVIDIA

This is original work, **compatible with** NVIDIA's OpenUSD B-rep schema
and checked against NVIDIA's own tools. It is not affiliated with or
endorsed by NVIDIA.

- **File format:** `usd_brep` implements NVIDIA's BrepArray schema exactly,
  so files work in NVIDIA's tools. The Rust code is written from the schema.
- **Validator:** modelled on NVIDIA's `brep_validator.py` (same checks and
  `BA_xxx` codes), reimplemented in Rust. It agrees with NVIDIA's validator on
  132 of 132 of NVIDIA's sample files.
- **Kernel, graph and editor:** our own designs. Exports load into NVIDIA's
  SMLib kernel as valid solids with matching volumes (11 of 11 sample
  solids), which is how we check correctness. No SMLib code is included.

Future features ported from NVIDIA's code follow one rule: their code is the
specification and the reference to test against, and the Rust version is
designed from scratch. See `NOTICE` for attribution under Apache-2.0.
