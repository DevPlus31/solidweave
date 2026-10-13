//! Geometry backends.
//!
//! Nodes never call a B-rep library directly. They ask the graph's `Kernel`
//! to build solids, and ask each `Solid` to measure or export itself. The
//! default backend is mini_brep; SMLib through FFI can be added later as one
//! more implementation without touching the nodes.

pub use mini_brep::BooleanOp;
use mini_brep::{Brep, Profile, Ring, Vec3, check_revolve_profile};
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
    /// Triangles for display, wound counter-clockwise seen from outside.
    fn tessellate(&self) -> Vec<[Vec3; 3]>;
    /// Lets a kernel recognise its own solids (for booleans).
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Builds solids. Every solid it returns has already passed its validity checks.
pub trait Kernel {
    fn name(&self) -> &'static str;
    /// Extrude a flat profile (outer boundary plus holes) straight up by
    /// `height`. Each hole becomes a through-hole.
    fn extrude(&self, profile: &Profile, height: f64) -> Result<Rc<dyn Solid>, String>;
    /// Revolve a profile without holes a full turn. The profile is drawn in
    /// the XY plane with x the distance from the axis and y the height; the
    /// solid stands round the Z axis.
    fn revolve(&self, _profile: &Profile) -> Result<Rc<dyn Solid>, String> {
        Err(format!("the {} kernel cannot revolve", self.name()))
    }
    fn sphere(&self, _center: Vec3, _radius: f64) -> Result<Rc<dyn Solid>, String> {
        Err(format!("the {} kernel cannot make spheres", self.name()))
    }
    /// `a` op `b`, for solids this kernel made.
    fn boolean(&self, _a: &dyn Solid, _b: &dyn Solid, _op: BooleanOp) -> Result<Rc<dyn Solid>, String> {
        Err(format!("the {} kernel cannot do booleans", self.name()))
    }
}

/// The built-in mini_brep backend.
pub struct MiniBrep;

fn checked(b: Brep) -> Result<Rc<dyn Solid>, String> {
    b.validate().map_err(|e| format!("invalid solid: {e:?}"))?;
    Ok(Rc::new(b))
}

impl Kernel for MiniBrep {
    fn name(&self) -> &'static str { "mini_brep" }
    fn extrude(&self, profile: &Profile, height: f64) -> Result<Rc<dyn Solid>, String> {
        // Check the region first: a hole outside the outline still makes
        // valid topology, just the wrong shape.
        profile.validate().map_err(|e| format!("invalid profile: {e}"))?;
        checked(Brep::extrude(profile, height))
    }

    fn revolve(&self, profile: &Profile) -> Result<Rc<dyn Solid>, String> {
        if !profile.holes.is_empty() {
            return Err("revolve takes a profile without holes".into());
        }
        match &profile.outer {
            Ring::Polygon(pts) => {
                let rz: Vec<[f64; 2]> = pts.iter().map(|p| [p.x, p.y]).collect();
                check_revolve_profile(&rz).map_err(|e| format!("invalid revolve profile: {e}"))?;
                checked(Brep::revolve(&rz))
            }
            // A circle revolved round an axis it does not touch is a torus.
            Ring::Circle { center, radius } if center.x > *radius => {
                checked(Brep::make_torus(Vec3::new(0.0, 0.0, center.y), center.x, *radius))
            }
            Ring::Circle { .. } => Err("a revolved circle must lie clear of the axis (x > radius)".into()),
        }
    }

    fn sphere(&self, center: Vec3, radius: f64) -> Result<Rc<dyn Solid>, String> {
        if radius <= 0.0 {
            return Err(format!("radius must be positive, got {radius}"));
        }
        checked(Brep::make_sphere(center, radius))
    }

    fn boolean(&self, a: &dyn Solid, b: &dyn Solid, op: BooleanOp) -> Result<Rc<dyn Solid>, String> {
        fn brep(s: &dyn Solid) -> Result<&Brep, String> {
            s.as_any().downcast_ref::<Brep>().ok_or_else(|| "a solid from another kernel".to_string())
        }
        let r = brep(a)?.boolean(brep(b)?, op).map_err(|e| e.to_string())?;
        match r.shells.len() {
            0 => Err("the result is empty".into()),
            1 => Ok(Rc::new(r)),
            // mini_brep handles these, but BrepArray export writes one shell.
            n => Err(format!("the result has {n} shells (separate pieces or a hollow inside); only one is supported for now")),
        }
    }
}

impl Solid for Brep {
    fn volume(&self) -> f64 { Brep::volume(self) }
    fn surface_area(&self) -> f64 { Brep::surface_area(self) }
    fn counts(&self) -> (usize, usize, usize) { Brep::counts(self) }
    fn to_brep_array(&self, name: &str) -> BrepArray {
        from_mini_brep(self, name, &ExportOptions::default())
    }
    fn tessellate(&self) -> Vec<[Vec3; 3]> {
        Brep::tessellate(self)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
