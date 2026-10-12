//! Rust mirror of NVIDIA's `omniSolid` BrepArray schema.
//!
//! The layout follows the schema exactly: every topology level and every
//! geometry API is a set of flat, packed arrays. One `BrepArray` prim can hold
//! many Breps; each Brep owns a consecutive range of every array.
//!
//! Hierarchy (counts link each level to the next):
//!
//! ```text
//! brep:regionCount ─► region:shellCount ─► shell:faceuseCount ─► faceuse:faceIndex
//!                                         shell:wireEdgeCount
//! face:loopCount ─► loop:edgeuseCount ─► edgeuse:edgeIndex ─► edge:vertexIndices
//! ```
//!
//! Every face has two faceuses (one per side), and every edgeuse is a
//! top/bottom mated pair linked radially around its edge.

use crate::usda::{self, Lit, Prim};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::ops::Range;

// ───────────────────────────── Tokens ─────────────────────────────

/// A schema token value. Unknown spellings are kept as `Other` so that a file
/// with bad tokens still loads and the validator can report them.
pub trait Token: Sized + Clone + PartialEq {
    fn from_token(s: &str) -> Self;
    fn as_token(&self) -> &str;
    /// The tokens the schema allows.
    fn allowed() -> &'static [&'static str];
}

macro_rules! token_enum {
    ($(#[$m:meta])* $name:ident { $($var:ident = $tok:literal),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name { $($var,)* Other(String) }
        impl Token for $name {
            fn from_token(s: &str) -> Self {
                match s { $($tok => Self::$var,)* other => Self::Other(other.to_string()) }
            }
            fn as_token(&self) -> &str {
                match self { $(Self::$var => $tok,)* Self::Other(s) => s }
            }
            fn allowed() -> &'static [&'static str] { &[$($tok),*] }
        }
    };
}

token_enum!(RegionType { Solid = "solidRegion", Void = "voidRegion" });
token_enum!(ShellPointType { BrepPoint = "BrepPointAPI", None = "none" });
token_enum!(
    /// `same` = the side (or direction) given by the underlying geometry.
    Orientation { Same = "same", Opposite = "opposite" }
);
token_enum!(RadialEntry { Top = "topEntry", Bottom = "bottomEntry" });
token_enum!(TrimType { Rectangular = "rectangular", General = "general" });
token_enum!(VertexPointType { BrepPoint = "BrepPointAPI" });
token_enum!(SurfaceType {
    Nurbs = "BrepSurfaceNurbAPI",
    Plane = "BrepSurfacePlaneAPI",
    Cylinder = "BrepSurfaceCylinderAPI",
    Cone = "BrepSurfaceConeAPI",
    Sphere = "BrepSurfaceSphereAPI",
    Torus = "BrepSurfaceTorusAPI",
});
token_enum!(CurveType {
    Nurbs = "BrepCurve3dNurbAPI",
    Line = "BrepCurve3dLineAPI",
    Circle = "BrepCurve3dCircleAPI",
    Ellipse = "BrepCurve3dEllipseAPI",
});

// ───────────────────────────── Geometry API sets ─────────────────────────────

pub type V3 = [f64; 3];
pub type V2 = [f64; 2];

/// `BrepCurve3dNurbAPI`: packed 3D NURBS curves (orders are degree + 1).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NurbsCurves3d {
    pub order: Vec<u32>,
    pub vertex_count: Vec<u32>,
    pub control_vertices: Vec<V3>,
    pub weights: Vec<f64>,
    pub knots: Vec<f64>,
}

/// `BrepCurveUvNurbAPI`: one 2D trim curve per edgeuse, in the face's UV space.
/// An edgeuse without a UV curve uses order 0 and vertexCount 0.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NurbsCurves2d {
    pub order: Vec<u32>,
    pub vertex_count: Vec<u32>,
    pub control_vertices: Vec<V2>,
    pub weights: Vec<f64>,
    pub knots: Vec<f64>,
}

/// `BrepSurfaceNurbAPI`. Control vertices are stored `u` major:
/// index = u * vVertexCount + v.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NurbsSurfaces {
    pub u_order: Vec<u32>,
    pub v_order: Vec<u32>,
    pub u_vertex_count: Vec<u32>,
    pub v_vertex_count: Vec<u32>,
    pub control_vertices: Vec<V3>,
    pub weights: Vec<f64>,
    pub u_knots: Vec<f64>,
    pub v_knots: Vec<f64>,
}

/// C(t) = origin + t * direction
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lines {
    pub origin: Vec<V3>,
    pub direction: Vec<V3>,
}

/// C(t) = center + r (cos t · ref + sin t · (axis × ref)), t in radians.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Circles {
    pub center: Vec<V3>,
    pub axis: Vec<V3>,
    pub ref_direction: Vec<V3>,
    pub radius: Vec<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ellipses {
    pub center: Vec<V3>,
    pub axis: Vec<V3>,
    pub ref_direction: Vec<V3>,
    pub x_radius: Vec<f64>,
    pub y_radius: Vec<f64>,
}

/// S(u,v) = origin + u · ref + v · (axis × ref). Normal = axis.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Planes {
    pub origin: Vec<V3>,
    pub axis: Vec<V3>,
    pub ref_direction: Vec<V3>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cylinders {
    pub origin: Vec<V3>,
    pub axis: Vec<V3>,
    pub ref_direction: Vec<V3>,
    pub radius: Vec<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cones {
    pub origin: Vec<V3>,
    pub axis: Vec<V3>,
    pub ref_direction: Vec<V3>,
    pub radius: Vec<f64>,
    pub semi_angle: Vec<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spheres {
    pub center: Vec<V3>,
    pub axis: Vec<V3>,
    pub ref_direction: Vec<V3>,
    pub radius: Vec<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tori {
    pub origin: Vec<V3>,
    pub axis: Vec<V3>,
    pub ref_direction: Vec<V3>,
    pub major_radius: Vec<f64>,
    pub minor_radius: Vec<f64>,
}

// ───────────────────────────── BrepArray ─────────────────────────────

/// One `BrepArray` prim. Field names follow the schema attribute names.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrepArray {
    /// Prim name (and path, when read from a file).
    pub name: String,
    pub path: String,
    /// Applied API schemas as read. Writing adds any that the data requires.
    pub api_schemas: Vec<String>,

    // Gprim
    pub prim_extent: Vec<V3>,

    // Brep level (one entry per Brep; extent has two)
    pub intersect_tol_3d: Vec<f64>,
    pub extent: Vec<V3>,
    pub region_count: Vec<u32>,

    // Regions
    pub region_shell_count: Vec<u32>,
    pub region_type: Vec<RegionType>,

    // Shells
    pub shell_faceuse_count: Vec<u32>,
    pub shell_wire_edge_count: Vec<u32>,
    pub shell_point_type: Vec<ShellPointType>,

    // Faceuses (two per face)
    pub faceuse_face_index: Vec<u32>,
    pub faceuse_orientation: Vec<Orientation>,

    // Faces
    pub face_loop_count: Vec<u32>,
    pub face_surface_type: Vec<SurfaceType>,
    pub face_trim_type: Vec<TrimType>,
    /// Two entries per face: {UVmin, UVmax}.
    pub face_range: Vec<V2>,

    // Loops
    pub loop_edgeuse_count: Vec<u32>,
    pub loop_vertex_index: Vec<u32>,

    // Edgeuses
    pub edgeuse_edge_index: Vec<u32>,
    pub edgeuse_orientation: Vec<Orientation>,
    pub edgeuse_next_radial: Vec<u32>,
    pub edgeuse_entry: Vec<RadialEntry>,

    // Edges
    pub edge_curve_type: Vec<CurveType>,
    /// Two entries per edge: {tMin, tMax}.
    pub edge_range: Vec<f64>,
    pub edge_vertex_indices: Vec<[i64; 2]>,

    // Wire edges
    pub wire_edge_curve_type: Vec<CurveType>,
    pub wire_edge_range: Vec<f64>,
    pub wire_edge_vertex_indices: Vec<[i64; 2]>,

    // Vertices
    pub vertex_point_type: Vec<VertexPointType>,

    // Point geometry
    pub vertex_points: Vec<V3>,
    pub shell_points: Vec<V3>,

    // Curve geometry (edge and wire-edge instances of each API)
    pub edge_nurbs: NurbsCurves3d,
    pub edge_lines: Lines,
    pub edge_circles: Circles,
    pub edge_ellipses: Ellipses,
    pub wire_edge_nurbs: NurbsCurves3d,
    pub wire_edge_lines: Lines,
    pub wire_edge_circles: Circles,
    pub wire_edge_ellipses: Ellipses,
    pub uv_curves: NurbsCurves2d,

    // Surface geometry
    pub surface_nurbs: NurbsSurfaces,
    pub planes: Planes,
    pub cylinders: Cylinders,
    pub cones: Cones,
    pub spheres: Spheres,
    pub tori: Tori,

    /// What was authored in the source file: attribute name → declared type.
    /// Empty for BrepArrays built in code.
    pub authored: BTreeMap<String, String>,
    /// Values that could not be converted to the schema type while reading.
    pub read_issues: Vec<String>,
}

// ───────────────────────────── Attribute data conversion ─────────────────────────────

/// Object-safe access to one array attribute, used by the generic reader/writer.
pub trait AttrData {
    fn read(&mut self, lit: &Lit) -> Result<(), String>;
    fn write(&self, out: &mut String);
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn list(lit: &Lit) -> Result<&[Lit], String> {
    match lit {
        Lit::List(v) => Ok(v),
        other => Err(format!("expected an array, found {other:?}")),
    }
}

fn num(l: &Lit) -> Result<f64, String> {
    l.as_f64().ok_or_else(|| format!("expected a number, found {l:?}"))
}

fn tuple<const N: usize>(l: &Lit) -> Result<[f64; N], String> {
    match l {
        Lit::Tuple(t) if t.len() == N => {
            let mut a = [0.0; N];
            for (k, x) in t.iter().enumerate() {
                a[k] = num(x)?;
            }
            Ok(a)
        }
        other => Err(format!("expected a {N}-tuple, found {other:?}")),
    }
}

fn write_list<T>(out: &mut String, items: &[T], mut f: impl FnMut(&mut String, &T)) {
    out.push('[');
    for (i, x) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        f(out, x);
    }
    out.push(']');
}

impl AttrData for Vec<f64> {
    fn read(&mut self, lit: &Lit) -> Result<(), String> {
        *self = list(lit)?.iter().map(num).collect::<Result<_, _>>()?;
        Ok(())
    }
    fn write(&self, out: &mut String) {
        write_list(out, self, |o, v| o.push_str(&usda::fmt_f64(*v)));
    }
    fn len(&self) -> usize {
        Vec::len(self)
    }
}

impl AttrData for Vec<u32> {
    fn read(&mut self, lit: &Lit) -> Result<(), String> {
        *self = list(lit)?
            .iter()
            .map(|l| {
                let v = num(l)?;
                if v.fract() != 0.0 || !(0.0..=u32::MAX as f64).contains(&v) {
                    Err(format!("{v} is not a valid uint"))
                } else {
                    Ok(v as u32)
                }
            })
            .collect::<Result<_, _>>()?;
        Ok(())
    }
    fn write(&self, out: &mut String) {
        write_list(out, self, |o, v| {
            let _ = write!(o, "{v}");
        });
    }
    fn len(&self) -> usize {
        Vec::len(self)
    }
}

impl AttrData for Vec<[i64; 2]> {
    fn read(&mut self, lit: &Lit) -> Result<(), String> {
        *self = list(lit)?
            .iter()
            .map(|l| {
                let [a, b] = tuple::<2>(l)?;
                if a.fract() != 0.0 || b.fract() != 0.0 {
                    return Err(format!("({a}, {b}) is not an int2"));
                }
                Ok([a as i64, b as i64])
            })
            .collect::<Result<_, _>>()?;
        Ok(())
    }
    fn write(&self, out: &mut String) {
        write_list(out, self, |o, [a, b]| {
            let _ = write!(o, "({a}, {b})");
        });
    }
    fn len(&self) -> usize {
        Vec::len(self)
    }
}

impl AttrData for Vec<V3> {
    fn read(&mut self, lit: &Lit) -> Result<(), String> {
        *self = list(lit)?.iter().map(tuple::<3>).collect::<Result<_, _>>()?;
        Ok(())
    }
    fn write(&self, out: &mut String) {
        write_list(out, self, |o, p| {
            let _ = write!(o, "({}, {}, {})", usda::fmt_f64(p[0]), usda::fmt_f64(p[1]), usda::fmt_f64(p[2]));
        });
    }
    fn len(&self) -> usize {
        Vec::len(self)
    }
}

impl AttrData for Vec<V2> {
    fn read(&mut self, lit: &Lit) -> Result<(), String> {
        *self = list(lit)?.iter().map(tuple::<2>).collect::<Result<_, _>>()?;
        Ok(())
    }
    fn write(&self, out: &mut String) {
        write_list(out, self, |o, p| {
            let _ = write!(o, "({}, {})", usda::fmt_f64(p[0]), usda::fmt_f64(p[1]));
        });
    }
    fn len(&self) -> usize {
        Vec::len(self)
    }
}

impl<T: Token> AttrData for Vec<T> {
    fn read(&mut self, lit: &Lit) -> Result<(), String> {
        *self = list(lit)?
            .iter()
            .map(|l| match l {
                Lit::Str(s) => Ok(T::from_token(s)),
                other => Err(format!("expected a token string, found {other:?}")),
            })
            .collect::<Result<_, _>>()?;
        Ok(())
    }
    fn write(&self, out: &mut String) {
        write_list(out, self, |o, t| o.push_str(&usda::quote(t.as_token())));
    }
    fn len(&self) -> usize {
        Vec::len(self)
    }
}

// ───────────────────────────── The attribute table ─────────────────────────────

/// One schema attribute: USD name, declared USD type, and the API schema that
/// owns it ("" = core BrepArray / Gprim attribute).
#[derive(Debug, Clone, Copy)]
pub struct AttrSpec {
    pub name: &'static str,
    pub usd_type: &'static str,
    pub api: &'static str,
}

/// Single source of truth: drives the reader, the writer and the type checks.
macro_rules! attribute_table {
    ($( $($field:ident).+ => $name:literal : $ty:literal @ $api:literal ),* $(,)?) => {
        /// Every attribute this crate understands, in schema order.
        pub const ATTRIBUTES: &[AttrSpec] = &[ $( AttrSpec { name: $name, usd_type: $ty, api: $api } ),* ];

        impl BrepArray {
            fn visit(&self, f: &mut dyn FnMut(&'static AttrSpec, &dyn AttrData)) {
                let mut i = 0;
                $( f(&ATTRIBUTES[i], &self.$($field).+); i += 1; )*
                let _ = i;
            }
            fn visit_mut(&mut self, f: &mut dyn FnMut(&'static AttrSpec, &mut dyn AttrData)) {
                let mut i = 0;
                $( f(&ATTRIBUTES[i], &mut self.$($field).+); i += 1; )*
                let _ = i;
            }
        }
    };
}

attribute_table! {
    prim_extent                  => "extent"                       : "float3[]"  @ "",
    intersect_tol_3d             => "brep:intersectTol3d"          : "double[]"  @ "",
    extent                       => "brep:extent"                  : "double3[]" @ "",
    region_count                 => "brep:regionCount"             : "uint[]"    @ "",
    region_shell_count           => "region:shellCount"            : "uint[]"    @ "",
    region_type                  => "region:type"                  : "token[]"   @ "",
    shell_faceuse_count          => "shell:faceuseCount"           : "uint[]"    @ "",
    shell_wire_edge_count        => "shell:wireEdgeCount"          : "uint[]"    @ "",
    shell_point_type             => "shell:pointType"              : "token[]"   @ "",
    faceuse_face_index           => "faceuse:faceIndex"            : "uint[]"    @ "",
    faceuse_orientation          => "faceuse:orientationType"      : "token[]"   @ "",
    face_loop_count              => "face:loopCount"               : "uint[]"    @ "",
    face_surface_type            => "face:surfaceType"             : "token[]"   @ "",
    face_trim_type               => "face:trimType"                : "token[]"   @ "",
    face_range                   => "face:range"                   : "double2[]" @ "",
    loop_edgeuse_count           => "loop:edgeuseCount"            : "uint[]"    @ "",
    loop_vertex_index            => "loop:vertexIndex"             : "uint[]"    @ "",
    edgeuse_edge_index           => "edgeuse:edgeIndex"            : "uint[]"    @ "",
    edgeuse_orientation          => "edgeuse:orientationType"      : "token[]"   @ "",
    edgeuse_next_radial          => "edgeuse:nextRadialEUIndex"    : "uint[]"    @ "",
    edgeuse_entry                => "edgeuse:thisRadialEntryType"  : "token[]"   @ "",
    edge_curve_type              => "edge:curveType"               : "token[]"   @ "",
    edge_range                   => "edge:range"                   : "double[]"  @ "",
    edge_vertex_indices          => "edge:vertexIndices"           : "int2[]"    @ "",
    wire_edge_curve_type         => "wireEdge:curveType"           : "token[]"   @ "",
    wire_edge_range              => "wireEdge:range"               : "double[]"  @ "",
    wire_edge_vertex_indices     => "wireEdge:vertexIndices"       : "int2[]"    @ "",
    vertex_point_type            => "vertex:pointType"             : "token[]"   @ "",

    vertex_points                => "brep:vertexPoint:point:position" : "point3d[]" @ "BrepPointAPI:vertexPoint",
    shell_points                 => "brep:shellPoint:point:position"  : "point3d[]" @ "BrepPointAPI:shellPoint",

    edge_nurbs.control_vertices  => "brep:edge3dNurb:curve3d:nurb:controlVertices" : "point3d[]" @ "BrepCurve3dNurbAPI:edge3dNurb",
    edge_nurbs.vertex_count      => "brep:edge3dNurb:curve3d:nurb:vertexCount"     : "uint[]"    @ "BrepCurve3dNurbAPI:edge3dNurb",
    edge_nurbs.order             => "brep:edge3dNurb:curve3d:nurb:order"           : "uint[]"    @ "BrepCurve3dNurbAPI:edge3dNurb",
    edge_nurbs.knots             => "brep:edge3dNurb:curve3d:nurb:knots"           : "double[]"  @ "BrepCurve3dNurbAPI:edge3dNurb",
    edge_nurbs.weights           => "brep:edge3dNurb:curve3d:nurb:weights"         : "double[]"  @ "BrepCurve3dNurbAPI:edge3dNurb",
    wire_edge_nurbs.control_vertices => "brep:wireEdge3dNurb:curve3d:nurb:controlVertices" : "point3d[]" @ "BrepCurve3dNurbAPI:wireEdge3dNurb",
    wire_edge_nurbs.vertex_count => "brep:wireEdge3dNurb:curve3d:nurb:vertexCount"     : "uint[]"    @ "BrepCurve3dNurbAPI:wireEdge3dNurb",
    wire_edge_nurbs.order        => "brep:wireEdge3dNurb:curve3d:nurb:order"           : "uint[]"    @ "BrepCurve3dNurbAPI:wireEdge3dNurb",
    wire_edge_nurbs.knots        => "brep:wireEdge3dNurb:curve3d:nurb:knots"           : "double[]"  @ "BrepCurve3dNurbAPI:wireEdge3dNurb",
    wire_edge_nurbs.weights      => "brep:wireEdge3dNurb:curve3d:nurb:weights"         : "double[]"  @ "BrepCurve3dNurbAPI:wireEdge3dNurb",

    edge_lines.origin            => "brep:edge3dLine:curve3d:line:origin"         : "point3d[]"  @ "BrepCurve3dLineAPI:edge3dLine",
    edge_lines.direction         => "brep:edge3dLine:curve3d:line:direction"      : "vector3d[]" @ "BrepCurve3dLineAPI:edge3dLine",
    wire_edge_lines.origin       => "brep:wireEdge3dLine:curve3d:line:origin"     : "point3d[]"  @ "BrepCurve3dLineAPI:wireEdge3dLine",
    wire_edge_lines.direction    => "brep:wireEdge3dLine:curve3d:line:direction"  : "vector3d[]" @ "BrepCurve3dLineAPI:wireEdge3dLine",

    edge_circles.center          => "brep:edge3dCircle:curve3d:circle:center"        : "point3d[]"  @ "BrepCurve3dCircleAPI:edge3dCircle",
    edge_circles.axis            => "brep:edge3dCircle:curve3d:circle:axis"          : "vector3d[]" @ "BrepCurve3dCircleAPI:edge3dCircle",
    edge_circles.ref_direction   => "brep:edge3dCircle:curve3d:circle:refDirection"  : "vector3d[]" @ "BrepCurve3dCircleAPI:edge3dCircle",
    edge_circles.radius          => "brep:edge3dCircle:curve3d:circle:radius"        : "double[]"   @ "BrepCurve3dCircleAPI:edge3dCircle",
    wire_edge_circles.center     => "brep:wireEdge3dCircle:curve3d:circle:center"       : "point3d[]"  @ "BrepCurve3dCircleAPI:wireEdge3dCircle",
    wire_edge_circles.axis       => "brep:wireEdge3dCircle:curve3d:circle:axis"         : "vector3d[]" @ "BrepCurve3dCircleAPI:wireEdge3dCircle",
    wire_edge_circles.ref_direction => "brep:wireEdge3dCircle:curve3d:circle:refDirection" : "vector3d[]" @ "BrepCurve3dCircleAPI:wireEdge3dCircle",
    wire_edge_circles.radius     => "brep:wireEdge3dCircle:curve3d:circle:radius"       : "double[]"   @ "BrepCurve3dCircleAPI:wireEdge3dCircle",

    edge_ellipses.center         => "brep:edge3dEllipse:curve3d:ellipse:center"       : "point3d[]"  @ "BrepCurve3dEllipseAPI:edge3dEllipse",
    edge_ellipses.axis           => "brep:edge3dEllipse:curve3d:ellipse:axis"         : "vector3d[]" @ "BrepCurve3dEllipseAPI:edge3dEllipse",
    edge_ellipses.ref_direction  => "brep:edge3dEllipse:curve3d:ellipse:refDirection" : "vector3d[]" @ "BrepCurve3dEllipseAPI:edge3dEllipse",
    edge_ellipses.x_radius       => "brep:edge3dEllipse:curve3d:ellipse:xRadius"      : "double[]"   @ "BrepCurve3dEllipseAPI:edge3dEllipse",
    edge_ellipses.y_radius       => "brep:edge3dEllipse:curve3d:ellipse:yRadius"      : "double[]"   @ "BrepCurve3dEllipseAPI:edge3dEllipse",
    wire_edge_ellipses.center    => "brep:wireEdge3dEllipse:curve3d:ellipse:center"       : "point3d[]"  @ "BrepCurve3dEllipseAPI:wireEdge3dEllipse",
    wire_edge_ellipses.axis      => "brep:wireEdge3dEllipse:curve3d:ellipse:axis"         : "vector3d[]" @ "BrepCurve3dEllipseAPI:wireEdge3dEllipse",
    wire_edge_ellipses.ref_direction => "brep:wireEdge3dEllipse:curve3d:ellipse:refDirection" : "vector3d[]" @ "BrepCurve3dEllipseAPI:wireEdge3dEllipse",
    wire_edge_ellipses.x_radius  => "brep:wireEdge3dEllipse:curve3d:ellipse:xRadius"      : "double[]"   @ "BrepCurve3dEllipseAPI:wireEdge3dEllipse",
    wire_edge_ellipses.y_radius  => "brep:wireEdge3dEllipse:curve3d:ellipse:yRadius"      : "double[]"   @ "BrepCurve3dEllipseAPI:wireEdge3dEllipse",

    uv_curves.control_vertices   => "brep:curveUv:nurb:controlVertices" : "double2[]" @ "BrepCurveUvNurbAPI",
    uv_curves.vertex_count       => "brep:curveUv:nurb:vertexCount"     : "uint[]"    @ "BrepCurveUvNurbAPI",
    uv_curves.order              => "brep:curveUv:nurb:order"           : "uint[]"    @ "BrepCurveUvNurbAPI",
    uv_curves.knots              => "brep:curveUv:nurb:knots"           : "double[]"  @ "BrepCurveUvNurbAPI",
    uv_curves.weights            => "brep:curveUv:nurb:weights"         : "double[]"  @ "BrepCurveUvNurbAPI",

    surface_nurbs.control_vertices => "brep:surface:nurb:controlVertices" : "point3d[]" @ "BrepSurfaceNurbAPI",
    surface_nurbs.u_knots        => "brep:surface:nurb:uKnots"        : "double[]" @ "BrepSurfaceNurbAPI",
    surface_nurbs.u_order        => "brep:surface:nurb:uOrder"        : "uint[]"   @ "BrepSurfaceNurbAPI",
    surface_nurbs.u_vertex_count => "brep:surface:nurb:uVertexCount"  : "uint[]"   @ "BrepSurfaceNurbAPI",
    surface_nurbs.v_knots        => "brep:surface:nurb:vKnots"        : "double[]" @ "BrepSurfaceNurbAPI",
    surface_nurbs.v_order        => "brep:surface:nurb:vOrder"        : "uint[]"   @ "BrepSurfaceNurbAPI",
    surface_nurbs.v_vertex_count => "brep:surface:nurb:vVertexCount"  : "uint[]"   @ "BrepSurfaceNurbAPI",
    surface_nurbs.weights        => "brep:surface:nurb:weights"       : "double[]" @ "BrepSurfaceNurbAPI",

    planes.origin                => "brep:surface:plane:origin"        : "point3d[]"  @ "BrepSurfacePlaneAPI",
    planes.axis                  => "brep:surface:plane:axis"          : "vector3d[]" @ "BrepSurfacePlaneAPI",
    planes.ref_direction         => "brep:surface:plane:refDirection"  : "vector3d[]" @ "BrepSurfacePlaneAPI",

    cylinders.origin             => "brep:surface:cylinder:origin"       : "point3d[]"  @ "BrepSurfaceCylinderAPI",
    cylinders.axis               => "brep:surface:cylinder:axis"         : "vector3d[]" @ "BrepSurfaceCylinderAPI",
    cylinders.ref_direction      => "brep:surface:cylinder:refDirection" : "vector3d[]" @ "BrepSurfaceCylinderAPI",
    cylinders.radius             => "brep:surface:cylinder:radius"       : "double[]"   @ "BrepSurfaceCylinderAPI",

    cones.origin                 => "brep:surface:cone:origin"       : "point3d[]"  @ "BrepSurfaceConeAPI",
    cones.axis                   => "brep:surface:cone:axis"         : "vector3d[]" @ "BrepSurfaceConeAPI",
    cones.ref_direction          => "brep:surface:cone:refDirection" : "vector3d[]" @ "BrepSurfaceConeAPI",
    cones.radius                 => "brep:surface:cone:radius"       : "double[]"   @ "BrepSurfaceConeAPI",
    cones.semi_angle             => "brep:surface:cone:semiAngle"    : "double[]"   @ "BrepSurfaceConeAPI",

    spheres.center               => "brep:surface:sphere:center"       : "point3d[]"  @ "BrepSurfaceSphereAPI",
    spheres.axis                 => "brep:surface:sphere:axis"         : "vector3d[]" @ "BrepSurfaceSphereAPI",
    spheres.ref_direction        => "brep:surface:sphere:refDirection" : "vector3d[]" @ "BrepSurfaceSphereAPI",
    spheres.radius               => "brep:surface:sphere:radius"       : "double[]"   @ "BrepSurfaceSphereAPI",

    tori.origin                  => "brep:surface:torus:origin"       : "point3d[]"  @ "BrepSurfaceTorusAPI",
    tori.axis                    => "brep:surface:torus:axis"         : "vector3d[]" @ "BrepSurfaceTorusAPI",
    tori.ref_direction           => "brep:surface:torus:refDirection" : "vector3d[]" @ "BrepSurfaceTorusAPI",
    tori.major_radius            => "brep:surface:torus:majorRadius"  : "double[]"   @ "BrepSurfaceTorusAPI",
    tori.minor_radius            => "brep:surface:torus:minorRadius"  : "double[]"   @ "BrepSurfaceTorusAPI",
}

impl BrepArray {
    /// Calls `f(attribute name, number of entries)` for every attribute.
    pub(crate) fn visit_lens(&self, f: &mut dyn FnMut(&'static str, usize)) {
        self.visit(&mut |spec, data| f(spec.name, data.len()));
    }
}

pub fn attr_spec(name: &str) -> Option<&'static AttrSpec> {
    ATTRIBUTES.iter().find(|a| a.name == name)
}

/// Loop vertex index written when a loop has edgeuses (the value is ignored).
pub const NO_LOOP_VERTEX: u32 = 9_999_999;

// ───────────────────────────── Reading and writing ─────────────────────────────

impl BrepArray {
    /// Build from a parsed `BrepArray` prim. Never fails: values that cannot be
    /// converted are left empty and recorded in `read_issues`.
    pub fn from_prim(prim: &Prim) -> Self {
        let mut b = BrepArray {
            name: prim.name.clone(),
            path: prim.path.clone(),
            api_schemas: prim.api_schemas.clone(),
            ..Default::default()
        };
        let mut authored = BTreeMap::new();
        let mut issues = Vec::new();
        b.visit_mut(&mut |spec, data| {
            if let Some(attr) = prim.attr(spec.name) {
                authored.insert(spec.name.to_string(), attr.type_name.clone());
                if let Some(v) = &attr.value
                    && let Err(e) = data.read(v) {
                        issues.push(format!("{}: {e}", spec.name));
                    }
            }
        });
        b.authored = authored;
        b.read_issues = issues;
        b
    }

    /// Read every BrepArray prim from USDA text.
    pub fn read_all(usda_text: &str) -> Result<Vec<BrepArray>, usda::ParseError> {
        let layer = usda::parse(usda_text)?;
        Ok(layer
            .all_prims()
            .into_iter()
            .filter(|p| p.type_name.as_deref() == Some("BrepArray"))
            .map(BrepArray::from_prim)
            .collect())
    }

    /// API schemas the data needs (rule BA_583) plus any already applied.
    pub fn api_schemas_for_write(&self) -> Vec<String> {
        let mut set: Vec<String> = self.api_schemas.clone();
        let mut add = |s: &str| {
            if !set.iter().any(|x| x == s) {
                set.push(s.to_string());
            }
        };
        if !self.vertex_point_type.is_empty() {
            add("BrepPointAPI:vertexPoint");
        }
        if !self.shell_points.is_empty() {
            add("BrepPointAPI:shellPoint");
        }
        for t in &self.edge_curve_type {
            if let Some(api) = curve_api(t, "edge") {
                add(&api);
            }
        }
        for t in &self.wire_edge_curve_type {
            if let Some(api) = curve_api(t, "wireEdge") {
                add(&api);
            }
        }
        for t in &self.face_surface_type {
            if !matches!(t, SurfaceType::Other(_)) {
                add(t.as_token());
            }
        }
        let uv = &self.uv_curves;
        let uv_record = uv.order.iter().any(|&o| o != 0) || uv.vertex_count.iter().any(|&v| v != 0);
        let uv_packed = !uv.control_vertices.is_empty() || !uv.knots.is_empty() || !uv.weights.is_empty();
        if uv_record || uv_packed {
            add("BrepCurveUvNurbAPI");
        }
        set
    }

    /// The prim body (attributes) in USDA, indented by `indent` spaces.
    /// Core topology attributes are always written (the schema requires them);
    /// geometry API attributes are written when their API is applied, when
    /// they hold data, or when they were authored in the source file.
    pub fn write_attributes(&self, indent: usize) -> String {
        let apis = self.api_schemas_for_write();
        let pad = " ".repeat(indent);
        let mut out = String::new();
        self.visit(&mut |spec, data| {
            let wanted = spec.api.is_empty()
                || apis.iter().any(|a| a == spec.api)
                || !data.is_empty()
                || self.authored.contains_key(spec.name);
            if !wanted || (spec.name == "extent" && data.is_empty() && !self.authored.contains_key("extent")) {
                return;
            }
            let _ = write!(out, "{pad}uniform {} {} = ", spec.usd_type, spec.name);
            data.write(&mut out);
            out.push('\n');
        });
        out
    }

    /// A complete `.usda` layer holding this BrepArray as its default prim.
    pub fn to_usda(&self) -> String {
        layer_to_usda(std::slice::from_ref(self))
    }

    fn prim_name(&self) -> &str {
        if self.name.is_empty() { "Brep" } else { &self.name }
    }

    /// The `def BrepArray` block for this prim.
    fn write_prim(&self, s: &mut String) {
        let _ = write!(s, "def BrepArray {} (\n    prepend apiSchemas = [", usda::quote(self.prim_name()));
        for (i, a) in self.api_schemas_for_write().iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&usda::quote(a));
        }
        s.push_str("]\n)\n{\n");
        s.push_str(&self.write_attributes(4));
        s.push_str("}\n");
    }
}

/// A complete `.usda` layer holding each BrepArray as a sibling root prim,
/// the first one being the default prim. Prim names must be unique.
pub fn layer_to_usda(breps: &[BrepArray]) -> String {
    let mut s = String::from("#usda 1.0\n(\n");
    if let Some(first) = breps.first() {
        let _ = writeln!(s, "    defaultPrim = {}", usda::quote(first.prim_name()));
    }
    s.push_str("    metersPerUnit = 1\n    upAxis = \"Z\"\n)\n");
    for b in breps {
        s.push('\n');
        b.write_prim(&mut s);
    }
    s
}

fn curve_api(t: &CurveType, instance_prefix: &str) -> Option<String> {
    let suffix = match t {
        CurveType::Nurbs => "3dNurb",
        CurveType::Line => "3dLine",
        CurveType::Circle => "3dCircle",
        CurveType::Ellipse => "3dEllipse",
        CurveType::Other(_) => return None,
    };
    Some(format!("{}:{instance_prefix}{suffix}", t.as_token()))
}

// ───────────────────────────── Per-Brep ranges ─────────────────────────────

/// The consecutive ranges of each array that belong to one Brep.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrepSpan {
    pub regions: Range<usize>,
    pub shells: Range<usize>,
    pub faceuses: Range<usize>,
    pub faces: Range<usize>,
    pub loops: Range<usize>,
    pub edgeuses: Range<usize>,
    pub wire_edges: Range<usize>,
    pub edges: Range<usize>,
    pub vertices: Range<usize>,
}

fn sum_range(counts: &[u32], r: Range<usize>) -> usize {
    let end = r.end.min(counts.len());
    let start = r.start.min(end);
    counts[start..end].iter().map(|&c| c as usize).sum()
}

impl BrepArray {
    pub fn brep_count(&self) -> usize {
        self.region_count.len()
    }

    /// Partition every array by Brep, following the same rules as NVIDIA's
    /// validator: counts cascade down the hierarchy, faces = faceuses / 2, and
    /// (for multi-Brep arrays) edges and vertices are counted by use.
    pub fn spans(&self) -> Vec<BrepSpan> {
        let n = self.brep_count();
        let mut spans = Vec::with_capacity(n);
        let (mut r0, mut s0, mut fu0, mut f0, mut l0, mut eu0, mut w0, mut e0, mut v0) = (0, 0, 0, 0, 0, 0, 0, 0, 0);
        for b in 0..n {
            let regions = r0..r0 + self.region_count[b] as usize;
            let shells = s0..s0 + sum_range(&self.region_shell_count, regions.clone());
            let faceuses = fu0..fu0 + sum_range(&self.shell_faceuse_count, shells.clone());
            let faces = f0..f0 + faceuses.len() / 2;
            let loops = l0..l0 + sum_range(&self.face_loop_count, faces.clone());
            let edgeuses = eu0..eu0 + sum_range(&self.loop_edgeuse_count, loops.clone());
            let wire_edges = w0..w0 + sum_range(&self.shell_wire_edge_count, shells.clone());
            let (edges, vertices) = if n == 1 {
                (0..self.edge_curve_type.len().max(self.edge_vertex_indices.len()), 0..self.vertex_point_type.len())
            } else {
                let lo = edgeuses.start.min(self.edgeuse_edge_index.len());
                let hi = edgeuses.end.min(self.edgeuse_edge_index.len());
                let used: BTreeSet<u32> = self.edgeuse_edge_index[lo..hi].iter().copied().collect();
                let mut verts = BTreeSet::new();
                for &e in &used {
                    if let Some(p) = self.edge_vertex_indices.get(e as usize) {
                        verts.extend(p.iter().copied());
                    }
                }
                for w in wire_edges.clone() {
                    if let Some(p) = self.wire_edge_vertex_indices.get(w) {
                        verts.extend(p.iter().copied());
                    }
                }
                for l in loops.clone() {
                    if self.loop_edgeuse_count.get(l) == Some(&0)
                        && let Some(&v) = self.loop_vertex_index.get(l) {
                            verts.insert(v as i64);
                        }
                }
                (e0..e0 + used.len(), v0..v0 + verts.len())
            };
            (r0, s0, fu0, f0, l0, eu0, w0, e0, v0) = (
                regions.end, shells.end, faceuses.end, faces.end, loops.end, edgeuses.end, wire_edges.end, edges.end, vertices.end,
            );
            spans.push(BrepSpan { regions, shells, faceuses, faces, loops, edgeuses, wire_edges, edges, vertices });
        }
        spans
    }

    /// For each face: (surface type, index into that surface API's arrays).
    /// Surfaces of each type are packed in the order their faces appear.
    pub fn face_surface_slots(&self) -> Vec<(SurfaceType, usize)> {
        let mut counters: BTreeMap<String, usize> = BTreeMap::new();
        self.face_surface_type
            .iter()
            .map(|t| {
                let c = counters.entry(t.as_token().to_string()).or_default();
                *c += 1;
                (t.clone(), *c - 1)
            })
            .collect()
    }

    /// For each edge: (curve type, index into that curve API's edge arrays).
    pub fn edge_curve_slots(&self) -> Vec<(CurveType, usize)> {
        let mut counters: BTreeMap<String, usize> = BTreeMap::new();
        self.edge_curve_type
            .iter()
            .map(|t| {
                let c = counters.entry(t.as_token().to_string()).or_default();
                *c += 1;
                (t.clone(), *c - 1)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_table_names_are_unique() {
        let mut names: Vec<_> = ATTRIBUTES.iter().map(|a| a.name).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n);
    }

    #[test]
    fn tokens_round_trip_and_keep_unknowns() {
        assert_eq!(RegionType::from_token("solidRegion"), RegionType::Solid);
        assert_eq!(RegionType::from_token("bogus").as_token(), "bogus");
        assert_eq!(SurfaceType::allowed().len(), 6);
    }

    #[test]
    fn write_then_read_is_identity() {
        let mut b = BrepArray { name: "T".into(), ..Default::default() };
        b.region_count = vec![1];
        b.region_type = vec![RegionType::Solid];
        b.extent = vec![[0.0, -1.5, 0.0], [1.0, 2.0, 3e-9]];
        b.edge_vertex_indices = vec![[0, 1]];
        b.edge_curve_type = vec![CurveType::Line];
        b.edge_lines.origin = vec![[0.0; 3]];
        b.edge_lines.direction = vec![[1.0, 0.0, 0.0]];
        let text = b.to_usda();
        let back = &BrepArray::read_all(&text).unwrap()[0];
        assert!(back.read_issues.is_empty(), "{:?}", back.read_issues);
        assert_eq!(back.extent, b.extent);
        assert_eq!(back.edge_lines, b.edge_lines);
        assert_eq!(back.region_type, b.region_type);
        assert!(back.api_schemas.contains(&"BrepCurve3dLineAPI:edge3dLine".to_string()));
    }

    #[test]
    fn layer_holds_several_prims_and_reads_back() {
        let prim = |name: &str, x: f64| {
            let mut b = BrepArray { name: name.into(), ..Default::default() };
            b.region_count = vec![1];
            b.region_type = vec![RegionType::Solid];
            b.vertex_point_type = vec![VertexPointType::BrepPoint];
            b.vertex_points = vec![[x, 0.0, 0.0]];
            b
        };
        let text = layer_to_usda(&[prim("A", 1.0), prim("B", 2.0)]);
        assert!(text.contains("defaultPrim = \"A\""));
        let back = BrepArray::read_all(&text).unwrap();
        assert_eq!(back.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), ["A", "B"]);
        assert_eq!(back[1].vertex_points, vec![[2.0, 0.0, 0.0]]);
        // A one-prim layer is exactly what to_usda writes.
        assert_eq!(layer_to_usda(&[prim("A", 1.0)]), prim("A", 1.0).to_usda());
    }
}
