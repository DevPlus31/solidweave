//! usd_brep — pure-Rust support for NVIDIA's OpenUSD B-rep format.
//!
//! * [`usda`]     — a dependency-free USDA text reader and writing helpers
//! * [`model`]    — the `BrepArray` schema as Rust types (one attribute table
//!   drives reading, writing and type checks)
//! * [`validate`](mod@validate) — structural checks modelled on NVIDIA's `brep_validator.py`
//!   and reimplemented in Rust, tagged with the same `BA_xxx` codes
//! * [`convert`]  — export `mini_brep` solids (planes, cylinders, cones,
//!   spheres, tori) to BrepArray and import planar BrepArrays back
//!
//! Format reference: `source/schema/omniSolid/resources/schema.usda` in
//! <https://github.com/NVIDIA-Omniverse/usd-brep> (Apache-2.0).

pub mod convert;
pub mod model;
pub mod usda;
pub mod validate;

pub use model::*;
pub use validate::{Issue, Severity, validate};
