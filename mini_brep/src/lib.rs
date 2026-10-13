//! mini_brep — a small, dependency-free Boundary Representation (B-rep) in Rust.
//!
//! Structure:
//!   Geometry : Point (Vec3), Curve (Line, Circle), Surface (Plane, Cylinder)
//!   Topology : Solid → Shell → Face → Loop → CoEdge → Edge → Vertex
//!   Faces may have holes; solids may have through-holes (genus > 0).
//!
//! Curves and surfaces use the parameterisations of NVIDIA's OpenUSD
//! BrepArray schema, so exporting needs no conversion. Closed surfaces follow
//! its conventions too: a full circle is one edge whose start and end vertex
//! are the same, and a cylinder face is cut open along a "seam" edge that
//! the face uses twice, once in each direction.
//!
//! Topology lives in flat arenas (Vec) addressed by typed ids, which avoids
//! Rc<RefCell<..>> graphs and keeps the structure easy to serialize and copy.

use std::collections::HashMap;
use std::f64::consts::TAU;
use std::fmt::Write as _;
use std::ops::{Add, Mul, Neg, Sub};

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
impl Neg for Vec3 {
    type Output = Self;
    fn neg(self) -> Self { Self::new(-self.x, -self.y, -self.z) }
}

/// Global modeling tolerance (model units).
pub const TOLERANCE: f64 = 1e-9;

/// How far a vertex or edge may sit from its curve or surface.
pub const GEOMETRY_TOLERANCE: f64 = 1e-6;

/// Segments per full turn when a curved edge or face is tessellated.
pub const SEGMENTS_PER_TURN: usize = 64;

// ─────────────────────────── Geometry ───────────────────────────

/// Exact curve an edge lies on.
#[derive(Debug, Clone)]
pub enum Curve {
    /// C(t) = origin + t · dir, `dir` a unit vector.
    Line { origin: Vec3, dir: Vec3 },
    /// C(t) = center + radius · (cos t · ref_dir + sin t · (axis × ref_dir)),
    /// t in radians; `axis` and `ref_dir` are perpendicular unit vectors.
    Circle { center: Vec3, axis: Vec3, ref_dir: Vec3, radius: f64 },
}

impl Curve {
    pub fn point_at(&self, t: f64) -> Vec3 {
        match *self {
            Curve::Line { origin, dir } => origin + dir * t,
            Curve::Circle { center, axis, ref_dir, radius } => {
                center + (ref_dir * t.cos() + axis.cross(ref_dir) * t.sin()) * radius
            }
        }
    }

    /// Parameter of the curve point nearest to `p` (a circle's is in (−π, π]).
    pub fn param_of(&self, p: Vec3) -> f64 {
        match *self {
            Curve::Line { origin, dir } => (p - origin).dot(dir),
            Curve::Circle { center, axis, ref_dir, .. } => {
                let d = p - center;
                d.dot(axis.cross(ref_dir)).atan2(d.dot(ref_dir))
            }
        }
    }

    pub fn distance(&self, p: Vec3) -> f64 {
        (self.point_at(self.param_of(p)) - p).length()
    }
}

/// Exact surface a face lies on. Its own normal may point into or out of
/// the solid; `Face::sense` says which.
#[derive(Debug, Clone)]
pub enum Surface {
    /// S(u, v) = origin + u · x + v · (normal × x), with x = `plane_x(normal)`.
    Plane { origin: Vec3, normal: Vec3 },
    /// S(u, v) = origin + radius · (cos u · ref_dir + sin u · (axis × ref_dir)) + v · axis.
    /// The normal points away from the axis.
    Cylinder { origin: Vec3, axis: Vec3, ref_dir: Vec3, radius: f64 },
}

/// A plane's u direction: horizontal where possible, so the faces of an
/// axis-aligned box get axis-aligned (u, v) frames.
pub fn plane_x(normal: Vec3) -> Vec3 {
    let h = Vec3::new(0.0, 0.0, 1.0).cross(normal);
    if h.length() > 1e-6 { h.normalize() } else { (Vec3::new(1.0, 0.0, 0.0) - normal * normal.x).normalize() }
}

impl Surface {
    pub fn point_at(&self, u: f64, v: f64) -> Vec3 {
        match *self {
            Surface::Plane { origin, normal } => {
                let x = plane_x(normal);
                origin + x * u + normal.cross(x) * v
            }
            Surface::Cylinder { origin, axis, ref_dir, radius } => {
                origin + (ref_dir * u.cos() + axis.cross(ref_dir) * u.sin()) * radius + axis * v
            }
        }
    }

    /// (u, v) of the surface point nearest to `p`; a cylinder's u is in (−π, π].
    pub fn uv_of(&self, p: Vec3) -> [f64; 2] {
        match *self {
            Surface::Plane { origin, normal } => {
                let (d, x) = (p - origin, plane_x(normal));
                [d.dot(x), d.dot(normal.cross(x))]
            }
            Surface::Cylinder { origin, axis, ref_dir, .. } => {
                let d = p - origin;
                [d.dot(axis.cross(ref_dir)).atan2(d.dot(ref_dir)), d.dot(axis)]
            }
        }
    }

    pub fn distance(&self, p: Vec3) -> f64 {
        match *self {
            Surface::Plane { origin, normal } => (p - origin).dot(normal).abs(),
            Surface::Cylinder { origin, axis, radius, .. } => {
                let d = p - origin;
                ((d - axis * d.dot(axis)).length() - radius).abs()
            }
        }
    }
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
/// It is shared by the two faces on either side of it. A closed edge (a full
/// circle) starts and ends at the same vertex.
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

/// A closed ring of coedges. Seen from outside the solid, outer loops run
/// counter-clockwise and holes clockwise.
#[derive(Debug, Clone, Default)]
pub struct Loop {
    pub coedges: Vec<CoEdge>,
}

#[derive(Debug, Clone)]
pub struct Face {
    pub surface: Surface,
    /// True when the surface's normal points out of the solid. A round hole's
    /// wall is a cylinder whose normal points into the material: false.
    pub sense: bool,
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

    /// Parameter interval (t0, t1), t0 < t1, of an edge on its curve. A
    /// closed edge (start == end) is one full turn of its circle.
    pub fn edge_range(&self, e: EdgeId) -> (f64, f64) {
        let edge = &self.edges[e.0];
        let t0 = edge.curve.param_of(self.point(edge.start));
        let t1 = edge.curve.param_of(self.point(edge.end));
        match edge.curve {
            Curve::Line { .. } => (t0, t1),
            Curve::Circle { .. } if edge.start == edge.end => (t0, t0 + TAU),
            // An arc runs forward from t0, so t1 must come after it.
            Curve::Circle { .. } => (t0, t0 + (t1 - t0).rem_euclid(TAU)),
        }
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

    /// Adds an edge on `curve` from `start` to `end`. For a closed edge (a
    /// full circle) pass the same vertex twice.
    pub fn add_edge(&mut self, curve: Curve, start: VertexId, end: VertexId) -> EdgeId {
        self.edges.push(Edge { curve, start, end });
        EdgeId(self.edges.len() - 1)
    }

    /// Adds a face from ready-made loops, which run counter-clockwise seen
    /// from outside (holes clockwise). `sense`: does the surface normal point
    /// out of the solid?
    pub fn add_face(&mut self, surface: Surface, sense: bool, outer: Loop, holes: Vec<Loop>) -> FaceId {
        self.faces.push(Face { surface, sense, outer, holes });
        FaceId(self.faces.len() - 1)
    }

    /// Adds a planar face with straight edges. `outer` must be
    /// counter-clockwise seen from outside; each hole clockwise. The plane
    /// normal is computed from the outer loop.
    pub fn add_planar_face(&mut self, outer: &[VertexId], holes: &[&[VertexId]]) -> FaceId {
        assert!(outer.len() >= 3, "a face needs at least 3 vertices");
        let pts: Vec<Vec3> = outer.iter().map(|&v| self.point(v)).collect();
        let normal = newell_normal(&pts);
        let outer_loop = self.make_loop(outer);
        let hole_loops = holes.iter().map(|h| self.make_loop(h)).collect();
        self.add_face(Surface::Plane { origin: pts[0], normal }, true, outer_loop, hole_loops)
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

    /// `extrude` for a polygon with polygonal holes.
    pub fn extrude_profile(outer: &[Vec3], holes: &[&[Vec3]], height: f64) -> Self {
        let holes: Vec<Ring> = holes.iter().map(|h| Ring::Polygon(h.to_vec())).collect();
        Self::extrude(&Ring::Polygon(outer.to_vec()), &holes, height)
    }

    /// Extrudes a profile lying in a plane z = const straight up by `height`.
    /// The profile is an outer ring plus holes, each a polygon or a circle.
    /// Every hole becomes a through-hole (genus + 1). A circle becomes a
    /// cylinder face with one seam edge. Polygons may run either way.
    pub fn extrude(outer: &Ring, holes: &[Ring], height: f64) -> Self {
        let ok = |r: &Ring| match r {
            Ring::Polygon(p) => p.len() >= 3,
            Ring::Circle { radius, .. } => *radius > TOLERANCE,
        };
        assert!(ok(outer) && holes.iter().all(ok) && height > TOLERANCE);
        let mut b = Brep::new();
        let (x, z) = (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
        let up = z * height;
        // Each ring with its direction seen from +Z: outer counter-clockwise.
        let rings: Vec<(Ring, bool)> = std::iter::once((outer.oriented(true), true))
            .chain(holes.iter().map(|h| (h.oriented(false), false)))
            .collect();

        // Corners: a polygon's, or one point on a circle, at the bottom and top.
        let corners: Vec<(Vec<VertexId>, Vec<VertexId>)> = rings
            .iter()
            .map(|(ring, _)| {
                let pts = match ring {
                    Ring::Polygon(p) => p.clone(),
                    Ring::Circle { center, radius } => vec![*center + x * *radius],
                };
                let bot = pts.iter().map(|&p| b.add_vertex(p)).collect();
                let top = pts.iter().map(|&p| b.add_vertex(p + up)).collect();
                (bot, top)
            })
            .collect();

        // Cap loops. The bottom cap faces down (-Z), so its loops run the other way.
        let (mut bottom_loops, mut top_loops, mut circles) = (Vec::new(), Vec::new(), Vec::new());
        for ((ring, ccw), (bot, top)) in rings.iter().zip(&corners) {
            match ring {
                Ring::Polygon(_) => {
                    let rev: Vec<VertexId> = bot.iter().rev().copied().collect();
                    bottom_loops.push(b.make_loop(&rev));
                    top_loops.push(b.make_loop(top));
                    circles.push(None);
                }
                Ring::Circle { center, radius } => {
                    // Circles turn counter-clockwise seen from +Z.
                    let circle = |c: Vec3| Curve::Circle { center: c, axis: z, ref_dir: x, radius: *radius };
                    let cb = b.add_edge(circle(*center), bot[0], bot[0]);
                    let ct = b.add_edge(circle(*center + up), top[0], top[0]);
                    bottom_loops.push(Loop { coedges: vec![CoEdge { edge: cb, forward: !ccw }] });
                    top_loops.push(Loop { coedges: vec![CoEdge { edge: ct, forward: *ccw }] });
                    circles.push(Some((cb, ct)));
                }
            }
        }
        let base = b.point(corners[0].0[0]);
        let mut caps = Vec::new();
        for (loops, origin, normal) in [(bottom_loops, base, -z), (top_loops, base + up, z)] {
            let mut loops = loops.into_iter();
            let outer = loops.next().unwrap();
            caps.push(b.add_face(Surface::Plane { origin, normal }, true, outer, loops.collect()));
        }

        // Walls. Around a hole they face into the hole, away from the material.
        let mut faces = caps;
        for (((ring, ccw), (bot, top)), circle) in rings.iter().zip(&corners).zip(&circles) {
            match (ring, circle) {
                (Ring::Circle { center, radius }, Some((cb, ct))) => {
                    let seam = b.add_edge(Curve::Line { origin: b.point(bot[0]), dir: z }, bot[0], top[0]);
                    let ce = |edge: EdgeId, forward| CoEdge { edge, forward };
                    // Outer wall: along the bottom, up the seam, back along the
                    // top, down the seam. A hole's wall runs the other way.
                    let coedges = if *ccw {
                        vec![ce(*cb, true), ce(seam, true), ce(*ct, false), ce(seam, false)]
                    } else {
                        vec![ce(seam, true), ce(*ct, true), ce(seam, false), ce(*cb, false)]
                    };
                    let surface = Surface::Cylinder { origin: *center, axis: z, ref_dir: x, radius: *radius };
                    faces.push(b.add_face(surface, *ccw, Loop { coedges }, Vec::new()));
                }
                _ => {
                    let n = bot.len();
                    for i in 0..n {
                        let j = (i + 1) % n;
                        faces.push(b.add_planar_face(&[bot[i], bot[j], top[j], top[i]], &[]));
                    }
                }
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

    /// Upright cylinder standing on the circle (`base`, `radius`).
    pub fn make_cylinder(base: Vec3, radius: f64, height: f64) -> Self {
        Self::extrude(&Ring::Circle { center: base, radius }, &[], height)
    }
}

/// One closed boundary of a profile lying in a plane z = const.
#[derive(Debug, Clone, PartialEq)]
pub enum Ring {
    Polygon(Vec<Vec3>),
    Circle { center: Vec3, radius: f64 },
}

impl Ring {
    /// The same ring running counter-clockwise (or clockwise) seen from +Z.
    /// A circle has no stored direction.
    fn oriented(&self, ccw: bool) -> Ring {
        match self {
            Ring::Polygon(p) if (signed_area_xy(p) > 0.0) != ccw => Ring::Polygon(p.iter().rev().copied().collect()),
            r => r.clone(),
        }
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

// ──────────────────────── Parameter space ───────────────────────

/// A trimming curve in a face's (u, v) parameter space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UvCurve {
    Line { a: [f64; 2], b: [f64; 2] },
    /// center + radius · (cos t · x + sin t · y) as t goes from t0 to t1
    /// (backwards when t1 < t0). Only planes have arcs in (u, v).
    Arc { center: [f64; 2], radius: f64, x: [f64; 2], y: [f64; 2], t0: f64, t1: f64 },
}

fn cross_2d(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}

impl UvCurve {
    pub fn point_at(&self, t: f64) -> [f64; 2] {
        match *self {
            UvCurve::Line { a, b } => [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t],
            UvCurve::Arc { center: c, radius: r, x, y, .. } => {
                let (cos, sin) = (t.cos(), t.sin());
                [c[0] + r * (cos * x[0] + sin * y[0]), c[1] + r * (cos * x[1] + sin * y[1])]
            }
        }
    }
    pub fn start(&self) -> [f64; 2] {
        match *self {
            UvCurve::Line { a, .. } => a,
            UvCurve::Arc { t0, .. } => self.point_at(t0),
        }
    }
    pub fn end(&self) -> [f64; 2] {
        match *self {
            UvCurve::Line { b, .. } => b,
            UvCurve::Arc { t1, .. } => self.point_at(t1),
        }
    }

    /// The same curve traversed the other way.
    pub fn reversed(&self) -> UvCurve {
        match *self {
            UvCurve::Line { a, b } => UvCurve::Line { a: b, b: a },
            UvCurve::Arc { center, radius, x, y, t0, t1 } => UvCurve::Arc { center, radius, x, y, t0: t1, t1: t0 },
        }
    }

    /// ½ ∫ (u dv − v du) along the curve. Summed round a loop it is the
    /// enclosed area (Green's theorem), positive counter-clockwise.
    fn area_term(&self) -> f64 {
        match *self {
            UvCurve::Line { a, b } => 0.5 * cross_2d(a, b),
            UvCurve::Arc { center, radius, x, y, t0, t1 } => {
                let (s, e) = (self.point_at(t0), self.point_at(t1));
                0.5 * (cross_2d(center, [e[0] - s[0], e[1] - s[1]]) + radius * radius * (t1 - t0) * cross_2d(x, y))
            }
        }
    }

    /// Points along the curve for tessellation, end point excluded.
    fn samples(&self) -> Vec<[f64; 2]> {
        match *self {
            UvCurve::Line { a, .. } => vec![a],
            UvCurve::Arc { t0, t1, .. } => {
                let n = segments_for(t1 - t0);
                (0..n).map(|i| self.point_at(t0 + (t1 - t0) * i as f64 / n as f64)).collect()
            }
        }
    }
}

fn segments_for(angle: f64) -> usize {
    ((angle.abs() / TAU) * SEGMENTS_PER_TURN as f64).ceil().max(1.0) as usize
}

impl Brep {
    /// A face's loops in its own (u, v) parameter space, outer loop first,
    /// each curve in its coedge's direction. Across a cylinder's seam u jumps
    /// by a full turn, so each loop closes in (u, v) too.
    pub fn face_uv_loops(&self, face: &Face) -> Vec<Vec<UvCurve>> {
        std::iter::once(&face.outer)
            .chain(&face.holes)
            .map(|lp| {
                let mut curves: Vec<UvCurve> = lp.coedges.iter().map(|&ce| self.coedge_uv(&face.surface, ce)).collect();
                if let Surface::Cylinder { .. } = face.surface {
                    // u is an angle: shift each curve by whole turns so it
                    // starts where the previous one ended.
                    for k in 1..curves.len() {
                        if let (UvCurve::Line { a, b }, prev) = (curves[k], curves[k - 1].end()) {
                            let shift = ((prev[0] - a[0]) / TAU).round() * TAU;
                            curves[k] = UvCurve::Line { a: [a[0] + shift, a[1]], b: [b[0] + shift, b[1]] };
                        }
                    }
                }
                curves
            })
            .collect()
    }

    fn coedge_uv(&self, surface: &Surface, ce: CoEdge) -> UvCurve {
        let curve = &self.edges[ce.edge.0].curve;
        let (t0, t1) = self.edge_range(ce.edge);
        let (ts, te) = if ce.forward { (t0, t1) } else { (t1, t0) };
        match (surface, curve) {
            (Surface::Plane { .. }, Curve::Circle { center, axis, ref_dir, radius }) => {
                let c = surface.uv_of(*center);
                let dir = |d: Vec3| {
                    let q = surface.uv_of(*center + d);
                    [q[0] - c[0], q[1] - c[1]]
                };
                UvCurve::Arc { center: c, radius: *radius, x: dir(*ref_dir), y: dir(axis.cross(*ref_dir)), t0: ts, t1: te }
            }
            (Surface::Cylinder { axis: cyl_axis, .. }, Curve::Circle { axis, .. }) => {
                // A circle round the axis is a line of constant v; u follows t
                // when the circle turns the same way as the cylinder.
                let a = surface.uv_of(curve.point_at(ts));
                UvCurve::Line { a, b: [a[0] + (te - ts) * axis.dot(*cyl_axis).signum(), a[1]] }
            }
            _ => UvCurve::Line { a: surface.uv_of(curve.point_at(ts)), b: surface.uv_of(curve.point_at(te)) },
        }
    }
}

// ────────────────────────── Validation ──────────────────────────

#[derive(Debug, PartialEq)]
pub enum BrepError {
    OpenLoop { face: FaceId },
    EdgeUseCount { edge: EdgeId, uses: usize },
    EdgeSameDirection { edge: EdgeId },
    /// An edge of the face leaves the face's surface.
    OffSurface { face: FaceId, deviation: f64 },
    /// An edge's end vertices are not on its curve.
    OffCurve { edge: EdgeId, deviation: f64 },
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
                // 2. Edges must lie on the face's surface: check their ends and
                //    three points between.
                let deviation = lp
                    .coedges
                    .iter()
                    .flat_map(|ce| {
                        let (t0, t1) = self.edge_range(ce.edge);
                        let curve = &self.edges[ce.edge.0].curve;
                        (0..=4).map(move |k| curve.point_at(t0 + (t1 - t0) * k as f64 / 4.0))
                    })
                    .map(|p| face.surface.distance(p))
                    .fold(0.0, f64::max);
                if deviation > GEOMETRY_TOLERANCE {
                    errors.push(BrepError::OffSurface { face: fid, deviation });
                }
            }
        }

        // 3. Each edge's vertices lie on its curve. Manifold & consistently
        //    oriented: each edge used once each way (a seam is used twice by
        //    the same face, once each way).
        for ei in 0..self.edges.len() {
            let id = EdgeId(ei);
            let edge = &self.edges[ei];
            let deviation = edge.curve.distance(self.point(edge.start)).max(edge.curve.distance(self.point(edge.end)));
            if deviation > GEOMETRY_TOLERANCE {
                errors.push(BrepError::OffCurve { edge: id, deviation });
            }
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
    /// wind counter-clockwise seen from outside, so they face out of the
    /// solid. Curved edges and faces are approximated with
    /// `SEGMENTS_PER_TURN` segments per full turn.
    pub fn tessellate(&self) -> Vec<[Vec3; 3]> {
        let mut tris = Vec::new();
        for face in &self.faces {
            let loops = self.face_uv_loops(face);
            match face.surface {
                Surface::Plane { normal, .. } => {
                    let rings: Vec<Vec<Vec3>> = loops
                        .iter()
                        .map(|lp| lp.iter().flat_map(UvCurve::samples).map(|[u, v]| face.surface.point_at(u, v)).collect())
                        .collect();
                    tris.extend(triangulate_planar(&rings, if face.sense { normal } else { -normal }));
                }
                Surface::Cylinder { .. } => {
                    // Builders make cylinder faces whose (u, v) domain is a
                    // rectangle, so a grid of strips covers it.
                    let corners = loops.iter().flatten().flat_map(|c| [c.start(), c.end()]);
                    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
                    for p in corners {
                        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
                    }
                    let n = segments_for(hi[0] - lo[0]);
                    let u = |i: usize| lo[0] + (hi[0] - lo[0]) * i as f64 / n as f64;
                    for i in 0..n {
                        let s = |u, v| face.surface.point_at(u, v);
                        let q = [s(u(i), lo[1]), s(u(i + 1), lo[1]), s(u(i + 1), hi[1]), s(u(i), hi[1])];
                        // Counter-clockwise in (u, v) faces along the surface normal.
                        if face.sense {
                            tris.extend([[q[0], q[1], q[2]], [q[0], q[2], q[3]]]);
                        } else {
                            tris.extend([[q[0], q[2], q[1]], [q[0], q[3], q[2]]]);
                        }
                    }
                }
            }
        }
        tris
    }

    /// (area, volume contribution) of one face, exact for its curves.
    ///
    /// Volume uses the divergence theorem, V = ⅓ ∮ p·n dA over the boundary.
    /// Both integrals over the face are turned into integrals round its
    /// (u, v) loops by Green's theorem, so a hole's loop, which runs the other
    /// way, subtracts itself.
    fn face_area_volume(&self, face: &Face) -> (f64, f64) {
        let loops = self.face_uv_loops(face);
        let curves = loops.iter().flatten();
        match face.surface {
            Surface::Plane { origin, normal } => {
                // p·n is the same everywhere on a plane: origin·n.
                let a: f64 = curves.map(UvCurve::area_term).sum();
                (a.abs(), origin.dot(normal) * a / 3.0)
            }
            Surface::Cylinder { origin: o, axis, ref_dir: x, radius: r } => {
                // dA = r du dv and p·n = o·(cos u x + sin u y) + r, so with
                // G(u) = ∫ p·n du, the face integral is ∮ G(u) dv; H = ∫ G du.
                let y = axis.cross(x);
                let g = |u: f64| o.dot(x * u.sin() - y * u.cos()) + r * u;
                let h = |u: f64| -o.dot(x * u.cos() + y * u.sin()) + r * u * u / 2.0;
                let (mut area, mut flux) = (0.0, 0.0);
                for c in curves {
                    // Cylinder curves are straight in (u, v).
                    let ([u0, v0], [u1, v1]) = (c.start(), c.end());
                    let dv = v1 - v0;
                    area += 0.5 * (u0 + u1) * dv;
                    flux += if (u1 - u0).abs() < TOLERANCE { g(u0) * dv } else { dv / (u1 - u0) * (h(u1) - h(u0)) };
                }
                (r * area.abs(), r * flux / 3.0)
            }
        }
    }

    /// Enclosed volume, exact for planes and cylinders.
    pub fn volume(&self) -> f64 {
        self.faces.iter().map(|f| self.face_area_volume(f).1).sum()
    }

    /// Total face area, exact for planes and cylinders.
    pub fn surface_area(&self) -> f64 {
        self.faces.iter().map(|f| self.face_area_volume(f).0).sum()
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
            let Surface::Plane { origin, normal } = face.surface else { panic!("a box has only planes") };
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

    fn approx_rel(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= rel * b.abs()
    }

    #[test]
    fn cylinder_is_valid_with_exact_volume_and_area() {
        // Off the origin on purpose: the volume formula has origin terms.
        let (r, h) = (2.0, 3.0);
        let b = Brep::make_cylinder(Vec3::new(1.0, -2.0, 0.5), r, h);
        // 2 vertices (one per circle), 3 edges (2 circles + seam), 3 faces:
        // V − E + F = 2 − 3 + 3 = 2, genus 0.
        assert_eq!(b.counts(), (2, 3, 3));
        assert_eq!(b.validate(), Ok(()));
        assert_eq!(b.genus(), Some(0));
        let pi = std::f64::consts::PI;
        assert!(approx(b.volume(), pi * r * r * h), "volume {}", b.volume());
        assert!(approx(b.surface_area(), 2.0 * pi * r * h + 2.0 * pi * r * r), "area {}", b.surface_area());
    }

    #[test]
    fn round_hole_through_a_block() {
        // 4 x 4 x 2 block, hole of radius 1 off centre at (0.5, -0.5).
        let pi = std::f64::consts::PI;
        let hole = Ring::Circle { center: Vec3::new(0.5, -0.5, 0.0), radius: 1.0 };
        let b = Brep::extrude(&Ring::Polygon(square(0.0, 0.0, 2.0)), &[hole], 2.0);
        // V 8 + 2, E 12 + 3, F 2 caps + 4 sides + 1 wall, H 2: 10 − 15 + 7 − 2 = 0.
        assert_eq!(b.counts(), (10, 15, 7));
        assert_eq!(b.validate(), Ok(()));
        assert_eq!(b.genus(), Some(1));
        assert!(!b.faces[6].sense, "the hole's wall faces its axis");
        assert!(approx(b.volume(), (16.0 - pi) * 2.0), "volume {}", b.volume());
        let area = 2.0 * (16.0 - pi) + 16.0 * 2.0 + 2.0 * pi * 2.0; // caps, outer walls, hole wall
        assert!(approx(b.surface_area(), area), "area {}", b.surface_area());
    }

    #[test]
    fn square_hole_through_a_cylinder() {
        let pi = std::f64::consts::PI;
        let outer = Ring::Circle { center: Vec3::new(0.0, 0.0, 0.0), radius: 3.0 };
        let b = Brep::extrude(&outer, &[Ring::Polygon(square(0.0, 0.0, 1.0))], 1.0);
        assert_eq!(b.validate(), Ok(()));
        assert_eq!(b.genus(), Some(1));
        assert!(approx(b.volume(), 9.0 * pi - 4.0));
    }

    #[test]
    fn uv_loops_close_across_the_seam() {
        let hole = Ring::Circle { center: Vec3::new(0.0, 0.0, 0.0), radius: 1.0 };
        let b = Brep::extrude(&Ring::Circle { center: Vec3::new(0.0, 0.0, 0.0), radius: 2.0 }, &[hole], 1.0);
        for face in &b.faces {
            for lp in b.face_uv_loops(face) {
                for k in 0..lp.len() {
                    let (end, next) = (lp[k].end(), lp[(k + 1) % lp.len()].start());
                    assert!(approx(end[0], next[0]) && approx(end[1], next[1]), "{:?}", lp);
                }
            }
        }
        // The outer wall spans one full turn of u.
        let wall = b.face_uv_loops(&b.faces[2]);
        let us: Vec<f64> = wall[0].iter().map(|c| c.start()[0]).collect();
        let span = us.iter().cloned().fold(f64::NEG_INFINITY, f64::max) - us.iter().cloned().fold(f64::INFINITY, f64::min);
        assert!(approx(span, TAU));
    }

    #[test]
    fn curved_tessellation_converges_to_exact_values() {
        let hole = Ring::Circle { center: Vec3::new(0.5, 0.0, 0.0), radius: 1.0 };
        for b in [
            Brep::make_cylinder(Vec3::new(1.0, 1.0, 1.0), 2.0, 3.0),
            Brep::extrude(&Ring::Polygon(square(0.0, 0.0, 2.0)), &[hole], 2.0),
        ] {
            let (area, vol) = mesh_area_volume(&b);
            // 64 segments per turn: a chord polygon is within 0.2% of its circle.
            assert!(approx_rel(area, b.surface_area(), 0.002), "area {area} vs {}", b.surface_area());
            assert!(approx_rel(vol, b.volume(), 0.002), "volume {vol} vs {}", b.volume());
        }
    }

    #[test]
    fn detects_geometry_off_its_surface_or_curve() {
        let mut b = Brep::make_cylinder(Vec3::new(0.0, 0.0, 0.0), 1.0, 1.0);
        // The cylinder face no longer passes through its circles.
        if let Surface::Cylinder { radius, .. } = &mut b.faces[2].surface {
            *radius = 1.5;
        }
        let errs = b.validate().unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, BrepError::OffSurface { .. })), "{errs:?}");

        let mut b = Brep::make_cylinder(Vec3::new(0.0, 0.0, 0.0), 1.0, 1.0);
        b.vertices[0].point = Vec3::new(1.2, 0.0, 0.0); // off its bottom circle
        let errs = b.validate().unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, BrepError::OffCurve { .. })), "{errs:?}");
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
