//! Geometry backends.
//!
//! Nodes never call a B-rep library directly. They ask the graph's `Kernel`
//! to build solids, and ask each `Solid` to measure or export itself. The
//! default backend is mini_brep; SMLib through FFI can be added later as one
//! more implementation without touching the nodes.

use mini_brep::{Brep, Vec3};
use std::fmt;
use std::rc::Rc;
use usd_brep::BrepArray;
use usd_brep::convert::{ExportOptions, from_mini_brep};

/// A closed solid owned by some kernel.
pub trait Solid: fmt::Debug {
    fn volume(&self) -> f64;
    fn surface_area(&self) -> f64;
    /// (vertices, edges, faces)
    fn counts(&self) -> (usize, usize, usize);
    /// The solid as a one-Brep BrepArray prim: the kernel-neutral exchange form.
    fn to_brep_array(&self, name: &str) -> BrepArray;
}

/// Builds solids. Every solid it returns has already passed its validity checks.
pub trait Kernel {
    fn name(&self) -> &'static str;
    /// Extrude a closed planar profile in the XY plane straight up by `height`.
    fn extrude(&self, profile: &[Vec3], height: f64) -> Result<Rc<dyn Solid>, String>;
}

/// The built-in mini_brep backend.
pub struct MiniBrep;

impl Kernel for MiniBrep {
    fn name(&self) -> &'static str { "mini_brep" }
    fn extrude(&self, profile: &[Vec3], height: f64) -> Result<Rc<dyn Solid>, String> {
        let b = Brep::extrude_polygon(profile, height);
        b.validate().map_err(|e| format!("invalid solid: {e:?}"))?;
        Ok(Rc::new(b))
    }
}

impl Solid for Brep {
    fn volume(&self) -> f64 { Brep::volume(self) }
    fn surface_area(&self) -> f64 { Brep::surface_area(self) }
    fn counts(&self) -> (usize, usize, usize) { Brep::counts(self) }
    fn to_brep_array(&self, name: &str) -> BrepArray {
        from_mini_brep(self, name, &ExportOptions::default())
    }
}
