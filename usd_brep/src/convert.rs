//! Conversion between `mini_brep` solids and BrepArray.
//!
//! Export writes exact analytic geometry: every face becomes a
//! `BrepSurfacePlaneAPI` plane and every edge a `BrepCurve3dLineAPI` line,
//! with optional linear UV trim curves.
//!
//! Conventions (matching NVIDIA's OpenCASCADE importer):
//! * region 0 is the infinite void, region 1 the solid; one shell each;
//! * face normals point out of the solid, so the void shell uses each face
//!   on its `same` side and the solid shell on its `opposite` side;
//! * outer loops run counter-clockwise around the face normal, holes clockwise;
//! * an edgeuse running along its edge is entered from the bottom side of
//!   its face during a right-hand traversal around the edge (`bottomEntry`),
//!   one running against it from the top (`topEntry`).

use crate::model::*;
use mini_brep::{Brep, Curve, Surface, Vec3, VertexId};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// Write a UV trim curve (degree-1 NURBS) for every edgeuse.
    pub uv_curves: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self { uv_curves: true }
    }
}

fn v3(p: Vec3) -> V3 {
    [p.x, p.y, p.z]
}

/// Convert a closed `mini_brep` solid into a single-Brep BrepArray.
pub fn from_mini_brep(solid: &Brep, name: &str, opts: &ExportOptions) -> BrepArray {
    let mut b = BrepArray { name: name.to_string(), ..Default::default() };
    let nf = solid.faces.len();

    // Vertices
    for v in &solid.vertices {
        b.vertex_point_type.push(VertexPointType::BrepPoint);
        b.vertex_points.push(v3(v.point));
    }

    // Edges: exact lines parameterized by arc length.
    for e in &solid.edges {
        let (p0, p1) = (solid.point(e.start), solid.point(e.end));
        let len = (p1 - p0).length();
        let Curve::Line { dir, .. } = &e.curve;
        b.edge_curve_type.push(CurveType::Line);
        b.edge_range.extend([0.0, len]);
        b.edge_vertex_indices.push([e.start.0 as i64, e.end.0 as i64]);
        b.edge_lines.origin.push(v3(p0));
        b.edge_lines.direction.push(v3(dir.normalize()));
    }

    // Regions and shells: void (outside) first, then the solid.
    b.region_count = vec![2];
    b.region_type = vec![RegionType::Void, RegionType::Solid];
    b.region_shell_count = vec![1, 1];
    b.shell_faceuse_count = vec![nf as u32, nf as u32];
    b.shell_wire_edge_count = vec![0, 0];
    b.shell_point_type = vec![ShellPointType::None, ShellPointType::None];
    for (side, orient) in [(0, Orientation::Same), (1, Orientation::Opposite)] {
        let _ = side;
        for f in 0..nf {
            b.faceuse_face_index.push(f as u32);
            b.faceuse_orientation.push(orient.clone());
        }
    }

    // Faces, loops, edgeuses.
    let mut uses_of_edge: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for face in &solid.faces {
        let Surface::Plane { normal, .. } = face.surface;
        let outer_pts = solid.loop_points(&face.outer);
        let origin = outer_pts[0];
        let axis = normal.normalize();
        // refDirection: first edge direction, made exactly orthogonal to the axis.
        let mut refd = (outer_pts[1] - outer_pts[0]).normalize();
        refd = (refd - axis * refd.dot(axis)).normalize();
        let ydir = axis.cross(refd);
        let uv = |p: Vec3| -> V2 { [(p - origin).dot(refd), (p - origin).dot(ydir)] };

        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in &outer_pts {
            let q = uv(*p);
            for a in 0..2 {
                lo[a] = lo[a].min(q[a]);
                hi[a] = hi[a].max(q[a]);
            }
        }
        b.face_surface_type.push(SurfaceType::Plane);
        b.face_trim_type.push(TrimType::General);
        b.face_range.extend([lo, hi]);
        b.face_loop_count.push(1 + face.holes.len() as u32);
        b.planes.origin.push(v3(origin));
        b.planes.axis.push(v3(axis));
        b.planes.ref_direction.push(v3(refd));

        for lp in std::iter::once(&face.outer).chain(&face.holes) {
            b.loop_edgeuse_count.push(lp.coedges.len() as u32);
            b.loop_vertex_index.push(NO_LOOP_VERTEX);
            for ce in &lp.coedges {
                let eu = b.edgeuse_edge_index.len();
                uses_of_edge.entry(ce.edge.0).or_default().push(eu);
                b.edgeuse_edge_index.push(ce.edge.0 as u32);
                if ce.forward {
                    b.edgeuse_orientation.push(Orientation::Same);
                    b.edgeuse_entry.push(RadialEntry::Bottom);
                } else {
                    b.edgeuse_orientation.push(Orientation::Opposite);
                    b.edgeuse_entry.push(RadialEntry::Top);
                }
                b.edgeuse_next_radial.push(0); // filled below
                if opts.uv_curves {
                    let (s, t) = solid.coedge_vertices(*ce);
                    let len = (solid.point(t) - solid.point(s)).length();
                    let c = &mut b.uv_curves;
                    c.order.push(2);
                    c.vertex_count.push(2);
                    c.control_vertices.extend([uv(solid.point(s)), uv(solid.point(t))]);
                    c.weights.extend([1.0, 1.0]);
                    c.knots.extend([0.0, 0.0, len, len]);
                }
            }
        }
    }

    // Radial links: the edgeuses of each edge form a cycle. For a manifold
    // solid there are exactly two, which simply point at each other.
    for uses in uses_of_edge.values() {
        for (k, &eu) in uses.iter().enumerate() {
            b.edgeuse_next_radial[eu] = uses[(k + 1) % uses.len()] as u32;
        }
    }

    // Extent and tolerance.
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &b.vertex_points {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    let diag = (0..3).map(|a| (hi[a] - lo[a]).powi(2)).sum::<f64>().sqrt();
    let tol = 1e-6 * diag.max(1.0);
    for a in 0..3 {
        lo[a] -= tol;
        hi[a] += tol;
    }
    b.intersect_tol_3d = vec![2.0 * tol];
    b.extent = vec![lo, hi];
    // The Gprim extent is float3: round outward so it always contains brep:extent.
    let down = |x: f64| { let f = x as f32; if (f as f64) > x { f64::from(f.next_down()) } else { f as f64 } };
    let up = |x: f64| { let f = x as f32; if (f as f64) < x { f64::from(f.next_up()) } else { f as f64 } };
    b.prim_extent = vec![lo.map(down), hi.map(up)];
    b
}

// ───────────────────────────── Import ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ImportError {
    NoSuchBrep(usize),
    /// The Brep needs features `mini_brep` cannot represent.
    Unsupported(String),
    /// The data does not form a valid closed solid.
    Invalid(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ImportError::NoSuchBrep(i) => write!(f, "no Brep {i} in this BrepArray"),
            ImportError::Unsupported(s) => write!(f, "unsupported: {s}"),
            ImportError::Invalid(s) => write!(f, "invalid: {s}"),
        }
    }
}
impl std::error::Error for ImportError {}

/// Is edge `e` a straight segment (a line, or a degree-1 NURBS with 2 points)?
fn edge_is_straight(b: &BrepArray, slots: &[(CurveType, usize)], e: usize) -> bool {
    match slots.get(e) {
        Some((CurveType::Line, _)) => true,
        Some((CurveType::Nurbs, k)) => b.edge_nurbs.order.get(*k) == Some(&2) && b.edge_nurbs.vertex_count.get(*k) == Some(&2),
        _ => false,
    }
}

/// Convert one Brep of a BrepArray into a `mini_brep` solid.
///
/// Supported: a single solid region bounded by one shell of planar faces with
/// straight edges (exact planes, or flat NURBS patches such as the ones SMLib
/// writes for boxes). Curved geometry and non-manifold models are rejected.
pub fn to_mini_brep(b: &BrepArray, brep: usize) -> Result<Brep, ImportError> {
    let spans = b.spans();
    let span = spans.get(brep).ok_or(ImportError::NoSuchBrep(brep))?;
    let solids: Vec<usize> = span.regions.clone().filter(|&r| b.region_type.get(r) == Some(&RegionType::Solid)).collect();
    if solids.len() != 1 {
        return Err(ImportError::Unsupported(format!("{} solid regions (exactly 1 supported)", solids.len())));
    }
    // Locate the solid region's outer shell and its faceuses.
    let mut shell = span.shells.start;
    for r in span.regions.start..solids[0] {
        shell += b.region_shell_count[r] as usize;
    }
    if b.region_shell_count[solids[0]] != 1 {
        return Err(ImportError::Unsupported("solid region with inner (void) shells".into()));
    }
    let fu_start = span.faceuses.start + b.shell_faceuse_count[span.shells.start..shell].iter().map(|&c| c as usize).sum::<usize>();
    let fu_range = fu_start..fu_start + b.shell_faceuse_count[shell] as usize;

    // Loop and edgeuse offsets per face.
    let mut loop_start = vec![0usize; b.face_loop_count.len() + 1];
    for (f, &c) in b.face_loop_count.iter().enumerate() {
        loop_start[f + 1] = loop_start[f] + c as usize;
    }
    let mut eu_start = vec![0usize; b.loop_edgeuse_count.len() + 1];
    for (l, &c) in b.loop_edgeuse_count.iter().enumerate() {
        eu_start[l + 1] = eu_start[l] + c as usize;
    }
    let slots = b.edge_curve_slots();

    let mut out = Brep::new();
    let mut vmap: BTreeMap<i64, VertexId> = BTreeMap::new();
    let mut vertex = |out: &mut Brep, v: i64| -> Result<VertexId, ImportError> {
        if let Some(&id) = vmap.get(&v) {
            return Ok(id);
        }
        let p = b.vertex_points.get(v as usize).ok_or_else(|| ImportError::Invalid(format!("vertex {v} has no position")))?;
        let id = out.add_vertex(Vec3::new(p[0], p[1], p[2]));
        vmap.insert(v, id);
        Ok(id)
    };

    let mut faces = Vec::new();
    for fu in fu_range {
        let f = *b.faceuse_face_index.get(fu).ok_or_else(|| ImportError::Invalid(format!("faceuse {fu} missing")))? as usize;
        let side = b.faceuse_orientation.get(fu).cloned().unwrap_or(Orientation::Same);
        match b.face_surface_type.get(f) {
            Some(SurfaceType::Plane | SurfaceType::Nurbs) => {}
            other => return Err(ImportError::Unsupported(format!("face {f} has surface {other:?}"))),
        }
        // Loops run CCW around the surface normal. When the solid sees the face
        // from its `same` side, the normal points into the solid: reverse.
        let reverse = side == Orientation::Same;
        let mut loops: Vec<Vec<VertexId>> = Vec::new();
        for l in loop_start[f]..loop_start[f + 1] {
            let mut ids = Vec::new();
            for eu in eu_start[l]..eu_start[l + 1] {
                let e = b.edgeuse_edge_index[eu] as usize;
                if !edge_is_straight(b, &slots, e) {
                    return Err(ImportError::Unsupported(format!("edge {e} is curved")));
                }
                let [s, t] = b.edge_vertex_indices[e];
                let start = if b.edgeuse_orientation[eu] == Orientation::Opposite { t } else { s };
                ids.push(vertex(&mut out, start)?);
            }
            if ids.len() < 3 {
                return Err(ImportError::Unsupported(format!("face {f} has a loop with {} edges", ids.len())));
            }
            if reverse {
                ids.reverse();
            }
            loops.push(ids);
        }
        let (outer, holes) = loops.split_first().ok_or_else(|| ImportError::Invalid(format!("face {f} has no loops")))?;
        let hole_refs: Vec<&[VertexId]> = holes.iter().map(Vec::as_slice).collect();
        faces.push(out.add_planar_face(outer, &hole_refs));
    }
    out.add_shell(faces);
    out.validate().map_err(|e| ImportError::Invalid(format!("{e:?}")))?;
    if out.volume() <= 0.0 {
        return Err(ImportError::Invalid("faces are oriented inward (negative volume)".into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::{is_valid, validate};

    fn hexagon_prism() -> Brep {
        let pts: Vec<Vec3> = (0..6)
            .map(|k| {
                let a = std::f64::consts::TAU * k as f64 / 6.0;
                Vec3::new(5.0 * a.cos(), 5.0 * a.sin(), 0.0)
            })
            .collect();
        Brep::extrude_polygon(&pts, 3.0)
    }

    #[test]
    fn exported_box_has_expected_topology() {
        let solid = Brep::make_box(Vec3::new(0.0, 0.0, 0.0), 2.0, 3.0, 4.0);
        let b = from_mini_brep(&solid, "Box", &ExportOptions::default());
        assert_eq!(b.faceuse_face_index.len(), 12);
        assert_eq!(b.edgeuse_edge_index.len(), 24);
        assert_eq!(b.edge_curve_type.len(), 12);
        // Every edge has one same and one opposite use, linked to each other.
        for eu in 0..24 {
            let mate = b.edgeuse_next_radial[eu] as usize;
            assert_eq!(b.edgeuse_next_radial[mate] as usize, eu);
            assert_eq!(b.edgeuse_edge_index[mate], b.edgeuse_edge_index[eu]);
            assert_ne!(b.edgeuse_orientation[mate], b.edgeuse_orientation[eu]);
        }
    }

    #[test]
    fn exports_pass_validation_after_writing_and_reading() {
        for (name, solid) in [
            ("Box", Brep::make_box(Vec3::new(1.0, -2.0, 0.5), 2.0, 3.0, 4.0)),
            ("Hex", hexagon_prism()),
        ] {
            let text = from_mini_brep(&solid, name, &ExportOptions::default()).to_usda();
            let back = BrepArray::read_all(&text).unwrap().remove(0);
            let issues = validate(&back);
            assert!(is_valid(&issues) && issues.is_empty(), "{name}: {issues:#?}");
        }
    }

    #[test]
    fn solid_with_through_hole_exports_validates_and_imports() {
        let sq = |h: f64| -> Vec<Vec3> {
            [(-h, -h), (h, -h), (h, h), (-h, h)].iter().map(|&(x, y)| Vec3::new(x, y, 0.0)).collect()
        };
        let solid = Brep::extrude_profile(&sq(2.0), &[&sq(1.0)], 1.0);
        let text = from_mini_brep(&solid, "Ring", &ExportOptions::default()).to_usda();
        let b = BrepArray::read_all(&text).unwrap().remove(0);
        let issues = validate(&b);
        assert!(issues.is_empty(), "{issues:#?}");
        assert_eq!(b.face_loop_count.iter().filter(|&&c| c == 2).count(), 2, "both caps carry a hole loop");
        let back = to_mini_brep(&b, 0).unwrap();
        assert_eq!(back.genus(), Some(1));
        assert!((back.volume() - 12.0).abs() < 1e-9);
        assert_eq!(back.counts(), solid.counts());
    }

    #[test]
    fn round_trip_preserves_volume_and_area() {
        let p = |x, y| Vec3::new(x, y, 0.0);
        let l_shape = Brep::extrude_polygon(&[p(0.0, 0.0), p(3.0, 0.0), p(3.0, 1.0), p(1.0, 1.0), p(1.0, 3.0), p(0.0, 3.0)], 2.0);
        for solid in [l_shape, hexagon_prism()] {
            let b = from_mini_brep(&solid, "S", &ExportOptions::default());
            let back = to_mini_brep(&b, 0).unwrap();
            assert!((back.volume() - solid.volume()).abs() < 1e-9);
            assert!((back.surface_area() - solid.surface_area()).abs() < 1e-9);
            assert_eq!(back.counts(), solid.counts());
        }
    }
}
