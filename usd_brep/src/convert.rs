//! Conversion between `mini_brep` solids and BrepArray.
//!
//! Export writes exact analytic geometry: planes and cylinders
//! (`BrepSurfacePlaneAPI`, `BrepSurfaceCylinderAPI`), lines and circles
//! (`BrepCurve3dLineAPI`, `BrepCurve3dCircleAPI`), with optional UV trim
//! curves (degree 1 for lines, rational degree 2 for arcs). mini_brep uses
//! the schema's own parameterisations, so values are copied, not converted.
//!
//! Conventions (matching NVIDIA's OpenCASCADE importer):
//! * region 0 is the infinite void, region 1 the solid; one shell each;
//! * the void shell sees each face from outside the solid: on the surface's
//!   `same` side when its normal points out (`BrepTopologyBuilder.cpp`
//!   AddStagedFaceusePairForCurrentTraversal), `opposite` otherwise, as for
//!   a round hole's wall;
//! * loops run counter-clockwise around the surface's own normal, holes
//!   clockwise. A face whose normal points into the solid has its loops
//!   reversed (`OcctStagingDriver.cpp`, `bReverseLoop`);
//! * an edgeuse running along its edge is entered from the bottom side of
//!   its face during a right-hand traversal around the edge (`bottomEntry`),
//!   one running against it from the top (`topEntry`);
//! * a full circle is one edge from a vertex back to itself, and a cylinder
//!   face is closed by a seam edge it uses twice, as in NVIDIA's
//!   `Cone_SmConnect_Brep.usda`.

use crate::model::*;
use mini_brep::{Brep, Curve, EdgeId, Surface, UvCurve, Vec3, VertexId, plane_x};
use std::collections::BTreeMap;
use std::f64::consts::{FRAC_PI_2, PI, TAU};

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

/// Angles in the sweep t0 → t1 where coordinate k of
/// center + r (cos t · x + sin t · y) is at a maximum or minimum.
fn extreme_angles(t0: f64, t1: f64, xk: f64, yk: f64) -> impl Iterator<Item = f64> {
    let (lo, hi) = if t0 <= t1 { (t0, t1) } else { (t1, t0) };
    let a = yk.atan2(xk);
    [a, a + PI].into_iter().map(move |t| lo + (t - lo).rem_euclid(TAU)).filter(move |&t| t <= hi)
}

/// Points that bound a UV curve: its ends and an arc's extremes.
fn uv_bounding_points(c: &UvCurve) -> Vec<V2> {
    let mut pts = vec![c.start(), c.end()];
    if let UvCurve::Arc { x, y, t0, t1, .. } = *c {
        for k in 0..2 {
            pts.extend(extreme_angles(t0, t1, x[k], y[k]).map(|t| c.point_at(t)));
        }
    }
    pts
}

/// Appends one UV trim curve. `span` is the knot range: the edge's
/// parameter length, as for lines before.
fn push_uv_curve(c: &mut NurbsCurves2d, curve: &UvCurve, span: f64) {
    match *curve {
        UvCurve::Line { a, b } => {
            c.order.push(2);
            c.vertex_count.push(2);
            c.control_vertices.extend([a, b]);
            c.weights.extend([1.0, 1.0]);
            c.knots.extend([0.0, 0.0, span, span]);
        }
        // Rational quadratic arcs of at most 90° each: ends on the circle,
        // middle point where their tangents meet, with weight cos(half angle).
        UvCurve::Arc { center, radius, x, y, t0, t1 } => {
            let pieces = ((t1 - t0).abs() / FRAC_PI_2 - 1e-9).ceil().max(1.0) as usize;
            let step = (t1 - t0) / pieces as f64;
            let w = (step / 2.0).cos();
            let at = |t: f64, r: f64| [center[0] + r * (t.cos() * x[0] + t.sin() * y[0]), center[1] + r * (t.cos() * x[1] + t.sin() * y[1])];
            c.order.push(3);
            c.vertex_count.push(2 * pieces as u32 + 1);
            c.control_vertices.push(at(t0, radius));
            c.weights.push(1.0);
            c.knots.extend([0.0; 3]);
            for i in 0..pieces {
                let ti = t0 + step * i as f64;
                c.control_vertices.extend([at(ti + step / 2.0, radius / w), at(ti + step, radius)]);
                c.weights.extend([w, 1.0]);
                let k = span * (i + 1) as f64 / pieces as f64;
                c.knots.extend(if i + 1 < pieces { vec![k, k] } else { vec![k, k, k] });
            }
        }
    }
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

    // Edges: exact lines (parameter = arc length) and circles (parameter = angle).
    for (i, e) in solid.edges.iter().enumerate() {
        let (t0, t1) = solid.edge_range(EdgeId(i));
        b.edge_range.extend([t0, t1]);
        b.edge_vertex_indices.push([e.start.0 as i64, e.end.0 as i64]);
        match e.curve {
            Curve::Line { origin, dir } => {
                b.edge_curve_type.push(CurveType::Line);
                b.edge_lines.origin.push(v3(origin));
                b.edge_lines.direction.push(v3(dir.normalize()));
            }
            Curve::Circle { center, axis, ref_dir, radius } => {
                b.edge_curve_type.push(CurveType::Circle);
                b.edge_circles.center.push(v3(center));
                b.edge_circles.axis.push(v3(axis));
                b.edge_circles.ref_direction.push(v3(ref_dir));
                b.edge_circles.radius.push(radius);
            }
        }
    }

    // Regions and shells: void (outside) first, then the solid. Each face is
    // seen from outside by the void shell and from inside by the solid shell.
    b.region_count = vec![2];
    b.region_type = vec![RegionType::Void, RegionType::Solid];
    b.region_shell_count = vec![1, 1];
    b.shell_faceuse_count = vec![nf as u32, nf as u32];
    b.shell_wire_edge_count = vec![0, 0];
    b.shell_point_type = vec![ShellPointType::None, ShellPointType::None];
    for outside in [true, false] {
        for (f, face) in solid.faces.iter().enumerate() {
            b.faceuse_face_index.push(f as u32);
            b.faceuse_orientation.push(if face.sense == outside { Orientation::Same } else { Orientation::Opposite });
        }
    }

    // Faces, loops, edgeuses.
    let mut uses_of_edge: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for face in &solid.faces {
        match face.surface {
            Surface::Plane { origin, normal } => {
                b.face_surface_type.push(SurfaceType::Plane);
                b.planes.origin.push(v3(origin));
                b.planes.axis.push(v3(normal));
                b.planes.ref_direction.push(v3(plane_x(normal)));
            }
            Surface::Cylinder { origin, axis, ref_dir, radius } => {
                b.face_surface_type.push(SurfaceType::Cylinder);
                b.cylinders.origin.push(v3(origin));
                b.cylinders.axis.push(v3(axis));
                b.cylinders.ref_direction.push(v3(ref_dir));
                b.cylinders.radius.push(radius);
            }
        }
        b.face_trim_type.push(TrimType::General);
        b.face_loop_count.push(1 + face.holes.len() as u32);

        let uv_loops = solid.face_uv_loops(face);
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in uv_loops.iter().flatten().flat_map(uv_bounding_points) {
            for a in 0..2 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        b.face_range.extend([lo, hi]);

        for (lp, uv) in std::iter::once(&face.outer).chain(&face.holes).zip(uv_loops) {
            let mut uses: Vec<_> = lp.coedges.iter().copied().zip(uv).collect();
            // Loops are stored round the surface normal; ours run round the
            // outward normal, so reverse them where the two differ.
            if !face.sense {
                uses.reverse();
                for (ce, c) in &mut uses {
                    ce.forward = !ce.forward;
                    *c = c.reversed();
                }
            }
            b.loop_edgeuse_count.push(uses.len() as u32);
            b.loop_vertex_index.push(NO_LOOP_VERTEX);
            for (ce, c) in uses {
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
                    let (t0, t1) = solid.edge_range(ce.edge);
                    push_uv_curve(&mut b.uv_curves, &c, t1 - t0);
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

    // Extent: vertices plus the extremes of every circle, then tolerance.
    let mut pts = b.vertex_points.clone();
    for (i, e) in solid.edges.iter().enumerate() {
        if let Curve::Circle { axis, ref_dir, .. } = e.curve {
            let (t0, t1) = solid.edge_range(EdgeId(i));
            let y = axis.cross(ref_dir);
            for k in 0..3 {
                let (xk, yk) = (v3(ref_dir)[k], v3(y)[k]);
                pts.extend(extreme_angles(t0, t1, xk, yk).map(|t| v3(e.curve.point_at(t))));
            }
        }
    }
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &pts {
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

    fn written_and_read_back(solid: &Brep, name: &str) -> BrepArray {
        let text = from_mini_brep(solid, name, &ExportOptions::default()).to_usda();
        BrepArray::read_all(&text).unwrap().remove(0)
    }

    #[test]
    fn cylinder_exports_like_nvidia_closed_surfaces() {
        let b = written_and_read_back(&Brep::make_cylinder(Vec3::new(1.0, 2.0, 0.0), 2.0, 3.0), "Cyl");
        let issues = validate(&b);
        assert!(issues.is_empty(), "{issues:#?}");
        assert_eq!(b.face_surface_type.iter().filter(|t| **t == SurfaceType::Cylinder).count(), 1);
        // Both circles are closed edges: one vertex at each end, a full turn of range.
        let circles: Vec<usize> = (0..b.edge_curve_type.len()).filter(|&e| b.edge_curve_type[e] == CurveType::Circle).collect();
        assert_eq!(circles.len(), 2);
        for &e in &circles {
            let [s, t] = b.edge_vertex_indices[e];
            assert_eq!(s, t);
            assert!((b.edge_range[2 * e + 1] - b.edge_range[2 * e] - TAU).abs() < 1e-12);
        }
        // The seam is used twice by the cylinder face (face 2), once each way.
        let seam = (0..b.edge_curve_type.len()).find(|&e| b.edge_curve_type[e] == CurveType::Line).unwrap();
        let seam_uses: Vec<usize> = (0..b.edgeuse_edge_index.len()).filter(|&u| b.edgeuse_edge_index[u] as usize == seam).collect();
        assert_eq!(seam_uses.len(), 2);
        assert_ne!(b.edgeuse_orientation[seam_uses[0]], b.edgeuse_orientation[seam_uses[1]]);
        // The extent reaches round the circles, not just to their vertices.
        assert!(b.extent[0][1] < 0.0 + 1e-6 && b.extent[1][1] > 4.0 - 1e-6, "{:?}", b.extent);
    }

    #[test]
    fn hole_wall_is_seen_from_its_opposite_side() {
        let hole = mini_brep::Ring::Circle { center: Vec3::new(0.0, 0.0, 0.0), radius: 1.0 };
        let sq = [(-2.0, -2.0), (2.0, -2.0), (2.0, 2.0), (-2.0, 2.0)].map(|(x, y)| Vec3::new(x, y, 0.0));
        let solid = Brep::extrude(&mini_brep::Ring::Polygon(sq.to_vec()), &[hole], 1.0);
        let b = written_and_read_back(&solid, "Plate");
        let issues = validate(&b);
        assert!(issues.is_empty(), "{issues:#?}");
        // Faceuses 0..7 belong to the void shell; face 6 is the hole's wall.
        assert_eq!(b.faceuse_orientation[6], Orientation::Opposite);
        assert_eq!(b.faceuse_orientation[7 + 6], Orientation::Same);
        assert_eq!(b.face_loop_count.iter().filter(|&&c| c == 2).count(), 2, "both caps carry the hole");
    }

    #[test]
    fn rational_uv_arcs_lie_on_their_circle() {
        let mut c = NurbsCurves2d::default();
        let arc = UvCurve::Arc { center: [1.0, -1.0], radius: 2.0, x: [1.0, 0.0], y: [0.0, 1.0], t0: 0.3, t1: 0.3 - TAU };
        push_uv_curve(&mut c, &arc, TAU);
        assert_eq!((c.order[0], c.vertex_count[0]), (3, 9), "a full turn in four quarter arcs");
        for k in 0..=40 {
            let t = TAU * k as f64 / 40.0;
            let p = crate::validate::nurbs_eval(3, &c.knots, &c.control_vertices, &c.weights, t).unwrap();
            let r = ((p[0] - 1.0).powi(2) + (p[1] + 1.0).powi(2)).sqrt();
            assert!((r - 2.0).abs() < 1e-12, "t {t}: radius {r}");
        }
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
