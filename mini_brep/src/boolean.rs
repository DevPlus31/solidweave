//! Boolean operations (union, difference, intersection) on closed solids
//! whose faces are all planar.
//!
//! Method:
//! 1. Cut each solid into convex polygons: its faces' triangles.
//! 2. Clip each set against a BSP tree of the other solid. Each tree node
//!    splits space by one face plane; polygons end up in front of, behind or
//!    on each plane, so coplanar faces (a pocket flush with a top face, two
//!    boxes sharing a face) are resolved consistently. This is the algorithm
//!    of Evan Wallace's csg.js (MIT licence), reimplemented in Rust.
//! 3. Rebuild a B-rep from the polygon soup: weld coincident vertices, close
//!    T-junctions (a corner of one piece on the edge of another), merge the
//!    pieces of each plane into faces with holes, drop vertices in the
//!    middle of straight edges, group faces into shells, and validate.
//!
//! Curved faces are not supported yet.

use crate::{Brep, BrepError, Face, Surface, Vec3, VertexId};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BooleanOp {
    Union,
    /// The first solid minus the second.
    Difference,
    Intersection,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BooleanError {
    /// An input has a curved face.
    NotPlanar,
    /// An input is not a valid closed solid.
    InvalidInput(Vec<BrepError>),
    /// The pieces did not join into a valid solid.
    InvalidResult(String),
}

impl std::fmt::Display for BooleanError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            BooleanError::NotPlanar => write!(f, "booleans support only solids with planar faces so far"),
            BooleanError::InvalidInput(e) => write!(f, "an input is not a valid solid: {e:?}"),
            BooleanError::InvalidResult(s) => write!(f, "the result is not a valid solid: {s}"),
        }
    }
}

impl std::error::Error for BooleanError {}

impl Brep {
    /// `self` op `other`. Both must be valid closed solids with planar
    /// faces. The result may be empty (no faces), or have several shells:
    /// separate pieces, or a hollow inside.
    pub fn boolean(&self, other: &Brep, op: BooleanOp) -> Result<Brep, BooleanError> {
        for b in [self, other] {
            if b.faces.iter().any(|f| !matches!(f.surface, Surface::Plane { .. })) {
                return Err(BooleanError::NotPlanar);
            }
            b.validate().map_err(BooleanError::InvalidInput)?;
        }
        // Tolerances scale with the model, as split points carry rounding.
        let scale = [self, other]
            .iter()
            .filter_map(|b| b.bounding_box())
            .map(|(lo, hi)| (hi - lo).length())
            .fold(1.0_f64, f64::max);
        let eps = 1e-9 * scale;

        let mut a = Bsp::new(polygons(self), eps);
        let mut b = Bsp::new(polygons(other), eps);
        // csg.js's sequences: clip each solid against the other, inverting
        // to select inside rather than outside parts.
        match op {
            BooleanOp::Union => {
                a.clip_to(&b, eps);
                b.clip_to(&a, eps);
                b.invert();
                b.clip_to(&a, eps);
                b.invert();
                a.build(b.all_polygons(), eps);
            }
            BooleanOp::Difference => {
                a.invert();
                a.clip_to(&b, eps);
                b.clip_to(&a, eps);
                b.invert();
                b.clip_to(&a, eps);
                b.invert();
                a.build(b.all_polygons(), eps);
                a.invert();
            }
            BooleanOp::Intersection => {
                a.invert();
                b.clip_to(&a, eps);
                b.invert();
                a.clip_to(&b, eps);
                b.clip_to(&a, eps);
                a.build(b.all_polygons(), eps);
                a.invert();
            }
        }
        rebuild(a.all_polygons(), 100.0 * eps)
    }
}

// ───────────────────────────── BSP clipping ─────────────────────────────

/// n·p = w; the front side is where n points.
#[derive(Debug, Clone, Copy)]
struct Plane {
    n: Vec3,
    w: f64,
}

impl Plane {
    fn flipped(self) -> Plane {
        Plane { n: -self.n, w: -self.w }
    }
}

/// A convex polygon, wound counter-clockwise round its plane's normal.
#[derive(Debug, Clone)]
struct Polygon {
    verts: Vec<Vec3>,
    plane: Plane,
}

impl Polygon {
    fn flip(&mut self) {
        self.verts.reverse();
        self.plane = self.plane.flipped();
    }
}

/// Every face's triangles, each carrying its face's exact plane.
fn polygons(b: &Brep) -> Vec<Polygon> {
    b.faces
        .iter()
        .flat_map(|f: &Face| {
            let Surface::Plane { origin, normal } = f.surface else { unreachable!("checked planar") };
            let n = if f.sense { normal } else { -normal };
            let plane = Plane { n, w: n.dot(origin) };
            b.tessellate_face(f).into_iter().map(move |t| Polygon { verts: t.to_vec(), plane })
        })
        .collect()
}

/// Where a polygon lies relative to a plane.
enum Side {
    CoplanarFront(Polygon),
    CoplanarBack(Polygon),
    Front(Polygon),
    Back(Polygon),
    /// Cut in two: the parts in front and behind.
    Spanning(Option<Polygon>, Option<Polygon>),
}

fn classify(plane: Plane, poly: Polygon, eps: f64) -> Side {
    const ON: u8 = 0;
    const FRONT: u8 = 1;
    const BACK: u8 = 2;
    let side = |v: Vec3| {
        let t = plane.n.dot(v) - plane.w;
        if t < -eps { BACK } else if t > eps { FRONT } else { ON }
    };
    let sides: Vec<u8> = poly.verts.iter().map(|&v| side(v)).collect();
    match sides.iter().fold(ON, |acc, s| acc | s) {
        ON if plane.n.dot(poly.plane.n) > 0.0 => Side::CoplanarFront(poly),
        ON => Side::CoplanarBack(poly),
        FRONT => Side::Front(poly),
        BACK => Side::Back(poly),
        _ => {
            let (mut front, mut back) = (Vec::new(), Vec::new());
            let n = poly.verts.len();
            for i in 0..n {
                let (vi, vj, si, sj) = (poly.verts[i], poly.verts[(i + 1) % n], sides[i], sides[(i + 1) % n]);
                if si != BACK {
                    front.push(vi);
                }
                if si != FRONT {
                    back.push(vi);
                }
                if si | sj == FRONT | BACK {
                    let t = (plane.w - plane.n.dot(vi)) / plane.n.dot(vj - vi);
                    let v = vi + (vj - vi) * t;
                    front.push(v);
                    back.push(v);
                }
            }
            let keep = |verts: Vec<Vec3>| (verts.len() >= 3).then_some(Polygon { verts, plane: poly.plane });
            Side::Spanning(keep(front), keep(back))
        }
    }
}

/// A BSP tree over polygons: each node's plane splits space, and the node
/// keeps the polygons lying in that plane.
#[derive(Default)]
struct Bsp {
    plane: Option<Plane>,
    front: Option<Box<Bsp>>,
    back: Option<Box<Bsp>>,
    polygons: Vec<Polygon>,
}

impl Bsp {
    fn new(polygons: Vec<Polygon>, eps: f64) -> Bsp {
        let mut b = Bsp::default();
        b.build(polygons, eps);
        b
    }

    /// Swaps inside and outside.
    fn invert(&mut self) {
        for p in &mut self.polygons {
            p.flip();
        }
        if let Some(plane) = &mut self.plane {
            *plane = plane.flipped();
        }
        for child in [&mut self.front, &mut self.back].into_iter().flatten() {
            child.invert();
        }
        std::mem::swap(&mut self.front, &mut self.back);
    }

    /// The parts of `polygons` outside this tree's solid.
    fn clip_polygons(&self, polygons: Vec<Polygon>, eps: f64) -> Vec<Polygon> {
        let Some(plane) = self.plane else { return polygons };
        let (mut front, mut back) = (Vec::new(), Vec::new());
        for p in polygons {
            match classify(plane, p, eps) {
                Side::CoplanarFront(p) | Side::Front(p) => front.push(p),
                Side::CoplanarBack(p) | Side::Back(p) => back.push(p),
                Side::Spanning(f, b) => {
                    front.extend(f);
                    back.extend(b);
                }
            }
        }
        let mut kept = match &self.front {
            Some(node) => node.clip_polygons(front, eps),
            None => front,
        };
        // Behind a leaf is inside the solid: dropped.
        if let Some(node) = &self.back {
            kept.extend(node.clip_polygons(back, eps));
        }
        kept
    }

    /// Removes the parts of this tree's polygons that lie inside `other`.
    fn clip_to(&mut self, other: &Bsp, eps: f64) {
        self.polygons = other.clip_polygons(std::mem::take(&mut self.polygons), eps);
        for child in [&mut self.front, &mut self.back].into_iter().flatten() {
            child.clip_to(other, eps);
        }
    }

    fn all_polygons(&self) -> Vec<Polygon> {
        let mut all = self.polygons.clone();
        for child in [&self.front, &self.back].into_iter().flatten() {
            all.extend(child.all_polygons());
        }
        all
    }

    fn build(&mut self, polygons: Vec<Polygon>, eps: f64) {
        let Some(first) = polygons.first() else { return };
        let plane = *self.plane.get_or_insert(first.plane);
        let (mut front, mut back) = (Vec::new(), Vec::new());
        for p in polygons {
            match classify(plane, p, eps) {
                Side::CoplanarFront(p) | Side::CoplanarBack(p) => self.polygons.push(p),
                Side::Front(p) => front.push(p),
                Side::Back(p) => back.push(p),
                Side::Spanning(f, b) => {
                    front.extend(f);
                    back.extend(b);
                }
            }
        }
        if !front.is_empty() {
            self.front.get_or_insert_with(Default::default).build(front, eps);
        }
        if !back.is_empty() {
            self.back.get_or_insert_with(Default::default).build(back, eps);
        }
    }
}

// ───────────────────────────── Rebuilding ─────────────────────────────

/// Joins clipped polygons into a B-rep. `tol` is the welding distance.
fn rebuild(polygons: Vec<Polygon>, tol: f64) -> Result<Brep, BooleanError> {
    // 1. Weld vertices that coincide within tol.
    let mut pool: Vec<Vec3> = Vec::new();
    let mut weld = |p: Vec3| -> usize {
        match pool.iter().position(|q| (*q - p).length() <= tol) {
            Some(i) => i,
            None => {
                pool.push(p);
                pool.len() - 1
            }
        }
    };
    // Group polygons by plane, so coplanar pieces become one face.
    let mut planes: Vec<Plane> = Vec::new();
    let mut polys: Vec<(usize, Vec<usize>)> = Vec::new();
    for p in polygons {
        let mut ids: Vec<usize> = p.verts.iter().map(|&v| weld(v)).collect();
        ids.dedup();
        while ids.len() > 1 && ids.first() == ids.last() {
            ids.pop();
        }
        if ids.len() < 3 {
            continue; // a sliver
        }
        let group = match planes.iter().position(|q| q.n.dot(p.plane.n) > 1.0 - 1e-9 && (q.w - p.plane.w).abs() <= tol) {
            Some(g) => g,
            None => {
                planes.push(p.plane);
                planes.len() - 1
            }
        };
        polys.push((group, ids));
    }

    // 2. Close T-junctions: a vertex lying on another polygon's edge is
    //    inserted into that edge, so neighbours share whole edges.
    for (_, ids) in &mut polys {
        let mut out = Vec::with_capacity(ids.len());
        for k in 0..ids.len() {
            let (a, b) = (ids[k], ids[(k + 1) % ids.len()]);
            out.push(a);
            let (pa, pb) = (pool[a], pool[b]);
            let ab = pb - pa;
            let len2 = ab.dot(ab);
            let mut inside: Vec<(f64, usize)> = pool
                .iter()
                .enumerate()
                .filter(|&(i, _)| i != a && i != b)
                .filter_map(|(i, &p)| {
                    let t = (p - pa).dot(ab) / len2;
                    (t > 0.0 && t < 1.0 && (pa + ab * t - p).length() <= tol).then_some((t, i))
                })
                .collect();
            inside.sort_by(|x, y| x.0.total_cmp(&y.0));
            out.extend(inside.into_iter().map(|(_, i)| i));
        }
        *ids = out;
    }

    // 3. Per plane, edges shared by two pieces (in opposite directions)
    //    are interior; the rest bound the merged faces.
    let mut regions: Vec<Region> = Vec::new();
    for (g, plane) in planes.iter().enumerate() {
        let mut edges: HashMap<(usize, usize), i32> = HashMap::new();
        for (_, ids) in polys.iter().filter(|(pg, _)| *pg == g) {
            for k in 0..ids.len() {
                let (a, b) = (ids[k], ids[(k + 1) % ids.len()]);
                match edges.get_mut(&(b, a)) {
                    Some(c) if *c > 0 => *c -= 1,
                    _ => *edges.entry((a, b)).or_default() += 1,
                }
            }
        }
        let boundary: Vec<(usize, usize)> = edges.into_iter().filter(|&(_, c)| c > 0).map(|(e, _)| e).collect();
        let loops = chain_loops(boundary, &pool, plane.n)?;
        regions.extend(assign_holes(loops, &pool, plane.n)?);
    }

    // 4. Drop vertices in the middle of straight edges: those with exactly
    //    two neighbours, in line with them. Every face using such a vertex
    //    passes straight through it, so all of them drop it together.
    let mut neighbours: HashMap<usize, HashSet<usize>> = HashMap::new();
    for lp in regions.iter().flat_map(|(o, hs)| std::iter::once(o).chain(hs)) {
        for k in 0..lp.len() {
            let (a, b) = (lp[k], lp[(k + 1) % lp.len()]);
            neighbours.entry(a).or_default().insert(b);
            neighbours.entry(b).or_default().insert(a);
        }
    }
    let straight: HashSet<usize> = neighbours
        .iter()
        .filter(|(_, ns)| ns.len() == 2)
        .filter(|&(&v, ns)| {
            let mut it = ns.iter();
            let (u, w) = (pool[*it.next().unwrap()], pool[*it.next().unwrap()]);
            let p = pool[v];
            (u - p).cross(w - p).length() <= tol * (w - u).length() && (u - p).dot(w - p) < 0.0
        })
        .map(|(&v, _)| v)
        .collect();

    // 5. Build the B-rep.
    let mut b = Brep::new();
    let mut vid: HashMap<usize, VertexId> = HashMap::new();
    let mut vertex = |b: &mut Brep, i: usize| *vid.entry(i).or_insert_with(|| b.add_vertex(pool[i]));
    let mut faces = Vec::new();
    for (outer, holes) in regions {
        let clean = |lp: &Vec<usize>| -> Vec<usize> { lp.iter().copied().filter(|v| !straight.contains(v)).collect() };
        let outer = clean(&outer);
        if outer.len() < 3 {
            continue;
        }
        let outer_ids: Vec<VertexId> = outer.iter().map(|&i| vertex(&mut b, i)).collect();
        let hole_ids: Vec<Vec<VertexId>> = holes
            .iter()
            .map(clean)
            .filter(|h| h.len() >= 3)
            .map(|h| h.iter().map(|&i| vertex(&mut b, i)).collect())
            .collect();
        let hole_refs: Vec<&[VertexId]> = hole_ids.iter().map(Vec::as_slice).collect();
        faces.push(b.add_planar_face(&outer_ids, &hole_refs));
    }

    // 6. Shells: faces connected through shared edges.
    let mut parent: Vec<usize> = (0..faces.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut first_face_of_edge: HashMap<usize, usize> = HashMap::new();
    for (fi, f) in faces.iter().enumerate() {
        let face = &b.faces[f.0];
        for ce in std::iter::once(&face.outer).chain(&face.holes).flat_map(|l| &l.coedges) {
            match first_face_of_edge.get(&ce.edge.0) {
                Some(&other) => {
                    let (ra, rb) = (root(&mut parent, fi), root(&mut parent, other));
                    parent[ra] = rb;
                }
                None => {
                    first_face_of_edge.insert(ce.edge.0, fi);
                }
            }
        }
    }
    let mut shells: HashMap<usize, Vec<crate::FaceId>> = HashMap::new();
    let mut order = Vec::new();
    for (fi, &f) in faces.iter().enumerate() {
        let r = root(&mut parent, fi);
        if !shells.contains_key(&r) {
            order.push(r);
        }
        shells.entry(r).or_default().push(f);
    }
    for r in order {
        b.add_shell(shells.remove(&r).unwrap());
    }

    b.validate().map_err(|e| BooleanError::InvalidResult(format!("{e:?}")))?;
    Ok(b)
}

/// Joins directed boundary edges into closed loops. Where several edges
/// leave one vertex (two loops touching at a corner), takes the sharpest
/// right turn, which keeps the loops apart.
fn chain_loops(edges: Vec<(usize, usize)>, pool: &[Vec3], n: Vec3) -> Result<Vec<Vec<usize>>, BooleanError> {
    let mut out: HashMap<usize, Vec<usize>> = HashMap::new();
    for &(a, b) in &edges {
        out.entry(a).or_default().push(b);
    }
    let mut loops = Vec::new();
    let mut left = edges.len();
    while left > 0 {
        let start = *out.iter().find(|(_, v)| !v.is_empty()).unwrap().0;
        let mut lp = vec![start];
        let (mut prev, mut cur) = (start, out.get_mut(&start).unwrap().pop().unwrap());
        left -= 1;
        while cur != start {
            lp.push(cur);
            let nexts = out.get_mut(&cur).filter(|v| !v.is_empty()).ok_or_else(|| {
                BooleanError::InvalidResult(format!("boundary does not close at vertex {cur}"))
            })?;
            let d_in = pool[cur] - pool[prev];
            // Signed turn angle from the incoming direction, round the normal.
            let turn = |next: usize| {
                let d = pool[next] - pool[cur];
                n.dot(d_in.cross(d)).atan2(d_in.dot(d))
            };
            let k = (0..nexts.len()).min_by(|&i, &j| turn(nexts[i]).total_cmp(&turn(nexts[j]))).unwrap();
            prev = cur;
            cur = nexts.swap_remove(k);
            left -= 1;
        }
        loops.push(lp);
    }
    Ok(loops)
}

/// Loops running counter-clockwise round `n` are outer boundaries, the
/// others holes; each hole goes to the smallest outer loop containing it.
/// An outer loop and its holes, as vertex indices.
type Region = (Vec<usize>, Vec<Vec<usize>>);

fn assign_holes(loops: Vec<Vec<usize>>, pool: &[Vec3], n: Vec3) -> Result<Vec<Region>, BooleanError> {
    let area = |lp: &Vec<usize>| -> f64 {
        let mut s = Vec3::new(0.0, 0.0, 0.0);
        for k in 0..lp.len() {
            s = s + pool[lp[k]].cross(pool[lp[(k + 1) % lp.len()]]);
        }
        0.5 * s.dot(n)
    };
    // 2D frame in the plane, for point-in-polygon tests.
    let u = crate::plane_x(n);
    let v = n.cross(u);
    let flat = |p: Vec3| [p.dot(u), p.dot(v)];
    let contains = |lp: &Vec<usize>, p: [f64; 2]| {
        let mut inside = false;
        for k in 0..lp.len() {
            let (a, b) = (flat(pool[lp[k]]), flat(pool[lp[(k + 1) % lp.len()]]));
            if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) * (b[0] - a[0]) / (b[1] - a[1]) {
                inside = !inside;
            }
        }
        inside
    };
    let (outers, holes): (Vec<_>, Vec<_>) = loops.into_iter().partition(|lp| area(lp) > 0.0);
    let mut regions: Vec<Region> = outers.into_iter().map(|o| (o, Vec::new())).collect();
    for h in holes {
        // Test a point just inside the hole's first edge, not a vertex,
        // which may lie on the outer loop.
        let (a, b) = (pool[h[0]], pool[h[1]]);
        let probe = flat((a + b) * 0.5);
        let best = regions
            .iter()
            .enumerate()
            .filter(|(_, (o, _))| contains(o, probe))
            .min_by(|x, y| area(&x.1.0).total_cmp(&area(&y.1.0)))
            .map(|(i, _)| i)
            .ok_or_else(|| BooleanError::InvalidResult("a hole lies in no face".into()))?;
        regions[best].1.push(h);
    }
    Ok(regions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(x: f64, y: f64, z: f64, dx: f64, dy: f64, dz: f64) -> Brep {
        Brep::make_box(Vec3::new(x, y, z), dx, dy, dz)
    }

    /// Valid, with the expected volume (to 1e-9) and genus.
    fn check(b: &Brep, volume: f64, genus: usize) {
        assert_eq!(b.validate(), Ok(()));
        assert!((b.volume() - volume).abs() < 1e-9, "volume {} expected {volume}", b.volume());
        assert_eq!(b.genus(), Some(genus));
    }

    #[test]
    fn overlapping_boxes() {
        // Overlap is 1 x 1 x 1 at (1..2)^3.
        let (a, b) = (cube(0.0, 0.0, 0.0, 2.0, 2.0, 2.0), cube(1.0, 1.0, 1.0, 2.0, 2.0, 2.0));
        check(&a.boolean(&b, BooleanOp::Union).unwrap(), 8.0 + 8.0 - 1.0, 0);
        check(&a.boolean(&b, BooleanOp::Difference).unwrap(), 8.0 - 1.0, 0);
        check(&a.boolean(&b, BooleanOp::Intersection).unwrap(), 1.0, 0);
        check(&b.boolean(&a, BooleanOp::Union).unwrap(), 15.0, 0);
    }

    #[test]
    fn boxes_sharing_a_face_merge_into_one_clean_box() {
        // Coplanar faces everywhere: the result must be a plain box, with the
        // shared face gone and no leftover vertices on straight edges.
        let u = cube(0.0, 0.0, 0.0, 2.0, 2.0, 2.0).boolean(&cube(2.0, 0.0, 0.0, 2.0, 2.0, 2.0), BooleanOp::Union).unwrap();
        check(&u, 16.0, 0);
        assert_eq!(u.counts(), (8, 12, 6));
    }

    #[test]
    fn pocket_flush_with_the_top_face() {
        // The cutter's top is coplanar with the block's top.
        let block = cube(0.0, 0.0, 0.0, 4.0, 4.0, 4.0);
        let pocket = block.boolean(&cube(1.0, 1.0, 2.0, 2.0, 2.0, 2.0), BooleanOp::Difference).unwrap();
        check(&pocket, 64.0 - 8.0, 0);
        // 8 block corners + 8 pocket corners; 6 + 5 faces, top face has a hole.
        assert_eq!(pocket.counts(), (16, 24, 11));
    }

    #[test]
    fn square_hole_through_a_block() {
        let block = cube(0.0, 0.0, 0.0, 4.0, 4.0, 4.0);
        let r = block.boolean(&cube(1.0, 1.0, -1.0, 2.0, 2.0, 6.0), BooleanOp::Difference).unwrap();
        check(&r, 64.0 - 16.0, 1);
        assert_eq!(r.counts(), (16, 24, 10), "same as extruding a square with a square hole");
    }

    #[test]
    fn slanted_planes() {
        // Hexagonal prism (slanted side planes) minus a box through it.
        let hex: Vec<Vec3> = (0..6)
            .map(|k| {
                let a = std::f64::consts::TAU * k as f64 / 6.0;
                Vec3::new(3.0 * a.cos(), 3.0 * a.sin(), 0.0)
            })
            .collect();
        let prism = Brep::extrude_polygon(&hex, 2.0);
        let hex_area = 3.0 * 3.0_f64.sqrt() / 2.0 * 9.0;
        let r = prism.boolean(&cube(-1.0, -1.0, -1.0, 2.0, 2.0, 4.0), BooleanOp::Difference).unwrap();
        check(&r, hex_area * 2.0 - 8.0, 1);
        // The box clipped by the prism is the box's middle part.
        let i = prism.boolean(&cube(-1.0, -1.0, -1.0, 2.0, 2.0, 4.0), BooleanOp::Intersection).unwrap();
        check(&i, 8.0, 0);
    }

    #[test]
    fn hollow_and_split_results_have_several_shells() {
        let block = cube(0.0, 0.0, 0.0, 4.0, 4.0, 4.0);
        // A box strictly inside: a hollow, as a second shell facing inwards.
        let hollow = block.boolean(&cube(1.0, 1.0, 1.0, 2.0, 2.0, 2.0), BooleanOp::Difference).unwrap();
        assert_eq!(hollow.shells.len(), 2);
        check(&hollow, 64.0 - 8.0, 0);
        // A wall cut right through a bar: two pieces.
        let bar = cube(0.0, 0.0, 0.0, 4.0, 1.0, 1.0);
        let split = bar.boolean(&cube(1.5, -1.0, -1.0, 1.0, 3.0, 3.0), BooleanOp::Difference).unwrap();
        assert_eq!(split.shells.len(), 2);
        check(&split, 3.0, 0);
    }

    #[test]
    fn disjoint_solids() {
        let (a, b) = (cube(0.0, 0.0, 0.0, 1.0, 1.0, 1.0), cube(5.0, 0.0, 0.0, 1.0, 1.0, 1.0));
        assert!(a.boolean(&b, BooleanOp::Intersection).unwrap().faces.is_empty());
        let u = a.boolean(&b, BooleanOp::Union).unwrap();
        assert_eq!(u.shells.len(), 2);
        check(&u, 2.0, 0);
        check(&a.boolean(&b, BooleanOp::Difference).unwrap(), 1.0, 0);
    }

    #[test]
    fn identical_solids() {
        let a = cube(0.0, 0.0, 0.0, 2.0, 3.0, 4.0);
        let u = a.boolean(&a, BooleanOp::Union).unwrap();
        check(&u, 24.0, 0);
        assert_eq!(u.counts(), (8, 12, 6));
        check(&a.boolean(&a, BooleanOp::Intersection).unwrap(), 24.0, 0);
        assert!(a.boolean(&a, BooleanOp::Difference).unwrap().faces.is_empty());
    }

    #[test]
    fn chained_operations_stay_exact() {
        // A plate drilled three times with square holes, then a boss added on
        // top: each step feeds the next.
        let mut part = cube(0.0, 0.0, 0.0, 10.0, 4.0, 1.0);
        for x in [1.0, 4.5, 8.0] {
            part = part.boolean(&cube(x, 1.5, -1.0, 1.0, 1.0, 3.0), BooleanOp::Difference).unwrap();
        }
        check(&part, 40.0 - 3.0, 3);
        let part = part.boolean(&cube(2.5, 0.5, 1.0, 1.5, 3.0, 2.0), BooleanOp::Union).unwrap();
        check(&part, 37.0 + 9.0, 3);
    }

    #[test]
    fn rotated_overlap() {
        // A diamond (square turned 45°) prism through a box: slanted cuts
        // that meet the box's faces along non-axis lines.
        let s = 2.0_f64.sqrt();
        let diamond = [Vec3::new(0.0, -s, -1.0), Vec3::new(s, 0.0, -1.0), Vec3::new(0.0, s, -1.0), Vec3::new(-s, 0.0, -1.0)];
        let prism = Brep::extrude_polygon(&diamond, 4.0); // area 4, z from -1 to 3
        let block = cube(-1.0, -1.0, 0.0, 2.0, 2.0, 2.0);
        // In plan, the 2 x 2 square's corners stick out past the diamond's
        // sides |x| + |y| = √2: 4 triangles with legs 2 − √2, area (√2 − 1)² each.
        let poke = (s - 1.0) * (s - 1.0);
        check(&block.boolean(&prism, BooleanOp::Intersection).unwrap(), (4.0 - 4.0 * poke) * 2.0, 0);
        check(&block.boolean(&prism, BooleanOp::Union).unwrap(), 8.0 + 4.0 * 4.0 - (4.0 - 4.0 * poke) * 2.0, 0);
    }

    #[test]
    fn boxes_touching_only_along_an_edge_fail_cleanly() {
        // Their union would share one edge between four faces: not a
        // manifold solid, so it is reported, not returned.
        let (a, b) = (cube(0.0, 0.0, 0.0, 1.0, 1.0, 1.0), cube(1.0, 1.0, 0.0, 1.0, 1.0, 1.0));
        assert!(matches!(a.boolean(&b, BooleanOp::Union), Err(BooleanError::InvalidResult(_))));
    }

    #[test]
    fn curved_inputs_are_refused() {
        let c = Brep::make_cylinder(Vec3::new(0.0, 0.0, 0.0), 1.0, 1.0);
        let b = cube(0.0, 0.0, 0.0, 1.0, 1.0, 1.0);
        assert_eq!(b.boolean(&c, BooleanOp::Union).unwrap_err(), BooleanError::NotPlanar);
    }
}
