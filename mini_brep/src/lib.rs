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
use std::f64::consts::{FRAC_PI_2, TAU};
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
///
/// Everything but the plane is a surface of revolution: with y = axis × ref_dir,
/// S(u, v) = origin + ρ(v) · (cos u · ref_dir + sin u · y) + h(v) · axis,
/// u the angle round the axis. `profile` gives ρ and h. Its normal is
/// ∂S/∂u × ∂S/∂v, which points away from the axis (for a torus, away from the
/// tube's centre line), as the BrepArray schema specifies.
#[derive(Debug, Clone)]
pub enum Surface {
    /// S(u, v) = origin + u · x + v · (normal × x), with x = `plane_x(normal)`.
    Plane { origin: Vec3, normal: Vec3 },
    /// ρ = radius, h = v.
    Cylinder { origin: Vec3, axis: Vec3, ref_dir: Vec3, radius: f64 },
    /// ρ = radius + v · tan(semi_angle), h = v; `origin` is where ρ = radius.
    Cone { origin: Vec3, axis: Vec3, ref_dir: Vec3, radius: f64, semi_angle: f64 },
    /// ρ = radius · cos v, h = radius · sin v; v = ±π/2 at the poles.
    Sphere { center: Vec3, axis: Vec3, ref_dir: Vec3, radius: f64 },
    /// ρ = major + minor · cos v, h = minor · sin v.
    Torus { origin: Vec3, axis: Vec3, ref_dir: Vec3, major_radius: f64, minor_radius: f64 },
}

/// A plane's u direction: horizontal where possible, so the faces of an
/// axis-aligned box get axis-aligned (u, v) frames.
pub fn plane_x(normal: Vec3) -> Vec3 {
    let h = Vec3::new(0.0, 0.0, 1.0).cross(normal);
    if h.length() > 1e-6 { h.normalize() } else { (Vec3::new(1.0, 0.0, 0.0) - normal * normal.x).normalize() }
}

impl Surface {
    /// A surface of revolution's frame (origin, axis, ref_dir); None for a plane.
    pub fn revolution(&self) -> Option<(Vec3, Vec3, Vec3)> {
        match *self {
            Surface::Plane { .. } => None,
            Surface::Cylinder { origin, axis, ref_dir, .. }
            | Surface::Cone { origin, axis, ref_dir, .. }
            | Surface::Torus { origin, axis, ref_dir, .. } => Some((origin, axis, ref_dir)),
            Surface::Sphere { center, axis, ref_dir, .. } => Some((center, axis, ref_dir)),
        }
    }

    /// The profile of a surface of revolution at v: (ρ, dρ/dv, h, dh/dv).
    /// All zeros for a plane.
    pub fn profile(&self, v: f64) -> (f64, f64, f64, f64) {
        match *self {
            Surface::Plane { .. } => (0.0, 0.0, 0.0, 0.0),
            Surface::Cylinder { radius, .. } => (radius, 0.0, v, 1.0),
            Surface::Cone { radius, semi_angle, .. } => {
                let t = semi_angle.tan();
                (radius + v * t, t, v, 1.0)
            }
            Surface::Sphere { radius: r, .. } => (r * v.cos(), -r * v.sin(), r * v.sin(), r * v.cos()),
            Surface::Torus { major_radius: big, minor_radius: r, .. } => {
                (big + r * v.cos(), -r * v.sin(), r * v.sin(), r * v.cos())
            }
        }
    }

    pub fn point_at(&self, u: f64, v: f64) -> Vec3 {
        match (self, self.revolution()) {
            (Surface::Plane { origin, normal }, _) => {
                let x = plane_x(*normal);
                *origin + x * u + normal.cross(x) * v
            }
            (_, Some((o, a, x))) => {
                let (rho, _, h, _) = self.profile(v);
                o + (x * u.cos() + a.cross(x) * u.sin()) * rho + a * h
            }
            (_, None) => unreachable!("only planes have no axis"),
        }
    }

    /// (u, v) of the surface point nearest to `p`. Angles are in (−π, π].
    pub fn uv_of(&self, p: Vec3) -> [f64; 2] {
        let Some((o, a, x)) = self.revolution() else {
            let Surface::Plane { origin, normal } = *self else { unreachable!() };
            let (d, x) = (p - origin, plane_x(normal));
            return [d.dot(x), d.dot(normal.cross(x))];
        };
        let d = p - o;
        let (h, u) = (d.dot(a), d.dot(a.cross(x)).atan2(d.dot(x)));
        let rho = (d - a * h).length();
        let v = match *self {
            Surface::Sphere { .. } => h.atan2(rho),
            Surface::Torus { major_radius, .. } => h.atan2(rho - major_radius),
            _ => h,
        };
        [u, v]
    }

    pub fn distance(&self, p: Vec3) -> f64 {
        let Some((o, a, _)) = self.revolution() else {
            let Surface::Plane { origin, normal } = *self else { unreachable!() };
            return (p - origin).dot(normal).abs();
        };
        let d = p - o;
        let h = d.dot(a);
        let rho = (d - a * h).length();
        match *self {
            Surface::Cylinder { radius, .. } => (rho - radius).abs(),
            // Measured square to the cone's straight generating line.
            Surface::Cone { radius, semi_angle, .. } => (rho - radius - h * semi_angle.tan()).abs() * semi_angle.cos(),
            Surface::Sphere { radius, .. } => (rho.hypot(h) - radius).abs(),
            Surface::Torus { major_radius, minor_radius, .. } => ((rho - major_radius).hypot(h) - minor_radius).abs(),
            Surface::Plane { .. } => unreachable!(),
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
    /// `Some([lo, hi])` when the face is the whole (u, v) rectangle lo..hi of
    /// its surface, like a full turn of a cylinder or a whole sphere. Its
    /// loops then only record which edges bound it (a sphere's poles are
    /// points, so its loop does not close in (u, v)); area, volume and
    /// tessellation use the rectangle. Exported with trimType `rectangular`,
    /// as SMLib does.
    pub uv_rect: Option<[[f64; 2]; 2]>,
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
        self.faces.push(Face { surface, sense, outer, holes, uv_rect: None });
        FaceId(self.faces.len() - 1)
    }

    /// Adds a face that is the whole (u, v) rectangle `[lo, hi]` of its
    /// surface; see `Face::uv_rect`.
    pub fn add_rect_face(&mut self, surface: Surface, sense: bool, outer: Loop, rect: [[f64; 2]; 2]) -> FaceId {
        self.faces.push(Face { surface, sense, outer, holes: Vec::new(), uv_rect: Some(rect) });
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
        let holes = holes.iter().map(|h| Ring::Polygon(h.to_vec())).collect();
        Self::extrude(&Profile { outer: Ring::Polygon(outer.to_vec()), holes }, height)
    }

    /// Extrudes a profile straight up by `height`. Every hole becomes a
    /// through-hole (genus + 1). A circle becomes a cylinder face with one
    /// seam edge. Polygons may run either way. The profile should pass
    /// `Profile::validate`; this only asserts the basics.
    pub fn extrude(profile: &Profile, height: f64) -> Self {
        assert!(profile.rings().all(Ring::is_proper) && height > TOLERANCE);
        let mut b = Brep::new();
        let (x, z) = (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
        let up = z * height;
        // Each ring with its direction seen from +Z: outer counter-clockwise.
        let rings: Vec<(Ring, bool)> = std::iter::once((profile.outer.oriented(true), true))
            .chain(profile.holes.iter().map(|h| (h.oriented(false), false)))
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
                    faces.push(b.add_rect_face(surface, *ccw, Loop { coedges }, [[0.0, 0.0], [TAU, height]]));
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
        Self::extrude(&Profile::new(Ring::Circle { center: base, radius }), height)
    }

    /// Revolves a closed polygon a full turn round the Z axis. Points are
    /// `[r, z]`: distance from the axis (≥ 0) and height. The profile should
    /// pass `check_revolve_profile`.
    ///
    /// Each profile edge sweeps out one face: a horizontal edge a flat disc or
    /// ring, a vertical one a cylinder, a slanted one a cone (with its tip on
    /// the axis if the edge reaches it). Edges lying on the axis sweep
    /// nothing. Each corner off the axis sweeps a circle; each curved face is
    /// closed by a seam at u = 0, used twice, as for `extrude`.
    pub fn revolve(profile: &[[f64; 2]]) -> Self {
        assert!(check_revolve_profile(profile).is_ok(), "invalid revolve profile");
        let mut pts = profile.to_vec();
        if signed_area_rz(&pts) < 0.0 {
            pts.reverse(); // counter-clockwise in (r, z): material on the left
        }
        let n = pts.len();
        let on_axis = |p: [f64; 2]| p[0] <= TOLERANCE;
        let (x, z) = (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
        let at = |p: [f64; 2]| Vec3::new(p[0], 0.0, p[1]);

        let mut b = Brep::new();
        // One vertex per corner that a circle or a cone tip needs.
        let mut vertex: Vec<Option<VertexId>> = vec![None; n];
        let mut circle: Vec<Option<EdgeId>> = vec![None; n];
        for i in 0..n {
            // A corner on the axis needs a vertex only as a cone's tip: the
            // centre of a flat disc is not on any edge.
            let (prev, next) = (pts[(i + n - 1) % n], pts[(i + 1) % n]);
            let slanted_to = |q: [f64; 2]| !on_axis(q) && (q[1] - pts[i][1]).abs() > TOLERANCE;
            let tip = on_axis(pts[i]) && (slanted_to(prev) || slanted_to(next));
            if !on_axis(pts[i]) || tip {
                vertex[i] = Some(b.add_vertex(at(pts[i])));
            }
            if let (false, Some(v)) = (on_axis(pts[i]), vertex[i]) {
                let c = Curve::Circle { center: z * pts[i][1], axis: z, ref_dir: x, radius: pts[i][0] };
                circle[i] = Some(b.add_edge(c, v, v));
            }
        }

        let mut faces = Vec::new();
        for i in 0..n {
            let j = (i + 1) % n;
            let ([ri, zi], [rj, zj]) = (pts[i], pts[j]);
            let (dr, dz) = (rj - ri, zj - zi);
            if on_axis(pts[i]) && on_axis(pts[j]) {
                continue; // on the axis: sweeps no area
            }
            // Every face uses circle i forwards and circle j backwards, so
            // each circle is used once each way by its two faces.
            let (ci, cj) = (circle[i].map(|e| CoEdge { edge: e, forward: true }), circle[j].map(|e| CoEdge { edge: e, forward: false }));
            if dz.abs() <= TOLERANCE {
                // A disc or ring. The outward normal is (dz, −dr) in (r, z), so
                // it faces down when the edge runs outwards.
                let normal = z * -dr.signum();
                let (outer, hole) = if ri > rj { (ci, cj) } else { (cj, ci) };
                let outer = Loop { coedges: vec![outer.expect("the larger circle exists")] };
                let holes = hole.map(|h| Loop { coedges: vec![h] }).into_iter().collect();
                faces.push(b.add_face(Surface::Plane { origin: z * zi, normal }, true, outer, holes));
                continue;
            }
            let (vi, vj) = (vertex[i].expect("corner vertex"), vertex[j].expect("corner vertex"));
            let seam = b.add_edge(Curve::Line { origin: at(pts[i]), dir: (at(pts[j]) - at(pts[i])).normalize() }, vi, vj);
            let coedges: Vec<CoEdge> = [ci, Some(CoEdge { edge: seam, forward: true }), cj, Some(CoEdge { edge: seam, forward: false })]
                .into_iter()
                .flatten()
                .collect();
            let (surface, v0, v1) = if dr.abs() <= TOLERANCE {
                let s = Surface::Cylinder { origin: Vec3::new(0.0, 0.0, 0.0), axis: z, ref_dir: x, radius: ri };
                (s, zi.min(zj), zi.max(zj))
            } else {
                // The cone's axis points from its narrow end to its wide end,
                // so its semi-angle is positive: NVIDIA's validator requires
                // (0, π/2) (BA_515), although the schema allows (−π/2, π/2).
                // v runs from 0 at the narrow end.
                let ((r_narrow, z_narrow), z_wide) = if ri < rj { ((ri, zi), zj) } else { ((rj, zj), zi) };
                let axis = z * (z_wide - z_narrow).signum();
                let s = Surface::Cone { origin: z * z_narrow, axis, ref_dir: x, radius: r_narrow, semi_angle: (dr.abs() / dz.abs()).atan() };
                (s, 0.0, dz.abs())
            };
            // The surface normal points away from the axis; the face's points
            // away from the axis exactly when the edge runs upwards.
            faces.push(b.add_rect_face(surface, dz > 0.0, Loop { coedges }, [[0.0, v0], [TAU, v1]]));
        }
        b.add_shell(faces);
        b
    }

    /// Sphere with its poles on the Z axis, as SMLib builds it: one face, the
    /// two poles as vertices, and one seam edge (a half circle at u = 0 from
    /// the south pole to the north pole) used twice.
    pub fn make_sphere(center: Vec3, radius: f64) -> Self {
        assert!(radius > TOLERANCE);
        let (x, y, z) = (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
        let mut b = Brep::new();
        let south = b.add_vertex(center - z * radius);
        let north = b.add_vertex(center + z * radius);
        // In the plane of x and z: cos t · x + sin t · z, so axis = x × z = −y.
        let meridian = Curve::Circle { center, axis: -y, ref_dir: x, radius };
        let seam = b.add_edge(meridian, south, north);
        let coedges = vec![CoEdge { edge: seam, forward: true }, CoEdge { edge: seam, forward: false }];
        let surface = Surface::Sphere { center, axis: z, ref_dir: x, radius };
        let face = b.add_rect_face(surface, true, Loop { coedges }, [[0.0, -FRAC_PI_2], [TAU, FRAC_PI_2]]);
        b.add_shell(vec![face]);
        b
    }

    /// Ring torus round the Z axis, as SMLib builds it: one face, one vertex
    /// on the outer equator, and two closed seams (the outer equator and one
    /// tube circle), each used twice. Genus 1.
    pub fn make_torus(center: Vec3, major_radius: f64, minor_radius: f64) -> Self {
        assert!(minor_radius > TOLERANCE && major_radius > minor_radius);
        let (x, y, z) = (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
        let mut b = Brep::new();
        let v = b.add_vertex(center + x * (major_radius + minor_radius));
        let equator = b.add_edge(Curve::Circle { center, axis: z, ref_dir: x, radius: major_radius + minor_radius }, v, v);
        let tube = b.add_edge(Curve::Circle { center: center + x * major_radius, axis: -y, ref_dir: x, radius: minor_radius }, v, v);
        // In (u, v): along the equator (v = 0), up the tube circle, back, down.
        let ce = |edge, forward| CoEdge { edge, forward };
        let coedges = vec![ce(equator, true), ce(tube, true), ce(equator, false), ce(tube, false)];
        let surface = Surface::Torus { origin: center, axis: z, ref_dir: x, major_radius, minor_radius };
        let face = b.add_rect_face(surface, true, Loop { coedges }, [[0.0, 0.0], [TAU, TAU]]);
        b.add_shell(vec![face]);
        b
    }
}

/// Twice the signed area of an (r, z) polygon (positive = counter-clockwise).
fn signed_area_rz(p: &[[f64; 2]]) -> f64 {
    (0..p.len()).map(|i| p[i][0] * p[(i + 1) % p.len()][1] - p[(i + 1) % p.len()][0] * p[i][1]).sum()
}

/// Checks a profile for `Brep::revolve`: a simple polygon in the half-plane
/// r ≥ 0 that touches the axis, if at all, along one stretch of edges. A
/// lone corner on the axis would pinch the solid to a point; two separate
/// stretches would enclose a hollow.
pub fn check_revolve_profile(profile: &[[f64; 2]]) -> Result<(), ProfileError> {
    let as_xy: Vec<Vec3> = profile.iter().map(|p| Vec3::new(p[0], p[1], 0.0)).collect();
    if profile.len() < 3 || signed_area_rz(profile).abs() <= TOLERANCE {
        return Err(ProfileError::Degenerate { ring: 0 });
    }
    if profile.iter().any(|p| p[0] < -TOLERANCE) {
        return Err(ProfileError::CrossesAxis);
    }
    if polygon_self_intersects(&as_xy) {
        return Err(ProfileError::SelfIntersecting { ring: 0 });
    }
    let n = profile.len();
    let on = |i: usize| profile[i % n][0] <= TOLERANCE;
    let axis_edges = (0..n).filter(|&i| on(i) && on(i + 1)).count();
    // Stretches of consecutive axis edges.
    let stretches = (0..n).filter(|&i| on(i) && on(i + 1) && !(on(i + n - 1) && on(i))).count();
    let lone_corner = (0..n).any(|i| on(i) && !on(i + n - 1) && !on(i + 1));
    if lone_corner || stretches > 1 || (axis_edges == n) {
        return Err(ProfileError::TouchesAxisAtPoints);
    }
    Ok(())
}

// ─────────────────────────── Profiles ───────────────────────────

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

    /// At least 3 points enclosing some area, or a positive radius.
    pub fn is_proper(&self) -> bool {
        match self {
            Ring::Polygon(p) => p.len() >= 3 && signed_area_xy(p).abs() > TOLERANCE,
            Ring::Circle { radius, .. } => *radius > TOLERANCE,
        }
    }

    pub fn translated(&self, d: Vec3) -> Ring {
        match self {
            Ring::Polygon(p) => Ring::Polygon(p.iter().map(|&q| q + d).collect()),
            Ring::Circle { center, radius } => Ring::Circle { center: *center + d, radius: *radius },
        }
    }

    fn heights(&self) -> Vec<f64> {
        match self {
            Ring::Polygon(p) => p.iter().map(|q| q.z).collect(),
            Ring::Circle { center, .. } => vec![center.z],
        }
    }
}

/// A flat region in a plane z = const: an outer ring with holes in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub outer: Ring,
    pub holes: Vec<Ring>,
}

/// Why a profile cannot be extruded. Rings are numbered 0 for the outer
/// one and 1, 2, … for the holes; `hole` counts holes from 0.
#[derive(Debug, Clone, PartialEq)]
pub enum ProfileError {
    /// Fewer than 3 points or no area, or a radius ≤ 0.
    Degenerate { ring: usize },
    /// Not all in one plane z = const.
    NotFlat,
    SelfIntersecting { ring: usize },
    /// The hole crosses, touches or lies outside the outer boundary.
    HoleNotInside { hole: usize },
    /// Two holes cross, touch or lie one inside the other.
    HolesOverlap { a: usize, b: usize },
    /// A revolve profile reaches r < 0, across the axis.
    CrossesAxis,
    /// A revolve profile meets the axis at a lone corner (a pinch) or along
    /// more than one stretch (an enclosed hollow).
    TouchesAxisAtPoints,
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ProfileError::Degenerate { ring } => write!(f, "ring {ring} is degenerate"),
            ProfileError::NotFlat => write!(f, "rings are not in one plane z = const"),
            ProfileError::SelfIntersecting { ring } => write!(f, "ring {ring} crosses itself"),
            ProfileError::HoleNotInside { hole } => write!(f, "hole {hole} is not strictly inside the outer boundary"),
            ProfileError::HolesOverlap { a, b } => write!(f, "holes {a} and {b} overlap or touch"),
            ProfileError::CrossesAxis => write!(f, "the profile crosses the axis (r < 0)"),
            ProfileError::TouchesAxisAtPoints => {
                write!(f, "the profile must meet the axis along one stretch of edges, or not at all")
            }
        }
    }
}

impl std::error::Error for ProfileError {}

impl Profile {
    pub fn new(outer: Ring) -> Self {
        Self { outer, holes: Vec::new() }
    }

    /// The outer ring, then the holes.
    pub fn rings(&self) -> impl Iterator<Item = &Ring> {
        std::iter::once(&self.outer).chain(&self.holes)
    }

    pub fn translated(&self, d: Vec3) -> Profile {
        Profile { outer: self.outer.translated(d), holes: self.holes.iter().map(|h| h.translated(d)).collect() }
    }

    /// Checks that the region is well formed: flat, simple rings, every hole
    /// strictly inside the outer ring and clear of the other holes. Touching
    /// counts as overlapping, since it would make a non-manifold solid.
    pub fn validate(&self) -> Result<(), ProfileError> {
        let rings: Vec<&Ring> = self.rings().collect();
        if let Some(ring) = rings.iter().position(|r| !r.is_proper()) {
            return Err(ProfileError::Degenerate { ring });
        }
        let z0 = rings[0].heights()[0];
        if rings.iter().flat_map(|r| r.heights()).any(|z| (z - z0).abs() > GEOMETRY_TOLERANCE) {
            return Err(ProfileError::NotFlat);
        }
        for (ring, r) in rings.iter().enumerate() {
            if let Ring::Polygon(p) = r
                && polygon_self_intersects(p)
            {
                return Err(ProfileError::SelfIntersecting { ring });
            }
        }
        for (hole, h) in self.holes.iter().enumerate() {
            if !ring_inside(&self.outer, h) {
                return Err(ProfileError::HoleNotInside { hole });
            }
        }
        for a in 0..self.holes.len() {
            for b in a + 1..self.holes.len() {
                if !rings_apart(&self.holes[a], &self.holes[b]) {
                    return Err(ProfileError::HolesOverlap { a, b });
                }
            }
        }
        Ok(())
    }
}

// Plan-view (XY) tests behind `Profile::validate`.

fn xy(p: Vec3) -> P2 {
    [p.x, p.y]
}

fn dist_2d(a: P2, b: P2) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn edges_2d(p: &[Vec3]) -> impl Iterator<Item = (P2, P2)> + '_ {
    (0..p.len()).map(move |i| (xy(p[i]), xy(p[(i + 1) % p.len()])))
}

/// Do segments ab and cd cross or touch?
fn segments_touch(a: P2, b: P2, c: P2, d: P2) -> bool {
    let (d1, d2, d3, d4) = (cross2(c, d, a), cross2(c, d, b), cross2(a, b, c), cross2(a, b, d));
    if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
        return true;
    }
    // p lies on segment qr.
    let on = |q: P2, r: P2, p: P2, side: f64| {
        side.abs() <= TOLERANCE
            && p[0] >= q[0].min(r[0]) - TOLERANCE && p[0] <= q[0].max(r[0]) + TOLERANCE
            && p[1] >= q[1].min(r[1]) - TOLERANCE && p[1] <= q[1].max(r[1]) + TOLERANCE
    };
    on(c, d, a, d1) || on(c, d, b, d2) || on(a, b, c, d3) || on(a, b, d, d4)
}

fn polygon_self_intersects(p: &[Vec3]) -> bool {
    let e: Vec<(P2, P2)> = edges_2d(p).collect();
    let n = e.len();
    (0..n).any(|i| (i + 2..n).any(|j| !(i == 0 && j == n - 1) && segments_touch(e[i].0, e[i].1, e[j].0, e[j].1)))
}

fn polygons_touch(p: &[Vec3], q: &[Vec3]) -> bool {
    edges_2d(p).any(|(a, b)| edges_2d(q).any(|(c, d)| segments_touch(a, b, c, d)))
}

/// Even–odd ray casting.
fn point_in_polygon(pt: P2, p: &[Vec3]) -> bool {
    edges_2d(p).fold(false, |inside, (a, b)| {
        let crosses = (a[1] > pt[1]) != (b[1] > pt[1]) && pt[0] < a[0] + (pt[1] - a[1]) * (b[0] - a[0]) / (b[1] - a[1]);
        inside != crosses
    })
}

/// Distance from `pt` to the nearest edge of the polygon.
fn distance_to_polygon(pt: P2, p: &[Vec3]) -> f64 {
    edges_2d(p)
        .map(|(a, b)| {
            let ab = [b[0] - a[0], b[1] - a[1]];
            let t = (((pt[0] - a[0]) * ab[0] + (pt[1] - a[1]) * ab[1]) / (ab[0] * ab[0] + ab[1] * ab[1])).clamp(0.0, 1.0);
            dist_2d(pt, [a[0] + t * ab[0], a[1] + t * ab[1]])
        })
        .fold(f64::INFINITY, f64::min)
}

/// Is `inner` strictly inside `outer`?
fn ring_inside(outer: &Ring, inner: &Ring) -> bool {
    match (outer, inner) {
        (Ring::Polygon(p), Ring::Polygon(q)) => !polygons_touch(p, q) && point_in_polygon(xy(q[0]), p),
        (Ring::Polygon(p), Ring::Circle { center, radius }) => {
            point_in_polygon(xy(*center), p) && distance_to_polygon(xy(*center), p) > *radius
        }
        // A disc is convex, so a polygon whose corners are inside it is inside it.
        (Ring::Circle { center, radius }, Ring::Polygon(q)) => q.iter().all(|v| dist_2d(xy(*v), xy(*center)) < *radius),
        (Ring::Circle { center: c1, radius: r1 }, Ring::Circle { center: c2, radius: r2 }) => {
            dist_2d(xy(*c1), xy(*c2)) + r2 < *r1
        }
    }
}

/// Are the regions inside `a` and `b` disjoint, without touching?
fn rings_apart(a: &Ring, b: &Ring) -> bool {
    match (a, b) {
        (Ring::Polygon(p), Ring::Polygon(q)) => {
            !polygons_touch(p, q) && !point_in_polygon(xy(q[0]), p) && !point_in_polygon(xy(p[0]), q)
        }
        (Ring::Polygon(p), Ring::Circle { center, radius }) | (Ring::Circle { center, radius }, Ring::Polygon(p)) => {
            !point_in_polygon(xy(*center), p) && distance_to_polygon(xy(*center), p) > *radius
        }
        (Ring::Circle { center: c1, radius: r1 }, Ring::Circle { center: c2, radius: r2 }) => {
            dist_2d(xy(*c1), xy(*c2)) > r1 + r2
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

/// The angles base + kπ (any whole k) lying between `a` and `b`.
fn angles_in(base: f64, a: f64, b: f64) -> impl Iterator<Item = f64> {
    let (lo, hi) = (a.min(b), a.max(b));
    let first = base + ((lo - base) / std::f64::consts::PI).ceil() * std::f64::consts::PI;
    (0..).map(move |k| first + k as f64 * std::f64::consts::PI).take_while(move |&t| t <= hi)
}

/// ∫ from `a` to `b` of the v-parts of a surface of revolution's integrals:
/// [ρ h′, ρ² h′ − ρ ρ′ (o·a + h), ρ √(ρ′² + h′²)], for the flux through it
/// and its area. `oa` is o·a. 8-point Gauss–Legendre is exact for the
/// polynomials of cylinders and cones; spheres and tori are split into
/// pieces of at most π/8, which brings the error below rounding.
fn revolution_integrals(surface: &Surface, oa: f64, a: f64, b: f64) -> [f64; 3] {
    const NODES: [(f64, f64); 4] = [
        (0.183_434_642_495_649_8, 0.362_683_783_378_362),
        (0.525_532_409_916_329, 0.313_706_645_877_887_3),
        (0.796_666_477_413_626_7, 0.222_381_034_453_374_5),
        (0.960_289_856_497_536_2, 0.101_228_536_290_376_3),
    ];
    let pieces = match surface {
        Surface::Sphere { .. } | Surface::Torus { .. } => ((b - a).abs() / (std::f64::consts::PI / 8.0)).ceil().max(1.0) as usize,
        _ => 1,
    };
    let f = |v: f64| {
        let (rho, drho, h, dh) = surface.profile(v);
        [rho * dh, rho * rho * dh - rho * drho * (oa + h), rho * drho.hypot(dh)]
    };
    let width = (b - a) / pieces as f64;
    let mut sum = [0.0; 3];
    for p in 0..pieces {
        let mid = a + width * (p as f64 + 0.5);
        for (x, w) in NODES {
            for v in [mid - x * width / 2.0, mid + x * width / 2.0] {
                let y = f(v);
                for k in 0..3 {
                    sum[k] += w * width / 2.0 * y[k];
                }
            }
        }
    }
    sum
}

fn segments_for(angle: f64) -> usize {
    ((angle.abs() / TAU) * SEGMENTS_PER_TURN as f64).ceil().max(1.0) as usize
}

impl Brep {
    /// A face's loops in its own (u, v) parameter space, outer loop first,
    /// each curve in its coedge's direction. Angles jump by a full turn
    /// across a seam, so loops of trimmed faces close in (u, v) too. (A face
    /// with `uv_rect` need not close: a sphere's poles are single points.)
    pub fn face_uv_loops(&self, face: &Face) -> Vec<Vec<UvCurve>> {
        // Which parameters are angles, repeating every full turn.
        let (u_turns, v_turns) = match face.surface {
            Surface::Plane { .. } => (false, false),
            Surface::Torus { .. } => (true, true),
            _ => (true, false),
        };
        let shift = |from: f64, to: f64, on: bool| if on { ((to - from) / TAU).round() * TAU } else { 0.0 };
        std::iter::once(&face.outer)
            .chain(&face.holes)
            .map(|lp| {
                let mut curves: Vec<UvCurve> = lp.coedges.iter().map(|&ce| self.coedge_uv(&face.surface, ce)).collect();
                // Shift each curve by whole turns so it starts where the
                // previous one ended. Curved surfaces only have lines in (u, v).
                for k in 1..curves.len() {
                    if let (UvCurve::Line { a, b }, prev) = (curves[k], curves[k - 1].end()) {
                        let (su, sv) = (shift(a[0], prev[0], u_turns), shift(a[1], prev[1], v_turns));
                        curves[k] = UvCurve::Line { a: [a[0] + su, a[1] + sv], b: [b[0] + su, b[1] + sv] };
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
        let mid = curve.point_at((ts + te) / 2.0);
        match (surface, curve) {
            (Surface::Plane { .. }, Curve::Circle { center, axis, ref_dir, radius }) => {
                let c = surface.uv_of(*center);
                let dir = |d: Vec3| {
                    let q = surface.uv_of(*center + d);
                    [q[0] - c[0], q[1] - c[1]]
                };
                UvCurve::Arc { center: c, radius: *radius, x: dir(*ref_dir), y: dir(axis.cross(*ref_dir)), t0: ts, t1: te }
            }
            (Surface::Plane { .. }, Curve::Line { .. }) => {
                UvCurve::Line { a: surface.uv_of(curve.point_at(ts)), b: surface.uv_of(curve.point_at(te)) }
            }
            (_, Curve::Circle { .. }) => {
                // On a surface of revolution a circle is a parallel (u moves)
                // or a meridian of a sphere or torus (v moves), at one radian
                // per radian of t. Measure which way at the midpoint: a pole's
                // u is undefined, so the ends would not do.
                let m = surface.uv_of(mid);
                // A small step the way the coedge runs.
                let q = surface.uv_of(curve.point_at((ts + te) / 2.0 + 1e-6 * (te - ts).signum()));
                let wrap = |d: f64| d - (d / TAU).round() * TAU;
                let dir = [(wrap(q[0] - m[0]) / 1e-6).round(), (wrap(q[1] - m[1]) / 1e-6).round()];
                let half = (te - ts).abs() / 2.0;
                UvCurve::Line { a: [m[0] - dir[0] * half, m[1] - dir[1] * half], b: [m[0] + dir[0] * half, m[1] + dir[1] * half] }
            }
            (_, Curve::Line { .. }) => {
                // A straight line on a surface of revolution runs along the
                // axis at one u; take u from the midpoint, as a cone's tip has none.
                let u = surface.uv_of(mid)[0];
                let (a, b) = (surface.uv_of(curve.point_at(ts)), surface.uv_of(curve.point_at(te)));
                UvCurve::Line { a: [u, a[1]], b: [u, b[1]] }
            }
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
            if let Surface::Plane { normal, .. } = face.surface {
                let rings: Vec<Vec<Vec3>> = self
                    .face_uv_loops(face)
                    .iter()
                    .map(|lp| lp.iter().flat_map(UvCurve::samples).map(|[u, v]| face.surface.point_at(u, v)).collect())
                    .collect();
                tris.extend(triangulate_planar(&rings, if face.sense { normal } else { -normal }));
                continue;
            }
            // Curved faces: a grid over their (u, v) rectangle. Only spheres
            // and tori curve along v.
            let [[u0, v0], [u1, v1]] = self.face_rect(face);
            let nu = segments_for(u1 - u0);
            let nv = if matches!(face.surface, Surface::Sphere { .. } | Surface::Torus { .. }) { segments_for(v1 - v0) } else { 1 };
            let s = |i: usize, j: usize| {
                face.surface.point_at(u0 + (u1 - u0) * i as f64 / nu as f64, v0 + (v1 - v0) * j as f64 / nv as f64)
            };
            for i in 0..nu {
                for j in 0..nv {
                    let q = [s(i, j), s(i + 1, j), s(i + 1, j + 1), s(i, j + 1)];
                    for t in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
                        // Skip the slivers that collapse at a pole or a cone's tip.
                        if (t[1] - t[0]).cross(t[2] - t[0]).length() <= TOLERANCE {
                            continue;
                        }
                        // Counter-clockwise in (u, v) faces along the surface normal.
                        tris.push(if face.sense { t } else { [t[0], t[2], t[1]] });
                    }
                }
            }
        }
        tris
    }

    /// A curved face's (u, v) rectangle: `uv_rect`, or the bounds of its loops.
    fn face_rect(&self, face: &Face) -> [[f64; 2]; 2] {
        face.uv_rect.unwrap_or_else(|| {
            let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
            for p in self.face_uv_loops(face).iter().flatten().flat_map(|c| [c.start(), c.end()]) {
                lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                hi = [hi[0].max(p[0]), hi[1].max(p[1])];
            }
            [lo, hi]
        })
    }

    /// (area, volume contribution) of one face.
    ///
    /// Volume uses the divergence theorem, V = ⅓ ∬ p·n dA over the boundary.
    /// On a plane p·n is constant, and Green's theorem turns the area into an
    /// integral round the (u, v) loops, so a hole's loop, which runs the
    /// other way, subtracts itself.
    ///
    /// On a surface of revolution, with N = ∂S/∂u × ∂S/∂v = ρ (h′·r̂ − ρ′·a),
    /// p·N = ρ h′ (o·r̂(u) + ρ) − ρ ρ′ (o·a + h), where r̂(u) is the radial
    /// direction. The u part integrates in closed form; the v part
    /// (`revolution_integrals`) by Gauss–Legendre, exact for cylinders and
    /// cones and to rounding for spheres and tori.
    fn face_area_volume(&self, face: &Face) -> (f64, f64) {
        let Some((o, a, x)) = face.surface.revolution() else {
            let Surface::Plane { origin, normal } = face.surface else { unreachable!() };
            let area: f64 = self.face_uv_loops(face).iter().flatten().map(UvCurve::area_term).sum();
            return (area.abs(), origin.dot(normal) * area / 3.0);
        };
        let y = a.cross(x);
        // ∫ o·r̂(u) du
        let s = |u: f64| o.dot(x * u.sin() - y * u.cos());
        let q = |va: f64, vb: f64| revolution_integrals(&face.surface, o.dot(a), va, vb);
        if let Some([[u0, v0], [u1, v1]]) = face.uv_rect {
            let [q1, q2, q3] = q(v0, v1);
            let flux = (s(u1) - s(u0)) * q1 + (u1 - u0) * q2;
            // N points out of the solid exactly when the face's sense is true.
            let sign = if face.sense { 1.0 } else { -1.0 };
            return (((u1 - u0) * q3).abs(), sign * flux / 3.0);
        }
        // A trimmed face: Green's theorem round its loops, which run along
        // u = const (meridians) or v = const (parallels, which add nothing).
        // Loops run counter-clockwise in (u, v) exactly when N points out of
        // the solid, so the sign takes care of itself.
        let (mut area, mut flux) = (0.0, 0.0);
        for c in self.face_uv_loops(face).iter().flatten() {
            let ([ua, va], [ub, vb]) = (c.start(), c.end());
            let u = (ua + ub) / 2.0; // exact for meridians, where ua == ub
            let [q1, q2, q3] = q(va, vb);
            flux += s(u) * q1 + u * q2;
            area += u * q3;
        }
        (area.abs(), flux / 3.0)
    }

    /// Enclosed volume: exact for planes, cylinders and cones, to rounding
    /// for spheres and tori.
    pub fn volume(&self) -> f64 {
        self.faces.iter().map(|f| self.face_area_volume(f).1).sum()
    }

    /// Total face area, to the same accuracy as `volume`.
    pub fn surface_area(&self) -> f64 {
        self.faces.iter().map(|f| self.face_area_volume(f).0).sum()
    }

    /// Exact axis-aligned bounding box (lo, hi), or None if there are no
    /// vertices. Covers vertices, the bulge of circle edges and the bulge of
    /// whole turns of surfaces of revolution (a sphere's equator is on no
    /// edge). A partial turn is covered by sampling its surface.
    pub fn bounding_box(&self) -> Option<(Vec3, Vec3)> {
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        let mut add = |k: usize, value: f64| {
            lo[k] = lo[k].min(value);
            hi[k] = hi[k].max(value);
        };
        let c = |p: Vec3, k: usize| [p.x, p.y, p.z][k];
        for v in &self.vertices {
            for k in 0..3 {
                add(k, c(v.point, k));
            }
        }
        for (i, e) in self.edges.iter().enumerate() {
            if let Curve::Circle { axis, ref_dir, .. } = e.curve {
                // Coordinate k of a circle is extreme where tan t = y_k / x_k.
                let ((t0, t1), y) = (self.edge_range(EdgeId(i)), axis.cross(ref_dir));
                for k in 0..3 {
                    for t in angles_in(c(y, k).atan2(c(ref_dir, k)), t0, t1) {
                        add(k, c(e.curve.point_at(t), k));
                    }
                }
            }
        }
        for face in &self.faces {
            let Some((o, a, _)) = face.surface.revolution() else { continue };
            let [[u0, v0], [u1, v1]] = self.face_rect(face);
            if u1 - u0 < TAU - 1e-9 {
                for p in self.tessellate_face_grid(face) {
                    for k in 0..3 {
                        add(k, c(p, k));
                    }
                }
                continue;
            }
            for k in 0..3 {
                // Over a full turn, coordinate k spans o_k + h·a_k ± ρ·w with
                // w = √(1 − a_k²). For a sphere or torus these are
                // A·cos v + B·sin v plus constants: extreme where tan v = ±a_k / w.
                let (ak, w) = (c(a, k), (1.0 - c(a, k).powi(2)).max(0.0).sqrt());
                let mut vs = vec![v0, v1];
                if matches!(face.surface, Surface::Sphere { .. } | Surface::Torus { .. }) {
                    vs.extend(angles_in(ak.atan2(w), v0, v1));
                    vs.extend(angles_in((-ak).atan2(w), v0, v1));
                }
                for v in vs {
                    let (rho, _, h, _) = face.surface.profile(v);
                    add(k, c(o, k) + h * ak + rho * w);
                    add(k, c(o, k) + h * ak - rho * w);
                }
            }
        }
        (lo[0] <= hi[0]).then(|| (Vec3::new(lo[0], lo[1], lo[2]), Vec3::new(hi[0], hi[1], hi[2])))
    }

    /// Grid points of a curved face, for bounding a partial turn.
    fn tessellate_face_grid(&self, face: &Face) -> Vec<Vec3> {
        let [[u0, v0], [u1, v1]] = self.face_rect(face);
        let (nu, nv) = (segments_for(u1 - u0), segments_for(v1 - v0).max(4));
        (0..=nu)
            .flat_map(|i| (0..=nv).map(move |j| (i, j)))
            .map(|(i, j)| face.surface.point_at(u0 + (u1 - u0) * i as f64 / nu as f64, v0 + (v1 - v0) * j as f64 / nv as f64))
            .collect()
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
        let b = Brep::extrude(&Profile { outer: Ring::Polygon(square(0.0, 0.0, 2.0)), holes: vec![hole] }, 2.0);
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
        let b = Brep::extrude(&Profile { outer, holes: vec![Ring::Polygon(square(0.0, 0.0, 1.0))] }, 1.0);
        assert_eq!(b.validate(), Ok(()));
        assert_eq!(b.genus(), Some(1));
        assert!(approx(b.volume(), 9.0 * pi - 4.0));
    }

    #[test]
    fn uv_loops_close_across_the_seam() {
        let hole = Ring::Circle { center: Vec3::new(0.0, 0.0, 0.0), radius: 1.0 };
        let outer = Ring::Circle { center: Vec3::new(0.0, 0.0, 0.0), radius: 2.0 };
        let b = Brep::extrude(&Profile { outer, holes: vec![hole] }, 1.0);
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
            Brep::extrude(&Profile { outer: Ring::Polygon(square(0.0, 0.0, 2.0)), holes: vec![hole] }, 2.0),
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

    fn circle(x: f64, y: f64, radius: f64) -> Ring {
        Ring::Circle { center: Vec3::new(x, y, 0.0), radius }
    }

    #[test]
    fn profile_validation_accepts_a_drilled_plate() {
        let plate = Profile {
            outer: Ring::Polygon(square(0.0, 0.0, 4.0)),
            holes: vec![circle(-2.0, 0.0, 1.0), Ring::Polygon(square(2.0, 0.0, 1.0)), circle(0.0, 3.0, 0.5)],
        };
        assert_eq!(plate.validate(), Ok(()));
        let disc = Profile { outer: circle(0.0, 0.0, 3.0), holes: vec![Ring::Polygon(square(0.0, 0.0, 1.5))] };
        assert_eq!(disc.validate(), Ok(()), "square corners at 2.12 < 3");
    }

    #[test]
    fn profile_validation_rejects_bad_holes() {
        let plate = |holes: Vec<Ring>| Profile { outer: Ring::Polygon(square(0.0, 0.0, 2.0)), holes };
        // Outside, crossing the edge, and touching it (tangent at x = 2).
        for hole in [circle(5.0, 0.0, 1.0), circle(1.5, 0.0, 1.0), circle(1.0, 0.0, 1.0), Ring::Polygon(square(2.0, 0.0, 0.5))] {
            assert_eq!(plate(vec![hole.clone()]).validate(), Err(ProfileError::HoleNotInside { hole: 0 }), "{hole:?}");
        }
        // Crossing, tangent, and one inside the other.
        for (a, b) in [
            (circle(-0.5, 0.0, 0.8), circle(0.5, 0.0, 0.8)),
            (circle(-0.5, 0.0, 0.5), circle(0.5, 0.0, 0.5)),
            (circle(0.0, 0.0, 1.5), Ring::Polygon(square(0.0, 0.0, 0.5))),
            (Ring::Polygon(square(-0.5, 0.0, 0.6)), Ring::Polygon(square(0.5, 0.0, 0.6))),
        ] {
            assert_eq!(plate(vec![a, b]).validate(), Err(ProfileError::HolesOverlap { a: 0, b: 1 }));
        }
        // A square hole whose corners reach outside a circular outline.
        let disc = Profile { outer: circle(0.0, 0.0, 1.0), holes: vec![Ring::Polygon(square(0.0, 0.0, 0.8))] };
        assert_eq!(disc.validate(), Err(ProfileError::HoleNotInside { hole: 0 }));
    }

    #[test]
    fn profile_validation_rejects_bad_rings() {
        let p = |x, y| Vec3::new(x, y, 0.0);
        // Lopsided, so its two lobes do not cancel to zero area.
        let bow_tie = Profile::new(Ring::Polygon(vec![p(0.0, 0.0), p(3.0, 3.0), p(3.0, 0.0), p(0.0, 2.0)]));
        assert_eq!(bow_tie.validate(), Err(ProfileError::SelfIntersecting { ring: 0 }));
        let flat_line = Profile::new(Ring::Polygon(vec![p(0.0, 0.0), p(1.0, 0.0), p(2.0, 0.0)]));
        assert_eq!(flat_line.validate(), Err(ProfileError::Degenerate { ring: 0 }));
        let zero_hole = Profile { outer: Ring::Polygon(square(0.0, 0.0, 2.0)), holes: vec![circle(0.0, 0.0, 0.0)] };
        assert_eq!(zero_hole.validate(), Err(ProfileError::Degenerate { ring: 1 }));
        let raised = Ring::Circle { center: Vec3::new(0.0, 0.0, 1.0), radius: 0.5 };
        let tilted = Profile { outer: Ring::Polygon(square(0.0, 0.0, 2.0)), holes: vec![raised] };
        assert_eq!(tilted.validate(), Err(ProfileError::NotFlat));
    }

    const PI: f64 = std::f64::consts::PI;

    /// Volume of a revolved (r, z) polygon by Pappus: 2π ∬ r dA = 2π ∮ r²/2 dz,
    /// exact for straight edges. Independent of how `revolve` builds faces.
    fn pappus_volume(p: &[[f64; 2]]) -> f64 {
        let n = p.len();
        let s: f64 = (0..n)
            .map(|i| {
                let ([r0, z0], [r1, z1]) = (p[i], p[(i + 1) % n]);
                (z1 - z0) * (r0 * r0 + r0 * r1 + r1 * r1) / 6.0
            })
            .sum();
        2.0 * PI * s.abs()
    }

    #[test]
    fn revolved_rectangle_is_a_cylinder() {
        let (r, h) = (2.0, 3.0);
        let b = Brep::revolve(&[[0.0, 0.0], [r, 0.0], [r, h], [0.0, h]]);
        assert_eq!(b.counts(), (2, 3, 3), "same topology as make_cylinder");
        assert_eq!(b.validate(), Ok(()));
        assert!(approx(b.volume(), PI * r * r * h));
        assert!(approx(b.surface_area(), 2.0 * PI * r * h + 2.0 * PI * r * r));
    }

    #[test]
    fn revolved_triangle_is_a_cone_with_its_tip_on_the_axis() {
        let (r, h) = (3.0, 4.0);
        let b = Brep::revolve(&[[0.0, 0.0], [r, 0.0], [0.0, h]]);
        // Base circle's vertex + tip; base circle + seam; base disc + cone.
        assert_eq!(b.counts(), (2, 2, 2));
        assert_eq!(b.validate(), Ok(()));
        assert_eq!(b.genus(), Some(0));
        assert!(approx(b.volume(), PI * r * r * h / 3.0), "{}", b.volume());
        assert!(approx(b.surface_area(), PI * r * r + PI * r * (r * r + h * h).sqrt()));
    }

    #[test]
    fn revolved_profiles_match_pappus() {
        let cases: [&[[f64; 2]]; 5] = [
            // Frustum, either way round.
            &[[0.0, 0.0], [3.0, 0.0], [1.5, 2.0], [0.0, 2.0]],
            &[[0.0, 2.0], [1.5, 2.0], [3.0, 0.0], [0.0, 0.0]],
            // Tube (genus 1): its inner wall faces the axis.
            &[[1.0, 0.0], [3.0, 0.0], [3.0, 2.0], [1.0, 2.0]],
            // Tube with a cone flaring inwards, and a cone tip on top.
            &[[1.0, 0.0], [3.0, 0.0], [3.0, 2.0], [1.5, 2.5], [1.0, 1.0]],
            &[[0.0, -1.0], [2.0, -1.0], [2.0, 1.0], [1.0, 1.0], [0.0, 3.0]],
        ];
        for p in cases {
            let b = Brep::revolve(p);
            assert_eq!(b.validate(), Ok(()), "{p:?}");
            // NVIDIA's validator accepts only semi-angles in (0, π/2) (BA_515).
            for f in &b.faces {
                if let Surface::Cone { semi_angle, .. } = f.surface {
                    assert!(semi_angle > 0.0 && semi_angle < FRAC_PI_2, "{p:?}: {semi_angle}");
                }
            }
            let expected_genus = if p.iter().any(|q| q[0] == 0.0) { 0 } else { 1 };
            assert_eq!(b.genus(), Some(expected_genus), "{p:?}");
            assert!(approx(b.volume(), pappus_volume(p)), "{p:?}: {} vs {}", b.volume(), pappus_volume(p));
            let (area, vol) = mesh_area_volume(&b);
            assert!(approx_rel(area, b.surface_area(), 0.003) && approx_rel(vol, b.volume(), 0.003), "{p:?}");
        }
    }

    #[test]
    fn sphere_matches_smlib_topology_and_exact_values() {
        let r = 2.0;
        let b = Brep::make_sphere(Vec3::new(1.0, -1.0, 0.5), r);
        // Two poles, one seam, one face: 2 − 1 + 1 = 2.
        assert_eq!(b.counts(), (2, 1, 1));
        assert_eq!(b.validate(), Ok(()));
        assert_eq!(b.genus(), Some(0));
        assert!(approx(b.volume(), 4.0 / 3.0 * PI * r.powi(3)), "{}", b.volume());
        assert!(approx(b.surface_area(), 4.0 * PI * r * r));
        let (area, vol) = mesh_area_volume(&b);
        assert!(approx_rel(area, b.surface_area(), 0.003) && approx_rel(vol, b.volume(), 0.005));
    }

    #[test]
    fn torus_matches_smlib_topology_and_exact_values() {
        let (big, r) = (5.0, 1.5);
        let b = Brep::make_torus(Vec3::new(0.0, 2.0, -1.0), big, r);
        // One vertex, two closed seams, one face: 1 − 2 + 1 = 0, genus 1.
        assert_eq!(b.counts(), (1, 2, 1));
        assert_eq!(b.validate(), Ok(()));
        assert_eq!(b.genus(), Some(1));
        assert!(approx(b.volume(), 2.0 * PI * PI * big * r * r), "{}", b.volume());
        assert!(approx(b.surface_area(), 4.0 * PI * PI * big * r));
        let (area, vol) = mesh_area_volume(&b);
        assert!(approx_rel(area, b.surface_area(), 0.003) && approx_rel(vol, b.volume(), 0.005));
    }

    #[test]
    fn bounding_boxes_reach_the_bulges() {
        let close = |a: Vec3, b: Vec3| (a - b).length() < 1e-9;
        let (lo, hi) = Brep::make_sphere(Vec3::new(1.0, 2.0, 3.0), 2.0).bounding_box().unwrap();
        assert!(close(lo, Vec3::new(-1.0, 0.0, 1.0)) && close(hi, Vec3::new(3.0, 4.0, 5.0)), "{lo:?} {hi:?}");
        let (lo, hi) = Brep::make_torus(Vec3::new(0.0, 0.0, 0.0), 5.0, 1.0).bounding_box().unwrap();
        assert!(close(lo, Vec3::new(-6.0, -6.0, -1.0)) && close(hi, Vec3::new(6.0, 6.0, 1.0)), "{lo:?} {hi:?}");
        let (lo, hi) = Brep::make_cylinder(Vec3::new(0.0, 0.0, 0.0), 1.0, 2.0).bounding_box().unwrap();
        assert!(close(lo, Vec3::new(-1.0, -1.0, 0.0)) && close(hi, Vec3::new(1.0, 1.0, 2.0)), "{lo:?} {hi:?}");
    }

    #[test]
    fn revolve_profile_check_rejects_bad_profiles() {
        let check = |p: &[[f64; 2]]| check_revolve_profile(p);
        assert_eq!(check(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]), Ok(()));
        assert_eq!(check(&[[-1.0, 0.0], [1.0, 0.0], [0.0, 1.0]]), Err(ProfileError::CrossesAxis));
        // A diamond touching the axis at one corner would pinch to a point.
        assert_eq!(check(&[[0.0, 0.0], [1.0, -1.0], [2.0, 0.0], [1.0, 1.0]]), Err(ProfileError::TouchesAxisAtPoints));
        // A C shape meeting the axis twice would enclose a hollow.
        let c_shape = [[0.0, 0.0], [2.0, 0.0], [2.0, 3.0], [0.0, 3.0], [0.0, 2.0], [1.0, 2.0], [1.0, 1.0], [0.0, 1.0]];
        assert_eq!(check(&c_shape), Err(ProfileError::TouchesAxisAtPoints));
        assert_eq!(check(&[[1.0, 0.0], [2.0, 0.0], [3.0, 0.0]]), Err(ProfileError::Degenerate { ring: 0 }));
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
