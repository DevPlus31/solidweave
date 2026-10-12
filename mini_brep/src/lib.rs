//! mini_brep — a small, dependency-free Boundary Representation (B-rep) in Rust.
//!
//! Structure:
//!   Geometry : Point (Vec3), Curve (Line), Surface (Plane)
//!   Topology : Solid → Shell → Face → Loop → CoEdge → Edge → Vertex
//!   Faces may have holes; solids may have through-holes (genus > 0).
//!
//! Topology lives in flat arenas (Vec) addressed by typed ids, which avoids
//! Rc<RefCell<..>> graphs and keeps the structure easy to serialize and copy.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::ops::{Add, Mul, Sub};

// ───────────────────────────── Math ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }
    pub fn dot(self, o: Self) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    pub fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    pub fn length(self) -> f64 {
        self.dot(self).sqrt()
    }
    pub fn normalize(self) -> Self {
        let l = self.length();
        if l < TOLERANCE { self } else { self * (1.0 / l) }
    }
}

impl Add for Vec3 {
    type Output = Self;
    fn add(self, o: Self) -> Self { Self::new(self.x + o.x, self.y + o.y, self.z + o.z) }
}
impl Sub for Vec3 {
    type Output = Self;
    fn sub(self, o: Self) -> Self { Self::new(self.x - o.x, self.y - o.y, self.z - o.z) }
}
impl Mul<f64> for Vec3 {
    type Output = Self;
    fn mul(self, s: f64) -> Self { Self::new(self.x * s, self.y * s, self.z * s) }
}

/// Global modeling tolerance (model units).
pub const TOLERANCE: f64 = 1e-9;

// ─────────────────────────── Geometry ───────────────────────────

/// Exact curve an edge lies on.
#[derive(Debug, Clone)]
pub enum Curve {
    Line { origin: Vec3, dir: Vec3 },
    // Circle { center, normal, radius }, Nurbs { .. } would go here.
}

/// Exact surface a face lies on.
#[derive(Debug, Clone)]
pub enum Surface {
    /// Normal points OUT of the solid.
    Plane { origin: Vec3, normal: Vec3 },
    // Cylinder { .. }, Sphere { .. }, Nurbs { .. } would go here.
}

// ─────────────────────────── Topology ───────────────────────────

macro_rules! id_type {
    ($($name:ident),*) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub usize);
    )*};
}
id_type!(VertexId, EdgeId, FaceId, ShellId);

#[derive(Debug, Clone)]
pub struct Vertex {
    pub point: Vec3,
}

/// An edge is a bounded piece of a curve between two vertices.
/// It is shared by the two faces on either side of it.
#[derive(Debug, Clone)]
pub struct Edge {
    pub curve: Curve,
    pub start: VertexId,
    pub end: VertexId,
}

/// A face's use of an edge, with direction. In a valid closed shell every
/// edge is used exactly twice: once forward, once reversed.
#[derive(Debug, Clone, Copy)]
pub struct CoEdge {
    pub edge: EdgeId,
    pub forward: bool,
}

/// A closed ring of coedges. Outer loops run counter-clockwise when viewed
/// from outside the solid (against the face normal); holes run clockwise.
#[derive(Debug, Clone, Default)]
pub struct Loop {
    pub coedges: Vec<CoEdge>,
}

#[derive(Debug, Clone)]
pub struct Face {
    pub surface: Surface,
    pub outer: Loop,
    pub holes: Vec<Loop>,
}

#[derive(Debug, Clone, Default)]
pub struct Shell {
    pub faces: Vec<FaceId>,
}

/// The whole model: arenas for every entity plus the list of shells.
#[derive(Debug, Clone, Default)]
pub struct Brep {
    pub vertices: Vec<Vertex>,
    pub edges: Vec<Edge>,
    pub faces: Vec<Face>,
    pub shells: Vec<Shell>,
    /// Lookup so two faces that share a vertex pair share one edge.
    edge_lookup: HashMap<(VertexId, VertexId), EdgeId>,
}

// ─────────────────────────── Queries ────────────────────────────

impl Brep {
    pub fn point(&self, v: VertexId) -> Vec3 {
        self.vertices[v.0].point
    }

    /// (start, end) vertex of a coedge, respecting its direction.
    pub fn coedge_vertices(&self, ce: CoEdge) -> (VertexId, VertexId) {
        let e = &self.edges[ce.edge.0];
        if ce.forward { (e.start, e.end) } else { (e.end, e.start) }
    }

    /// Ordered vertex positions around a loop.
    pub fn loop_points(&self, lp: &Loop) -> Vec<Vec3> {
        lp.coedges.iter().map(|&ce| self.point(self.coedge_vertices(ce).0)).collect()
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        (self.vertices.len(), self.edges.len(), self.faces.len())
    }
}

// ─────────────────────────── Building ───────────────────────────

impl Brep {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_vertex(&mut self, p: Vec3) -> VertexId {
        self.vertices.push(Vertex { point: p });
        VertexId(self.vertices.len() - 1)
    }

    /// Returns a coedge from `a` to `b`, reusing an existing edge if the
    /// neighbouring face already created one (then it is used reversed).
    fn coedge_between(&mut self, a: VertexId, b: VertexId) -> CoEdge {
        if let Some(&id) = self.edge_lookup.get(&(a, b)) {
            return CoEdge { edge: id, forward: true };
        }
        if let Some(&id) = self.edge_lookup.get(&(b, a)) {
            return CoEdge { edge: id, forward: false };
        }
        let (pa, pb) = (self.point(a), self.point(b));
        self.edges.push(Edge {
            curve: Curve::Line { origin: pa, dir: (pb - pa).normalize() },
            start: a,
            end: b,
        });
        let id = EdgeId(self.edges.len() - 1);
        self.edge_lookup.insert((a, b), id);
        CoEdge { edge: id, forward: true }
    }

    fn make_loop(&mut self, verts: &[VertexId]) -> Loop {
        let n = verts.len();
        Loop {
            coedges: (0..n).map(|i| self.coedge_between(verts[i], verts[(i + 1) % n])).collect(),
        }
    }

    /// Adds a planar face. `outer` must be counter-clockwise seen from outside;
    /// each hole clockwise. The plane normal is computed from the outer loop.
    pub fn add_planar_face(&mut self, outer: &[VertexId], holes: &[&[VertexId]]) -> FaceId {
        assert!(outer.len() >= 3, "a face needs at least 3 vertices");
        let pts: Vec<Vec3> = outer.iter().map(|&v| self.point(v)).collect();
        let normal = newell_normal(&pts);
        let outer_loop = self.make_loop(outer);
        let hole_loops = holes.iter().map(|h| self.make_loop(h)).collect();
        self.faces.push(Face {
            surface: Surface::Plane { origin: pts[0], normal },
            outer: outer_loop,
            holes: hole_loops,
        });
        FaceId(self.faces.len() - 1)
    }

    pub fn add_shell(&mut self, faces: Vec<FaceId>) -> ShellId {
        self.shells.push(Shell { faces });
        ShellId(self.shells.len() - 1)
    }

    /// Extrudes a closed planar profile (lying at z = profile z) straight up
    /// by `height`. Produces a prism.
    pub fn extrude_polygon(profile: &[Vec3], height: f64) -> Self {
        Self::extrude_profile(profile, &[], height)
    }

    /// Extrudes a planar profile with holes straight up by `height`. Each
    /// hole becomes a through-hole, which adds one to the genus. Loops may
    /// run either way: the outer one is made counter-clockwise seen from +Z
    /// and the holes clockwise.
    pub fn extrude_profile(outer: &[Vec3], holes: &[&[Vec3]], height: f64) -> Self {
        assert!(outer.len() >= 3 && holes.iter().all(|h| h.len() >= 3) && height > TOLERANCE);
        let mut b = Brep::new();
        let up = Vec3::new(0.0, 0.0, height);
        let oriented = |pts: &[Vec3], ccw: bool| -> Vec<Vec3> {
            if (signed_area_xy(pts) > 0.0) == ccw { pts.to_vec() } else { pts.iter().rev().copied().collect() }
        };
        let rings: Vec<Vec<Vec3>> =
            std::iter::once(oriented(outer, true)).chain(holes.iter().map(|h| oriented(h, false))).collect();

        let bottom: Vec<Vec<VertexId>> = rings.iter().map(|r| r.iter().map(|&p| b.add_vertex(p)).collect()).collect();
        let top: Vec<Vec<VertexId>> = rings.iter().map(|r| r.iter().map(|&p| b.add_vertex(p + up)).collect()).collect();

        let mut faces = Vec::new();
        // Bottom faces down (-Z), so every loop on it is reversed.
        let bottom_rev: Vec<Vec<VertexId>> = bottom.iter().map(|r| r.iter().rev().copied().collect()).collect();
        let hole_refs: Vec<&[VertexId]> = bottom_rev[1..].iter().map(Vec::as_slice).collect();
        faces.push(b.add_planar_face(&bottom_rev[0], &hole_refs));
        // Top faces up (+Z).
        let hole_refs: Vec<&[VertexId]> = top[1..].iter().map(Vec::as_slice).collect();
        faces.push(b.add_planar_face(&top[0], &hole_refs));
        // Side quads. Around a hole the same winding makes the wall face into
        // the hole, which is away from the material.
        for (bot, top) in bottom.iter().zip(&top) {
            let n = bot.len();
            for i in 0..n {
                let j = (i + 1) % n;
                faces.push(b.add_planar_face(&[bot[i], bot[j], top[j], top[i]], &[]));
            }
        }
        b.add_shell(faces);
        b
    }

    /// Axis-aligned box from `min` with size (dx, dy, dz).
    pub fn make_box(min: Vec3, dx: f64, dy: f64, dz: f64) -> Self {
        let p = |x, y| Vec3::new(min.x + x, min.y + y, min.z);
        Self::extrude_polygon(&[p(0.0, 0.0), p(dx, 0.0), p(dx, dy), p(0.0, dy)], dz)
    }
}

/// Robust polygon normal (Newell's method); works for non-convex polygons.
fn newell_normal(pts: &[Vec3]) -> Vec3 {
    let mut n = Vec3::new(0.0, 0.0, 0.0);
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        n.x += (a.y - b.y) * (a.z + b.z);
        n.y += (a.z - b.z) * (a.x + b.x);
        n.z += (a.x - b.x) * (a.y + b.y);
    }
    n.normalize()
}

/// Twice the signed area of a polygon's projection on XY (positive = CCW).
fn signed_area_xy(pts: &[Vec3]) -> f64 {
    (0..pts.len()).map(|i| {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        a.x * b.y - b.x * a.y
    }).sum()
}

// ────────────────────────── Validation ──────────────────────────

#[derive(Debug, PartialEq)]
pub enum BrepError {
    OpenLoop { face: FaceId },
    EdgeUseCount { edge: EdgeId, uses: usize },
    EdgeSameDirection { edge: EdgeId },
    NonPlanarFace { face: FaceId, deviation: f64 },
    /// V − E + F − H of a shell is odd or above 2, so it cannot be one closed
    /// surface of any genus.
    EulerMismatch { shell: ShellId, v: usize, e: usize, f: usize, holes: usize },
}

impl Brep {
    /// V − E + F − H for one shell (H = inner loops), counting only the
    /// vertices and edges its faces use.
    fn shell_euler(&self, shell: &Shell) -> (usize, usize, usize, usize, i64) {
        let mut edges = std::collections::BTreeSet::new();
        let (mut f, mut holes) = (0, 0);
        // Ids that point past the arena are skipped; the edge-use check reports the gap.
        for face in shell.faces.iter().filter_map(|f| self.faces.get(f.0)) {
            f += 1;
            holes += face.holes.len();
            for lp in std::iter::once(&face.outer).chain(&face.holes) {
                edges.extend(lp.coedges.iter().map(|ce| ce.edge));
            }
        }
        let verts: std::collections::BTreeSet<VertexId> =
            edges.iter().flat_map(|e| [self.edges[e.0].start, self.edges[e.0].end]).collect();
        let (v, e) = (verts.len(), edges.len());
        (v, e, f, holes, v as i64 - e as i64 + f as i64 - holes as i64)
    }

    /// Number of through-holes ("handles") of a closed shell, from
    /// Euler–Poincaré: V − E + F − H = 2 − 2·genus. A cube has genus 0, a
    /// ring genus 1. None if the counts fit no closed surface.
    pub fn shell_genus(&self, shell: ShellId) -> Option<usize> {
        let (.., chi) = self.shell_euler(&self.shells[shell.0]);
        (chi <= 2 && chi % 2 == 0).then(|| ((2 - chi) / 2) as usize)
    }

    /// Total genus over all shells.
    pub fn genus(&self) -> Option<usize> {
        (0..self.shells.len()).map(|s| self.shell_genus(ShellId(s))).sum()
    }

    /// Checks that the B-rep is a valid closed (watertight) solid.
    pub fn validate(&self) -> Result<(), Vec<BrepError>> {
        let mut errors = Vec::new();
        let mut uses: HashMap<EdgeId, (usize, usize)> = HashMap::new(); // (forward, reversed)

        for (fi, face) in self.faces.iter().enumerate() {
            let fid = FaceId(fi);
            for lp in std::iter::once(&face.outer).chain(face.holes.iter()) {
                // 1. Each loop must be closed: end of coedge i == start of coedge i+1.
                let n = lp.coedges.len();
                for i in 0..n {
                    let (_, end) = self.coedge_vertices(lp.coedges[i]);
                    let (start, _) = self.coedge_vertices(lp.coedges[(i + 1) % n]);
                    if end != start {
                        errors.push(BrepError::OpenLoop { face: fid });
                        break;
                    }
                }
                for ce in &lp.coedges {
                    let u = uses.entry(ce.edge).or_default();
                    if ce.forward { u.0 += 1 } else { u.1 += 1 }
                }
                // 2. Vertices must lie on the face's surface.
                let Surface::Plane { origin, normal } = &face.surface;
                for p in self.loop_points(lp) {
                    let d = (p - *origin).dot(*normal).abs();
                    if d > 1e-6 {
                        errors.push(BrepError::NonPlanarFace { face: fid, deviation: d });
                        break;
                    }
                }
            }
        }

        // 3. Manifold & consistently oriented: each edge used once each way.
        for ei in 0..self.edges.len() {
            let id = EdgeId(ei);
            let (f, r) = uses.get(&id).copied().unwrap_or((0, 0));
            if f + r != 2 {
                errors.push(BrepError::EdgeUseCount { edge: id, uses: f + r });
            } else if f != 1 {
                errors.push(BrepError::EdgeSameDirection { edge: id });
            }
        }

        // 4. Euler–Poincaré per shell: V − E + F − H = 2 − 2·genus, so it must
        //    be even and at most 2. This cannot tell a through-hole from a
        //    defect with the same counts; checks 1–3 cover much of that.
        for (s, shell) in self.shells.iter().enumerate() {
            let (v, e, f, holes, chi) = self.shell_euler(shell);
            if chi > 2 || chi % 2 != 0 {
                errors.push(BrepError::EulerMismatch { shell: ShellId(s), v, e, f, holes });
            }
        }

        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }
}

// ───────────────────── Mass properties & mesh ───────────────────

impl Brep {
    /// Triangulates every face, concave faces and holes included. Triangles
    /// wind counter-clockwise seen from outside, so they face out of the solid.
    pub fn tessellate(&self) -> Vec<[Vec3; 3]> {
        let mut tris = Vec::new();
        for face in &self.faces {
            let Surface::Plane { normal, .. } = face.surface;
            let rings: Vec<Vec<Vec3>> =
                std::iter::once(&face.outer).chain(&face.holes).map(|lp| self.loop_points(lp)).collect();
            tris.extend(triangulate_planar(&rings, normal));
        }
        tris
    }

    /// Enclosed volume via the divergence theorem (sum of signed tetrahedra).
    /// A signed fan over every loop is exact for any planar face: holes run
    /// the other way, so their part is subtracted.
    pub fn volume(&self) -> f64 {
        self.faces
            .iter()
            .flat_map(|face| std::iter::once(&face.outer).chain(&face.holes))
            .map(|lp| {
                let pts = self.loop_points(lp);
                (1..pts.len() - 1).map(|i| pts[0].dot(pts[i].cross(pts[i + 1])) / 6.0).sum::<f64>()
            })
            .sum()
    }

    pub fn surface_area(&self) -> f64 {
        self.faces
            .iter()
            .map(|face| {
                let mut sum = Vec3::new(0.0, 0.0, 0.0);
                for lp in std::iter::once(&face.outer).chain(&face.holes) {
                    let pts = self.loop_points(lp);
                    for i in 0..pts.len() {
                        sum = sum + pts[i].cross(pts[(i + 1) % pts.len()]);
                    }
                }
                let Surface::Plane { normal, .. } = face.surface;
                0.5 * sum.dot(normal).abs()
            })
            .sum()
    }

    /// ASCII STL export of the tessellation.
    pub fn to_stl(&self, name: &str) -> String {
        let mut s = format!("solid {name}\n");
        for [a, b, c] in self.tessellate() {
            let n = (b - a).cross(c - a).normalize();
            let _ = writeln!(s, "  facet normal {} {} {}\n    outer loop", n.x, n.y, n.z);
            for p in [a, b, c] {
                let _ = writeln!(s, "      vertex {} {} {}", p.x, p.y, p.z);
            }
            s.push_str("    endloop\n  endfacet\n");
        }
        let _ = writeln!(s, "endsolid {name}");
        s
    }
}

// ───────────────────────── Triangulation ─────────────────────────

type P2 = [f64; 2];

/// (a − o) × (b − o): positive when o → a → b turns counter-clockwise.
fn cross2(o: P2, a: P2, b: P2) -> f64 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// Is `p` inside or on the triangle abc (either winding)?
fn in_triangle(p: P2, a: P2, b: P2, c: P2) -> bool {
    let d = [cross2(a, b, p), cross2(b, c, p), cross2(c, a, p)];
    !(d.iter().any(|&x| x < 0.0) && d.iter().any(|&x| x > 0.0))
}

/// Triangulates a planar polygon with holes by ear clipping. `rings[0]` is
/// the outer loop (counter-clockwise around `normal`), the rest are holes
/// (clockwise). Each hole is first joined to the boundary by a two-way
/// "bridge" edge, which turns the face into one simple polygon (D. Eberly,
/// "Triangulation by Ear Clipping").
fn triangulate_planar(rings: &[Vec<Vec3>], normal: Vec3) -> Vec<[Vec3; 3]> {
    // (u, v, normal) is right-handed, so counter-clockwise around the normal
    // stays counter-clockwise in 2D.
    let seed = if normal.x.abs() < 0.9 { Vec3::new(1.0, 0.0, 0.0) } else { Vec3::new(0.0, 1.0, 0.0) };
    let u = (seed - normal * seed.dot(normal)).normalize();
    let v = normal.cross(u);
    let pts3: Vec<Vec3> = rings.concat();
    let pts: Vec<P2> = pts3.iter().map(|p| [p.dot(u), p.dot(v)]).collect();

    let mut ranges = Vec::new();
    let mut start = 0;
    for r in rings {
        ranges.push(start..start + r.len());
        start += r.len();
    }
    let mut poly: Vec<usize> = ranges[0].clone().collect();
    // Right-most holes first, so a bridge never crosses a hole not yet merged.
    let mut holes: Vec<Vec<usize>> = ranges[1..].iter().map(|r| r.clone().collect()).collect();
    let max_x = |h: &Vec<usize>| h.iter().map(|&i| pts[i][0]).fold(f64::NEG_INFINITY, f64::max);
    holes.sort_by(|a, b| max_x(b).total_cmp(&max_x(a)));
    for hole in holes {
        let m = (0..hole.len()).max_by(|&a, &b| pts[hole[a]][0].total_cmp(&pts[hole[b]][0])).unwrap();
        let Some(k) = bridge_target(&pts, &poly, pts[hole[m]]) else { continue };
        // … P, M, hole …, M, P, … : out along the bridge, round the hole, back.
        let mut merged = Vec::with_capacity(poly.len() + hole.len() + 2);
        merged.extend_from_slice(&poly[..=k]);
        merged.extend((0..=hole.len()).map(|i| hole[(m + i) % hole.len()]));
        merged.extend_from_slice(&poly[k..]);
        poly = merged;
    }
    ear_clip(&pts, poly).into_iter().map(|[a, b, c]| [pts3[a], pts3[b], pts3[c]]).collect()
}

/// Position in `poly` of a vertex that `m`, the right-most point of a hole,
/// can see: cast a ray towards +x, take the nearest edge it hits, and use
/// that edge's right end unless a reflex vertex blocks the view.
fn bridge_target(pts: &[P2], poly: &[usize], m: P2) -> Option<usize> {
    let n = poly.len();
    let mut best: Option<(f64, usize)> = None;
    for i in 0..n {
        let (a, b) = (pts[poly[i]], pts[poly[(i + 1) % n]]);
        // The material is on the left of every edge, so seen from inside
        // the ray meets edges that run upwards.
        if !(a[1] <= m[1] && m[1] <= b[1] && a[1] < b[1]) {
            continue;
        }
        let x = a[0] + (m[1] - a[1]) * (b[0] - a[0]) / (b[1] - a[1]);
        if x >= m[0] && best.is_none_or(|(bx, _)| x < bx) {
            best = Some((x, if a[0] >= b[0] { i } else { (i + 1) % n }));
        }
    }
    let (hit_x, k) = best?;
    let (hit, p) = ([hit_x, m[1]], pts[poly[k]]);
    if p == hit {
        return Some(k);
    }
    // Reflex vertices inside triangle (m, hit, p) would hide p. Pick the one
    // closest in angle to the ray, then the nearest.
    let mut choice = (k, (f64::INFINITY, f64::INFINITY));
    for j in 0..n {
        let r = pts[poly[j]];
        let (prev, next) = (pts[poly[(j + n - 1) % n]], pts[poly[(j + 1) % n]]);
        if r == p || cross2(prev, r, next) >= 0.0 || !in_triangle(r, m, hit, p) {
            continue;
        }
        let (dx, dy) = (r[0] - m[0], r[1] - m[1]);
        let key = (dy.abs().atan2(dx), dx.hypot(dy));
        if key < choice.1 {
            choice = (j, key);
        }
    }
    Some(choice.0)
}

/// Repeatedly cuts off an "ear": a convex corner whose triangle holds no
/// other vertex. `poly` must be counter-clockwise.
fn ear_clip(pts: &[P2], mut poly: Vec<usize>) -> Vec<[usize; 3]> {
    let mut tris = Vec::with_capacity(poly.len().saturating_sub(2));
    let (mut i, mut misses) = (0, 0);
    while poly.len() > 3 {
        let n = poly.len();
        let (ia, ib, ic) = ((i + n - 1) % n, i % n, (i + 1) % n);
        let (a, b, c) = (pts[poly[ia]], pts[poly[ib]], pts[poly[ic]]);
        let is_ear = cross2(a, b, c) > 0.0
            && poly.iter().all(|&q| {
                let p = pts[q];
                p == a || p == b || p == c || !in_triangle(p, a, b, c)
            });
        // A degenerate polygon may have no strict ear; clip anyway so this ends.
        if is_ear || misses > n {
            tris.push([poly[ia], poly[ib], poly[ic]]);
            poly.remove(ib);
            i = (ib + poly.len() - 1) % poly.len();
            misses = 0;
        } else {
            i = (i + 1) % n;
            misses += 1;
        }
    }
    if poly.len() == 3 {
        tris.push([poly[0], poly[1], poly[2]]);
    }
    tris
}

// ───────────────────────────── Tests ────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn box_topology_counts() {
        let b = Brep::make_box(Vec3::new(0.0, 0.0, 0.0), 2.0, 3.0, 4.0);
        assert_eq!(b.counts(), (8, 12, 6));
        assert_eq!(b.validate(), Ok(()));
    }

    #[test]
    fn box_mass_properties() {
        let b = Brep::make_box(Vec3::new(1.0, -2.0, 5.0), 2.0, 3.0, 4.0);
        assert!(approx(b.volume(), 24.0));
        assert!(approx(b.surface_area(), 2.0 * (6.0 + 8.0 + 12.0)));
    }

    #[test]
    fn faces_point_outward() {
        let b = Brep::make_box(Vec3::new(0.0, 0.0, 0.0), 1.0, 1.0, 1.0);
        let centre = Vec3::new(0.5, 0.5, 0.5);
        for face in &b.faces {
            let Surface::Plane { origin, normal } = face.surface;
            let face_centre = b.loop_points(&face.outer).iter().fold(origin * 0.0, |a, &p| a + p) * 0.25;
            assert!((face_centre - centre).dot(normal) > 0.0);
        }
    }

    #[test]
    fn concave_l_prism() {
        let p = |x, y| Vec3::new(x, y, 0.0);
        let profile = [p(0.0, 0.0), p(3.0, 0.0), p(3.0, 1.0), p(1.0, 1.0), p(1.0, 3.0), p(0.0, 3.0)];
        let b = Brep::extrude_polygon(&profile, 2.0);
        assert_eq!(b.counts(), (12, 18, 8));
        assert_eq!(b.validate(), Ok(()));
        assert!(approx(b.volume(), 5.0 * 2.0)); // L area = 5
    }

    #[test]
    fn detects_open_shell() {
        let mut b = Brep::make_box(Vec3::new(0.0, 0.0, 0.0), 1.0, 1.0, 1.0);
        b.faces.pop(); // remove a side face → hole in the skin
        let errs = b.validate().unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, BrepError::EdgeUseCount { .. })));
        assert_eq!(b.genus(), None, "5 faces left: V − E + F = 8 − 12 + 5 = 1");
    }

    #[test]
    fn detects_flipped_face() {
        let mut b = Brep::make_box(Vec3::new(0.0, 0.0, 0.0), 1.0, 1.0, 1.0);
        // Reverse the top face's loop without fixing anything else.
        let lp = &mut b.faces[1].outer.coedges;
        lp.reverse();
        for ce in lp.iter_mut() {
            ce.forward = !ce.forward;
        }
        let errs = b.validate().unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, BrepError::EdgeSameDirection { .. })));
    }

    /// Counter-clockwise square centred at (cx, cy) with half-size `h`.
    fn square(cx: f64, cy: f64, h: f64) -> Vec<Vec3> {
        [(-h, -h), (h, -h), (h, h), (-h, h)].iter().map(|&(x, y)| Vec3::new(cx + x, cy + y, 0.0)).collect()
    }

    /// Area and enclosed volume measured on the tessellation.
    fn mesh_area_volume(b: &Brep) -> (f64, f64) {
        b.tessellate().iter().fold((0.0, 0.0), |(area, vol), [p, q, r]| {
            (area + 0.5 * (*q - *p).cross(*r - *p).length(), vol + p.dot(q.cross(*r)) / 6.0)
        })
    }

    #[test]
    fn through_hole_has_genus_one() {
        // 4 x 4 block with a 2 x 2 hole, 1 high. The hole is given
        // counter-clockwise on purpose: the builder reverses it.
        let b = Brep::extrude_profile(&square(0.0, 0.0, 2.0), &[&square(0.0, 0.0, 1.0)], 1.0);
        assert_eq!(b.counts(), (16, 24, 10)); // 16 − 24 + 10 − 2 holes = 0 = 2 − 2·1
        assert_eq!(b.validate(), Ok(()));
        assert_eq!(b.genus(), Some(1));
        assert!(approx(b.volume(), (16.0 - 4.0) * 1.0));
        assert!(approx(b.surface_area(), 2.0 * 12.0 + 16.0 + 8.0)); // caps, outer walls, hole walls
    }

    #[test]
    fn two_holes_give_genus_two() {
        let outer = [Vec3::new(0.0, 0.0, 0.0), Vec3::new(10.0, 0.0, 0.0), Vec3::new(10.0, 4.0, 0.0), Vec3::new(0.0, 4.0, 0.0)];
        let b = Brep::extrude_profile(&outer, &[&square(2.5, 2.0, 1.0), &square(7.5, 2.0, 1.0)], 3.0);
        assert_eq!(b.validate(), Ok(()));
        assert_eq!(b.genus(), Some(2));
        assert!(approx(b.volume(), (40.0 - 8.0) * 3.0));
    }

    #[test]
    fn solid_without_holes_has_genus_zero() {
        assert_eq!(Brep::make_box(Vec3::new(0.0, 0.0, 0.0), 1.0, 1.0, 1.0).genus(), Some(0));
    }

    #[test]
    fn clockwise_profile_is_reoriented() {
        let mut cw = square(0.0, 0.0, 1.0);
        cw.reverse();
        let b = Brep::extrude_polygon(&cw, 2.0);
        assert_eq!(b.validate(), Ok(()));
        assert!(approx(b.volume(), 8.0));
    }

    #[test]
    fn tessellation_matches_exact_area_and_volume() {
        let p = |x, y| Vec3::new(x, y, 0.0);
        let l_shape = [p(0.0, 0.0), p(3.0, 0.0), p(3.0, 1.0), p(1.0, 1.0), p(1.0, 3.0), p(0.0, 3.0)];
        // A concave (L-shaped) hole, off centre, in a concave outer profile.
        let u_shape = [p(0.0, 0.0), p(9.0, 0.0), p(9.0, 9.0), p(6.0, 9.0), p(6.0, 4.0), p(3.0, 4.0), p(3.0, 9.0), p(0.0, 9.0)];
        let l_hole: Vec<Vec3> = l_shape.iter().map(|&q| q * 0.5 + p(0.5, 0.5)).collect();
        for b in [
            Brep::make_box(Vec3::new(1.0, 2.0, 3.0), 2.0, 3.0, 4.0),
            Brep::extrude_polygon(&l_shape, 2.0),
            Brep::extrude_profile(&square(0.0, 0.0, 2.0), &[&square(0.0, 0.0, 1.0)], 1.0),
            Brep::extrude_profile(&u_shape, &[&l_hole, &square(7.5, 2.0, 0.5)], 1.5),
        ] {
            assert_eq!(b.validate(), Ok(()));
            let (area, vol) = mesh_area_volume(&b);
            assert!(approx(area, b.surface_area()), "area {area} vs {}", b.surface_area());
            assert!(approx(vol, b.volume()), "volume {vol} vs {}", b.volume());
        }
    }

    /// Unit cube at x0, built face by face into an existing Brep.
    fn add_cube(b: &mut Brep, x0: f64) -> Vec<FaceId> {
        let corners = [(0., 0., 0.), (1., 0., 0.), (1., 1., 0.), (0., 1., 0.), (0., 0., 1.), (1., 0., 1.), (1., 1., 1.), (0., 1., 1.)];
        let v: Vec<VertexId> = corners.iter().map(|&(x, y, z)| b.add_vertex(Vec3::new(x0 + x, y, z))).collect();
        [[0, 3, 2, 1], [4, 5, 6, 7], [0, 1, 5, 4], [1, 2, 6, 5], [2, 3, 7, 6], [3, 0, 4, 7]]
            .iter()
            .map(|q| b.add_planar_face(&q.map(|i| v[i]), &[]))
            .collect()
    }

    #[test]
    fn euler_rejects_two_solids_in_one_shell() {
        let mut one = Brep::new();
        let mut faces = add_cube(&mut one, 0.0);
        faces.extend(add_cube(&mut one, 5.0));
        one.add_shell(faces); // V − E + F − H = 4: no single closed surface
        let errs = one.validate().unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, BrepError::EulerMismatch { .. })));
        assert_eq!(one.genus(), None);

        let mut two = Brep::new();
        let (a, b) = (add_cube(&mut two, 0.0), add_cube(&mut two, 5.0));
        two.add_shell(a);
        two.add_shell(b);
        assert_eq!(two.validate(), Ok(()));
        assert_eq!(two.genus(), Some(0));
    }
}
