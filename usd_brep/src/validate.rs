//! Structural validation of BrepArray data.
//!
//! Failures use the same `BA_xxx` codes as NVIDIA's `brep_validator.py`
//! (tools/brep_validator in the usd-brep repo), with the same tolerances.
//! A few extra geometric checks that the Python validator does not run are
//! reported as warnings with `RS_xxx` codes.
//!
//! Like the Python tool, this is a data-sanity layer: passing it does not
//! prove the geometry is a valid solid (that needs a kernel).

// `!(x > 0.0)` is deliberate: it also rejects NaN, which `x <= 0.0` would accept.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use crate::model::*;
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Failure,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = if self.severity == Severity::Failure { "FAIL" } else { "WARN" };
        write!(f, "{s} {}: {}", self.code, self.message)
    }
}

/// Validate one BrepArray. Authorship and declared-type checks run only for
/// BrepArrays read from a file (those with a prim path).
pub fn validate(b: &BrepArray) -> Vec<Issue> {
    let mut v = Checker { b, out: Vec::new(), from_file: !b.path.is_empty(), lens: b.attr_lens() };
    v.data_types();
    v.authorship();
    v.brep_level();
    v.topology_sizes();
    v.tokens();
    v.face_values();
    v.index_ranges();
    v.topology_rules();
    v.nurbs_curves(&b.edge_nurbs, CurveSet::Edge);
    v.nurbs_curves(&b.wire_edge_nurbs, CurveSet::WireEdge);
    v.uv_curves();
    v.nurbs_surfaces();
    v.analytic();
    v.points();
    v.required_apis();
    v.schema_consistency();
    v.geometric_extras();
    v.out
}

/// True when no failures (warnings allowed).
pub fn is_valid(issues: &[Issue]) -> bool {
    issues.iter().all(|i| i.severity != Severity::Failure)
}

// ───────────────────────────── Helpers ─────────────────────────────

const NUMERICAL_TOLERANCE: f64 = 1e-11;
const UNIT_TOL: f64 = 1e-4;

fn isclose(a: f64, b: f64) -> bool {
    (a - b).abs() <= (1e-5 * a.abs().max(b.abs())).max(1e-6)
}
fn less(a: f64, b: f64) -> bool {
    a < b && !isclose(a, b)
}
fn greater(a: f64, b: f64) -> bool {
    a > b && !isclose(a, b)
}
fn len3(v: V3) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}
fn dot3(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn sub3(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross3(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

impl BrepArray {
    pub(crate) fn attr_lens(&self) -> BTreeMap<&'static str, usize> {
        let mut m = BTreeMap::new();
        self.visit_lens(&mut |name, len| {
            m.insert(name, len);
        });
        m
    }
}

#[derive(Clone, Copy, PartialEq)]
enum CurveSet {
    Edge,
    WireEdge,
}

struct Checker<'a> {
    b: &'a BrepArray,
    out: Vec<Issue>,
    from_file: bool,
    lens: BTreeMap<&'static str, usize>,
}

impl Checker<'_> {
    fn fail(&mut self, code: &'static str, message: impl Into<String>) {
        self.out.push(Issue { code, severity: Severity::Failure, message: message.into() });
    }
    fn warn(&mut self, code: &'static str, message: impl Into<String>) {
        self.out.push(Issue { code, severity: Severity::Warning, message: message.into() });
    }
    fn len(&self, name: &str) -> usize {
        self.lens.get(name).copied().unwrap_or(0)
    }
    fn authored(&self, name: &str) -> bool {
        self.b.authored.contains_key(name)
    }

    /// All attributes in `names` must be authored (file input only).
    fn require_authored(&mut self, code: &'static str, names: &[&str]) {
        if !self.from_file {
            return;
        }
        let missing: Vec<&str> = names.iter().copied().filter(|n| !self.authored(n)).collect();
        if !missing.is_empty() {
            self.fail(code, format!("required attribute(s) not authored: {}", missing.join(", ")));
        }
    }

    /// All attributes in `names` must have exactly `expected` entries.
    fn sizes(&mut self, code: &'static str, names: &[&str], expected: usize, why: &str) {
        for &n in names {
            let got = self.len(n);
            if got != expected && (self.authored(n) || !self.from_file || expected > 0) {
                self.fail(code, format!("{n} has {got} entries, expected {expected} ({why})"));
            }
        }
    }

    fn check_tokens<T: Token>(&mut self, code: &'static str, name: &str, values: &[T]) {
        for (i, t) in values.iter().enumerate() {
            if !T::allowed().contains(&t.as_token()) {
                self.fail(code, format!("{name}[{i}] = '{}' is not one of {:?}", t.as_token(), T::allowed()));
            }
        }
    }

    // ───────────── data types (BA_061 … BA_471) ─────────────

    fn data_types(&mut self) {
        fn type_code(name: &str) -> Option<&'static str> {
            Some(match name {
                n if n.starts_with("brep:vertexPoint:") => "BA_326",
                n if n.starts_with("brep:shellPoint:") => "BA_327",
                n if n.starts_with("brep:edge3dNurb:") => "BA_371",
                n if n.starts_with("brep:wireEdge3dNurb:") => "BA_291",
                n if n.starts_with("brep:curveUv:") => "BA_416",
                n if n.starts_with("brep:surface:nurb:") => "BA_471",
                "brep:intersectTol3d" | "brep:extent" | "brep:regionCount" => "BA_061",
                n if n.starts_with("region:") => "BA_076",
                n if n.starts_with("shell:") => "BA_091",
                n if n.starts_with("faceuse:") => "BA_116",
                n if n.starts_with("face:") => "BA_161",
                n if n.starts_with("loop:") => "BA_176",
                n if n.starts_with("edgeuse:") => "BA_196",
                n if n.starts_with("edge:") => "BA_237",
                n if n.starts_with("wireEdge:") => "BA_291",
                n if n.starts_with("vertex:") => "BA_316",
                _ => return None,
            })
        }
        let authored: Vec<(String, String)> = self.b.authored.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (name, declared) in authored {
            let Some(spec) = attr_spec(&name) else { continue };
            if let Some(code) = type_code(&name)
                && declared != spec.usd_type {
                    self.fail(code, format!("{name} is declared {declared}, schema type is {}", spec.usd_type));
                }
        }
        let issues = self.b.read_issues.clone();
        for msg in issues {
            let name = msg.split(':').take_while(|s| !s.starts_with(' ')).collect::<Vec<_>>().join(":");
            let code = type_code(&name).unwrap_or("BA_061");
            self.fail(code, format!("unreadable value: {msg}"));
        }
    }

    // ───────────── authorship (BA_005 … BA_300) ─────────────

    fn authorship(&mut self) {
        self.require_authored("BA_005", &["brep:intersectTol3d", "brep:extent", "brep:regionCount"]);
        self.require_authored("BA_070", &["region:shellCount", "region:type"]);
        self.require_authored("BA_085", &["shell:faceuseCount", "shell:wireEdgeCount", "shell:pointType"]);
        self.require_authored("BA_105", &["faceuse:faceIndex", "faceuse:orientationType"]);
        self.require_authored("BA_125", &["face:loopCount", "face:trimType", "face:surfaceType", "face:range"]);
        self.require_authored("BA_170", &["loop:edgeuseCount", "loop:vertexIndex"]);
        self.require_authored(
            "BA_185",
            &["edgeuse:edgeIndex", "edgeuse:orientationType", "edgeuse:nextRadialEUIndex", "edgeuse:thisRadialEntryType"],
        );
        self.require_authored("BA_215", &["edge:curveType", "edge:range", "edge:vertexIndices"]);
        self.require_authored("BA_300", &["vertex:pointType"]);
        // Wire-edge attributes are all-or-none, and required when shells own wire edges.
        if self.from_file {
            let names = ["wireEdge:curveType", "wireEdge:range", "wireEdge:vertexIndices"];
            let count = names.iter().filter(|n| self.authored(n)).count();
            let w: u32 = self.b.shell_wire_edge_count.iter().sum();
            if (count > 0 && count < 3) || (w > 0 && count < 3) {
                self.fail("BA_255", "wireEdge:curveType, wireEdge:range and wireEdge:vertexIndices must be authored together");
            }
        }
    }

    // ───────────── brep level (BA_000 … BA_050) ─────────────

    fn brep_level(&mut self) {
        let b = self.b;
        let n = b.brep_count();
        if b.intersect_tol_3d.len() != n {
            self.fail("BA_000", format!("brep:intersectTol3d has {} entries for {n} Breps", b.intersect_tol_3d.len()));
        }
        for (i, &t) in b.intersect_tol_3d.iter().enumerate() {
            if !(t >= NUMERICAL_TOLERANCE) {
                self.fail("BA_010", format!("brep:intersectTol3d[{i}] = {t} must be positive"));
            }
        }
        if b.extent.len() != 2 * n {
            self.fail("BA_020", format!("brep:extent has {} points, expected {} (2 per Brep)", b.extent.len(), 2 * n));
            return;
        }
        let prim = (b.prim_extent.len() >= 2).then(|| (b.prim_extent[0], b.prim_extent[1]));
        for i in 0..n {
            let (lo, hi) = (b.extent[2 * i], b.extent[2 * i + 1]);
            for (axis, code_order, code_in) in [(0, "BA_025", "BA_040"), (1, "BA_030", "BA_045"), (2, "BA_035", "BA_050")] {
                if lo[axis] > hi[axis] {
                    self.fail(code_order, format!("brep:extent of Brep {i}: min {} > max {} on axis {axis}", lo[axis], hi[axis]));
                }
                if let Some((plo, phi)) = prim
                    && (less(lo[axis], plo[axis]) || greater(hi[axis], phi[axis])) {
                        self.fail(code_in, format!("brep:extent of Brep {i} lies outside the prim extent on axis {axis}"));
                    }
            }
        }
    }

    // ───────────── topology sizes (BA_065 … BA_295) ─────────────

    fn topology_sizes(&mut self) {
        let b = self.b;
        let sum = |v: &[u32]| v.iter().map(|&x| x as usize).sum::<usize>();
        self.sizes("BA_065", &["region:shellCount", "region:type"], sum(&b.region_count), "sum of brep:regionCount");
        self.sizes(
            "BA_080",
            &["shell:faceuseCount", "shell:wireEdgeCount", "shell:pointType"],
            sum(&b.region_shell_count),
            "sum of region:shellCount",
        );
        self.sizes("BA_100", &["faceuse:faceIndex", "faceuse:orientationType"], sum(&b.shell_faceuse_count), "sum of shell:faceuseCount");
        let faces = b.faceuse_face_index.len() / 2;
        self.sizes("BA_120", &["face:loopCount", "face:trimType", "face:surfaceType"], faces, "faceuses / 2");
        if b.face_range.len() != 2 * faces {
            self.fail("BA_150", format!("face:range has {} entries, expected {} (2 per face)", b.face_range.len(), 2 * faces));
        }
        self.sizes("BA_165", &["loop:edgeuseCount", "loop:vertexIndex"], sum(&b.face_loop_count), "sum of face:loopCount");
        self.sizes(
            "BA_180",
            &["edgeuse:edgeIndex", "edgeuse:orientationType", "edgeuse:nextRadialEUIndex", "edgeuse:thisRadialEntryType"],
            sum(&b.loop_edgeuse_count),
            "sum of loop:edgeuseCount",
        );
        let edges = b.edge_curve_type.len();
        if b.edge_vertex_indices.len() != edges {
            self.fail("BA_210", format!("edge:vertexIndices has {} entries but edge:curveType has {edges}", b.edge_vertex_indices.len()));
        }
        if b.edge_range.len() != 2 * edges {
            self.fail("BA_230", format!("edge:range has {} values, expected {} (2 per edge)", b.edge_range.len(), 2 * edges));
        }
        let wires = sum(&b.shell_wire_edge_count);
        self.sizes("BA_250", &["wireEdge:curveType", "wireEdge:vertexIndices"], wires, "sum of shell:wireEdgeCount");
        if b.wire_edge_range.len() != 2 * b.wire_edge_curve_type.len() {
            self.fail("BA_270", format!("wireEdge:range has {} values, expected 2 per wire edge", b.wire_edge_range.len()));
        }
        let max_vertex = b
            .edge_vertex_indices
            .iter()
            .chain(&b.wire_edge_vertex_indices)
            .flat_map(|p| p.iter().copied())
            .max()
            .map_or(0, |m| (m + 1).max(0) as usize);
        if b.vertex_point_type.len() < max_vertex {
            self.fail("BA_295", format!("vertex:pointType has {} entries but vertex index {} is used", b.vertex_point_type.len(), max_vertex - 1));
        }
    }

    // ───────────── tokens ─────────────

    fn tokens(&mut self) {
        let b = self.b;
        self.check_tokens("BA_075", "region:type", &b.region_type);
        self.check_tokens("BA_090", "shell:pointType", &b.shell_point_type);
        self.check_tokens("BA_110", "faceuse:orientationType", &b.faceuse_orientation);
        self.check_tokens("BA_130", "face:surfaceType", &b.face_surface_type);
        self.check_tokens("BA_135", "face:trimType", &b.face_trim_type);
        self.check_tokens("BA_190", "edgeuse:orientationType", &b.edgeuse_orientation);
        self.check_tokens("BA_195", "edgeuse:thisRadialEntryType", &b.edgeuse_entry);
        self.check_tokens("BA_245", "edge:curveType", &b.edge_curve_type);
        self.check_tokens("BA_260", "wireEdge:curveType", &b.wire_edge_curve_type);
        self.check_tokens("BA_315", "vertex:pointType", &b.vertex_point_type);
    }

    // ───────────── face values ─────────────

    fn face_values(&mut self) {
        let b = self.b;
        for (i, &c) in b.face_loop_count.iter().enumerate() {
            if c < 1 {
                self.fail("BA_140", format!("face:loopCount[{i}] = {c}; every face needs an outer loop"));
            }
        }
        for (f, pair) in b.face_range.chunks(2).enumerate() {
            if pair.len() < 2 {
                break;
            }
            let (lo, hi) = (pair[0], pair[1]);
            if lo.iter().chain(&hi).any(|x| !x.is_finite()) {
                self.fail("BA_145", format!("face:range of face {f} is not finite: {lo:?} {hi:?}"));
                continue;
            }
            if hi[0] - lo[0] <= NUMERICAL_TOLERANCE {
                self.fail("BA_155", format!("face {f}: U range [{}, {}] is degenerate", lo[0], hi[0]));
            }
            if hi[1] - lo[1] <= NUMERICAL_TOLERANCE {
                self.fail("BA_160", format!("face {f}: V range [{}, {}] is degenerate", lo[1], hi[1]));
            }
        }
        for (code, name, ranges) in [("BA_235", "edge:range", &b.edge_range), ("BA_275", "wireEdge:range", &b.wire_edge_range)] {
            for (e, r) in ranges.chunks(2).enumerate() {
                if r.len() == 2 && !(r[0] <= r[1]) {
                    self.fail(code, format!("{name} of item {e}: [{}, {}] is not ordered", r[0], r[1]));
                }
            }
        }
    }

    // ───────────── per-Brep index ranges ─────────────

    fn index_ranges(&mut self) {
        let b = self.b;
        for (bi, s) in b.spans().iter().enumerate() {
            for fu in s.faceuses.clone() {
                if let Some(&f) = b.faceuse_face_index.get(fu)
                    && !s.faces.contains(&(f as usize)) {
                        self.fail("BA_115", format!("faceuse {fu} of Brep {bi} references face {f}, outside {:?}", s.faces));
                    }
            }
            for l in s.loops.clone() {
                if b.loop_edgeuse_count.get(l) == Some(&0)
                    && let Some(&v) = b.loop_vertex_index.get(l)
                        && !s.vertices.contains(&(v as usize)) {
                            self.fail("BA_175", format!("vertex loop {l} of Brep {bi} references vertex {v}, outside {:?}", s.vertices));
                        }
            }
            for eu in s.edgeuses.clone() {
                if let Some(&e) = b.edgeuse_edge_index.get(eu)
                    && (!s.edges.contains(&(e as usize)) || e as usize >= b.edge_curve_type.len()) {
                        self.fail("BA_205", format!("edgeuse {eu} of Brep {bi} references edge {e}, outside {:?}", s.edges));
                    }
                if let Some(&n) = b.edgeuse_next_radial.get(eu)
                    && !s.edgeuses.contains(&(n as usize)) {
                        self.fail("BA_200", format!("edgeuse {eu} of Brep {bi}: nextRadialEUIndex {n} is outside {:?}", s.edgeuses));
                    }
            }
            for e in s.edges.clone() {
                if let Some(p) = b.edge_vertex_indices.get(e) {
                    for &v in p {
                        if v < 0 || !s.vertices.contains(&(v as usize)) {
                            self.fail("BA_225", format!("edge {e} of Brep {bi} references vertex {v}, outside {:?}", s.vertices));
                        }
                    }
                }
            }
            for w in s.wire_edges.clone() {
                if let Some(p) = b.wire_edge_vertex_indices.get(w) {
                    for &v in p {
                        if v < 0 || v as usize >= b.vertex_point_type.len() {
                            self.fail("BA_265", format!("wire edge {w} references vertex {v}, which does not exist"));
                        }
                    }
                }
            }
        }
    }

    // ───────────── topology rules (BA_580 … BA_582) ─────────────

    fn topology_rules(&mut self) {
        let b = self.b;
        let faces = b.face_loop_count.len();
        let mut uses = vec![0usize; faces];
        for &f in &b.faceuse_face_index {
            if let Some(u) = uses.get_mut(f as usize) {
                *u += 1;
            }
        }
        for (f, &u) in uses.iter().enumerate() {
            if u != 2 {
                self.fail("BA_580", format!("face {f} is referenced by {u} faceuses; every face needs exactly 2"));
            }
        }
        // Radial closure: following next links from an edgeuse must come back to it,
        // visiting only edgeuses of the same edge.
        let n = b.edgeuse_next_radial.len().min(b.edgeuse_edge_index.len());
        for start in 0..n {
            let edge = b.edgeuse_edge_index[start];
            let mut cur = start;
            let mut ok = false;
            for _ in 0..=n {
                let Some(&next) = b.edgeuse_next_radial.get(cur) else { break };
                let next = next as usize;
                if next >= n || b.edgeuse_edge_index[next] != edge {
                    break;
                }
                if next == start {
                    ok = true;
                    break;
                }
                cur = next;
            }
            if !ok {
                self.fail("BA_581", format!("radial cycle from edgeuse {start} (edge {edge}) does not close"));
            }
        }
        let mut referenced = vec![false; b.edge_curve_type.len()];
        for &e in &b.edgeuse_edge_index {
            if let Some(r) = referenced.get_mut(e as usize) {
                *r = true;
            }
        }
        for (e, r) in referenced.iter().enumerate() {
            if !r {
                self.fail("BA_582", format!("edge {e} is not used by any edgeuse"));
            }
        }
    }

    // ───────────── 3D NURBS curves ─────────────

    fn nurbs_curves(&mut self, c: &NurbsCurves3d, set: CurveSet) {
        let b = self.b;
        let (types, prefix) = match set {
            CurveSet::Edge => (&b.edge_curve_type, "brep:edge3dNurb:curve3d:nurb:"),
            CurveSet::WireEdge => (&b.wire_edge_curve_type, "brep:wireEdge3dNurb:curve3d:nurb:"),
        };
        let count = types.iter().filter(|t| **t == CurveType::Nurbs).count();
        let codes = match set {
            //           sizes    order>0  ord<=n   cv/w     w>0      knots    sorted   extent   complete
            CurveSet::Edge => ["BA_330", "BA_335", "BA_340", "BA_345", "BA_350", "BA_355", "BA_360", "BA_365", "BA_370"],
            CurveSet::WireEdge => ["BA_650", "BA_651", "BA_652", "BA_653", "BA_654", "BA_655", "BA_656", "BA_657", "BA_658"],
        };
        if count == 0 && c.order.is_empty() && c.vertex_count.is_empty() {
            return;
        }
        if count > 0 {
            let names: Vec<String> =
                ["controlVertices", "vertexCount", "order", "knots", "weights"].iter().map(|s| format!("{prefix}{s}")).collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            self.require_authored(codes[8], &refs);
        }
        if c.order.len() != count || c.vertex_count.len() != count {
            self.fail(codes[0], format!("{prefix}order/vertexCount have {}/{} entries for {count} NURBS curves", c.order.len(), c.vertex_count.len()));
            return;
        }
        let total_cv: usize = c.vertex_count.iter().map(|&v| v as usize).sum();
        if c.control_vertices.len() != total_cv || c.weights.len() != total_cv {
            self.fail(
                codes[3],
                format!("{prefix}controlVertices/weights have {}/{} entries, expected {total_cv}", c.control_vertices.len(), c.weights.len()),
            );
        }
        for (i, &w) in c.weights.iter().enumerate() {
            if !(w > 0.0) {
                self.fail(codes[4], format!("{prefix}weights[{i}] = {w} must be positive"));
            }
        }
        let total_knots: usize = c.vertex_count.iter().zip(&c.order).map(|(&n, &o)| (n + o) as usize).sum();
        if c.knots.len() != total_knots {
            self.fail(codes[5], format!("{prefix}knots has {} values, expected {total_knots} (vertexCount + order per curve)", c.knots.len()));
        }
        let mut k0 = 0;
        for i in 0..count {
            let (o, n) = (c.order[i], c.vertex_count[i]);
            if o == 0 {
                self.fail(codes[1], format!("{prefix}order[{i}] must be positive"));
            } else if o > n {
                self.fail(codes[2], format!("{prefix}order[{i}] = {o} exceeds vertexCount {n}"));
            }
            let k1 = k0 + (n + o) as usize;
            if let Some(ks) = c.knots.get(k0..k1)
                && ks.windows(2).any(|w| w[1] < w[0]) {
                    self.fail(codes[6], format!("{prefix}knots of curve {i} are not non-decreasing"));
                }
            k0 = k1;
        }
        if let Some((lo, hi)) = self.union_extent() {
            for (i, p) in c.control_vertices.iter().enumerate() {
                if (0..3).any(|a| less(p[a], lo[a]) || greater(p[a], hi[a])) {
                    self.fail(codes[7], format!("{prefix}controlVertices[{i}] {p:?} lies outside brep:extent"));
                    break;
                }
            }
        }
    }

    fn union_extent(&self) -> Option<(V3, V3)> {
        let e = &self.b.extent;
        if e.len() < 2 || !e.len().is_multiple_of(2) {
            return None;
        }
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for pair in e.chunks(2) {
            for a in 0..3 {
                lo[a] = lo[a].min(pair[0][a]);
                hi[a] = hi[a].max(pair[1][a]);
            }
        }
        Some((lo, hi))
    }

    // ───────────── UV trim curves ─────────────

    fn uv_curves(&mut self) {
        let b = self.b;
        let c = &b.uv_curves;
        let authored = self.authored("brep:curveUv:nurb:order") || self.authored("brep:curveUv:nurb:vertexCount");
        if !authored && c.order.is_empty() && c.vertex_count.is_empty() {
            return;
        }
        let neu = b.edgeuse_edge_index.len();
        if c.order.len() != neu || c.vertex_count.len() != neu {
            self.fail("BA_375", format!("brep:curveUv:nurb order/vertexCount have {}/{} entries for {neu} edgeuses", c.order.len(), c.vertex_count.len()));
        }
        // Keep checking the curves that are present, as the Python validator does.
        let ncurves = c.order.len().min(c.vertex_count.len());
        let total_cv: usize = c.vertex_count.iter().map(|&v| v as usize).sum();
        if c.control_vertices.len() != total_cv {
            self.fail("BA_390", format!("brep:curveUv:nurb:controlVertices has {}, expected {total_cv}", c.control_vertices.len()));
        }
        if c.weights.len() != total_cv {
            self.fail("BA_405", format!("brep:curveUv:nurb:weights has {}, expected {total_cv}", c.weights.len()));
        }
        for (i, &w) in c.weights.iter().enumerate() {
            if !(w > 0.0) {
                self.fail("BA_410", format!("brep:curveUv:nurb:weights[{i}] = {w} must be positive"));
            }
        }
        let total_knots: usize =
            c.vertex_count.iter().zip(&c.order).filter(|(n, o)| !(**n == 0 && **o == 0)).map(|(&n, &o)| (n + o) as usize).sum();
        if c.knots.len() != total_knots {
            self.fail("BA_395", format!("brep:curveUv:nurb:knots has {} values, expected {total_knots}", c.knots.len()));
        }
        let consistent = c.control_vertices.len() == total_cv && c.weights.len() == total_cv && c.knots.len() == total_knots;
        let (mut cv0, mut k0) = (0usize, 0usize);
        let mut ends: Vec<Option<(V2, V2)>> = Vec::with_capacity(ncurves);
        for i in 0..ncurves {
            let (o, n) = (c.order[i], c.vertex_count[i]);
            if o == 0 && n == 0 {
                ends.push(None);
                continue;
            }
            if o == 0 {
                self.fail("BA_380", format!("UV curve {i}: order must be positive (or order and vertexCount both 0)"));
            } else if o > n {
                self.fail("BA_385", format!("UV curve {i}: order {o} exceeds vertexCount {n}"));
            }
            let (cv1, k1) = (cv0 + n as usize, k0 + (n + o) as usize);
            let mut end = None;
            if let Some(ks) = c.knots.get(k0..k1) {
                if ks.windows(2).any(|w| w[1] < w[0]) {
                    self.fail("BA_400", format!("UV curve {i}: knots are not non-decreasing"));
                } else if consistent && o >= 1 && o <= n {
                    let cvs = &c.control_vertices[cv0..cv1];
                    let ws = &c.weights[cv0..cv1];
                    let (mut ulo, mut uhi, mut vlo, mut vhi) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
                    for p in cvs {
                        (ulo, uhi, vlo, vhi) = (ulo.min(p[0]), uhi.max(p[0]), vlo.min(p[1]), vhi.max(p[1]));
                    }
                    if ((uhi - ulo).powi(2) + (vhi - vlo).powi(2)).sqrt() <= 1e-12 {
                        self.fail("BA_764", format!("UV curve {i} has collapsed control vertices at ({ulo}, {vlo})"));
                    }
                    let p0 = nurbs_eval(o as usize, ks, cvs, ws, ks[o as usize - 1]);
                    let p1 = nurbs_eval(o as usize, ks, cvs, ws, ks[n as usize]);
                    if let (Some(a), Some(z)) = (p0, p1) {
                        end = Some((a, z));
                    }
                }
            }
            ends.push(end);
            cv0 = cv1;
            k0 = k1;
        }
        // UV loops must close: each curve ends where the next one starts.
        if ends.len() != neu {
            return;
        }
        let mut eu = 0usize;
        let mut lp = 0usize;
        for (f, &nl) in b.face_loop_count.iter().enumerate() {
            for _ in 0..nl {
                let Some(&ne) = b.loop_edgeuse_count.get(lp) else { return };
                lp += 1;
                let (s, e) = (eu, eu + ne as usize);
                eu = e;
                let Some(seg) = ends.get(s..e) else { return };
                if seg.is_empty() || seg.iter().any(Option::is_none) {
                    continue;
                }
                for k in 0..seg.len() {
                    let end = seg[k].unwrap().1;
                    let next = seg[(k + 1) % seg.len()].unwrap().0;
                    let gap = ((end[0] - next[0]).powi(2) + (end[1] - next[1]).powi(2)).sqrt();
                    if gap > 1e-6 {
                        self.fail("BA_763", format!("face {f}: UV curve of edgeuse {} ends {gap:.6} away from the next one's start", s + k));
                    }
                }
            }
        }
    }

    // ───────────── NURBS surfaces ─────────────

    fn nurbs_surfaces(&mut self) {
        let b = self.b;
        let s = &b.surface_nurbs;
        let count = b.face_surface_type.iter().filter(|t| **t == SurfaceType::Nurbs).count();
        if count == 0 && s.u_order.is_empty() {
            return;
        }
        if count > 0 {
            let names = [
                "brep:surface:nurb:controlVertices",
                "brep:surface:nurb:uKnots",
                "brep:surface:nurb:uOrder",
                "brep:surface:nurb:uVertexCount",
                "brep:surface:nurb:vKnots",
                "brep:surface:nurb:vOrder",
                "brep:surface:nurb:vVertexCount",
                "brep:surface:nurb:weights",
            ];
            self.require_authored("BA_470", &names);
        }
        if [s.u_order.len(), s.v_order.len(), s.u_vertex_count.len(), s.v_vertex_count.len()].iter().any(|&l| l != count) {
            self.fail("BA_420", format!("brep:surface:nurb order/vertexCount arrays must each have {count} entries (one per NURBS face)"));
            return;
        }
        let total_cv: usize = s.u_vertex_count.iter().zip(&s.v_vertex_count).map(|(&u, &v)| (u * v) as usize).sum();
        if s.control_vertices.len() != total_cv || s.weights.len() != total_cv {
            self.fail("BA_435", format!("surface controlVertices/weights have {}/{}, expected {total_cv}", s.control_vertices.len(), s.weights.len()));
        }
        for (i, &w) in s.weights.iter().enumerate() {
            if !(w > 0.0) {
                self.fail("BA_440", format!("brep:surface:nurb:weights[{i}] = {w} must be positive"));
                break;
            }
        }
        let tu: usize = s.u_vertex_count.iter().zip(&s.u_order).map(|(&n, &o)| (n + o) as usize).sum();
        let tv: usize = s.v_vertex_count.iter().zip(&s.v_order).map(|(&n, &o)| (n + o) as usize).sum();
        if s.u_knots.len() != tu {
            self.fail("BA_445", format!("brep:surface:nurb:uKnots has {}, expected {tu}", s.u_knots.len()));
        }
        if s.v_knots.len() != tv {
            self.fail("BA_450", format!("brep:surface:nurb:vKnots has {}, expected {tv}", s.v_knots.len()));
        }
        let (mut ku, mut kv) = (0, 0);
        for i in 0..count {
            for (o, n, code_pos, code_le) in [
                (s.u_order[i], s.u_vertex_count[i], "BA_425", "BA_430"),
                (s.v_order[i], s.v_vertex_count[i], "BA_425", "BA_430"),
            ] {
                if o == 0 {
                    self.fail(code_pos, format!("NURBS surface {i}: order must be positive"));
                } else if o > n {
                    self.fail(code_le, format!("NURBS surface {i}: order {o} exceeds vertex count {n}"));
                }
            }
            let ku1 = ku + (s.u_vertex_count[i] + s.u_order[i]) as usize;
            let kv1 = kv + (s.v_vertex_count[i] + s.v_order[i]) as usize;
            if s.u_knots.get(ku..ku1).is_some_and(|k| k.windows(2).any(|w| w[1] < w[0])) {
                self.fail("BA_455", format!("NURBS surface {i}: uKnots are not non-decreasing"));
            }
            if s.v_knots.get(kv..kv1).is_some_and(|k| k.windows(2).any(|w| w[1] < w[0])) {
                self.fail("BA_460", format!("NURBS surface {i}: vKnots are not non-decreasing"));
            }
            (ku, kv) = (ku1, kv1);
        }
        if let Some((lo, hi)) = self.union_extent() {
            for (i, p) in s.control_vertices.iter().enumerate() {
                if (0..3).any(|a| less(p[a], lo[a]) || greater(p[a], hi[a])) {
                    self.fail("BA_465", format!("brep:surface:nurb:controlVertices[{i}] {p:?} lies outside brep:extent"));
                    break;
                }
            }
        }
    }

    // ───────────── analytic surfaces and curves ─────────────

    fn frame(&mut self, codes: [&'static str; 3], what: &str, axis: &[V3], refd: &[V3]) {
        for (i, (a, r)) in axis.iter().zip(refd).enumerate() {
            if (len3(*a) - 1.0).abs() > UNIT_TOL {
                self.fail(codes[0], format!("{what} {i}: axis is not unit length ({})", len3(*a)));
            }
            if (len3(*r) - 1.0).abs() > UNIT_TOL {
                self.fail(codes[1], format!("{what} {i}: refDirection is not unit length ({})", len3(*r)));
            }
            if dot3(*a, *r).abs() > UNIT_TOL {
                self.fail(codes[2], format!("{what} {i}: axis and refDirection are not orthogonal"));
            }
        }
    }

    fn analytic(&mut self) {
        let b = self.b;
        let count = |t: SurfaceType| b.face_surface_type.iter().filter(|x| **x == t).count();
        let positive = |v: &mut Self, code, what: &str, xs: &[f64], allow_zero: bool| {
            for (i, &x) in xs.iter().enumerate() {
                if !(x > 0.0 || (allow_zero && x == 0.0)) {
                    v.fail(code, format!("{what} {i}: value {x} must be {}", if allow_zero { "non-negative" } else { "positive" }));
                }
            }
        };

        let n = count(SurfaceType::Plane);
        let p = &b.planes;
        if [p.origin.len(), p.axis.len(), p.ref_direction.len()].iter().any(|&l| l != n) {
            self.fail("BA_490", format!("plane arrays must each have {n} entries"));
        }
        self.frame(["BA_491", "BA_492", "BA_493"], "plane", &p.axis, &p.ref_direction);

        let n = count(SurfaceType::Sphere);
        let s = &b.spheres;
        if [s.center.len(), s.axis.len(), s.ref_direction.len(), s.radius.len()].iter().any(|&l| l != n) {
            self.fail("BA_480", format!("sphere arrays must each have {n} entries"));
        }
        positive(self, "BA_481", "sphere", &s.radius, false);
        self.frame(["BA_482", "BA_483", "BA_484"], "sphere", &s.axis, &s.ref_direction);

        let n = count(SurfaceType::Cylinder);
        let c = &b.cylinders;
        if [c.origin.len(), c.axis.len(), c.ref_direction.len(), c.radius.len()].iter().any(|&l| l != n) {
            self.fail("BA_500", format!("cylinder arrays must each have {n} entries"));
        }
        positive(self, "BA_501", "cylinder", &c.radius, false);
        self.frame(["BA_502", "BA_503", "BA_504"], "cylinder", &c.axis, &c.ref_direction);

        let n = count(SurfaceType::Cone);
        let c = &b.cones;
        if [c.origin.len(), c.axis.len(), c.ref_direction.len(), c.radius.len(), c.semi_angle.len()].iter().any(|&l| l != n) {
            self.fail("BA_510", format!("cone arrays must each have {n} entries"));
        }
        positive(self, "BA_511", "cone", &c.radius, true);
        self.frame(["BA_512", "BA_513", "BA_514"], "cone", &c.axis, &c.ref_direction);
        for (i, &a) in c.semi_angle.iter().enumerate() {
            if !(a > 0.0 && a < std::f64::consts::FRAC_PI_2) {
                self.fail("BA_515", format!("cone {i}: semiAngle {a} must be in (0, pi/2)"));
            }
        }

        let n = count(SurfaceType::Torus);
        let t = &b.tori;
        if [t.origin.len(), t.axis.len(), t.ref_direction.len(), t.major_radius.len(), t.minor_radius.len()].iter().any(|&l| l != n) {
            self.fail("BA_520", format!("torus arrays must each have {n} entries"));
        }
        positive(self, "BA_521", "torus major radius", &t.major_radius, false);
        positive(self, "BA_522", "torus minor radius", &t.minor_radius, false);
        self.frame(["BA_523", "BA_524", "BA_525"], "torus", &t.axis, &t.ref_direction);

        for (types, lines, circles, ellipses, what) in [
            (&b.edge_curve_type, &b.edge_lines, &b.edge_circles, &b.edge_ellipses, "edge"),
            (&b.wire_edge_curve_type, &b.wire_edge_lines, &b.wire_edge_circles, &b.wire_edge_ellipses, "wire edge"),
        ] {
            let cnt = |t: CurveType| types.iter().filter(|x| **x == t).count();
            let n = cnt(CurveType::Line);
            if lines.origin.len() != n || lines.direction.len() != n {
                self.fail("BA_540", format!("{what} line arrays must each have {n} entries"));
            }
            for (i, d) in lines.direction.iter().enumerate() {
                if (len3(*d) - 1.0).abs() > UNIT_TOL {
                    self.fail("BA_541", format!("{what} line {i}: direction is not unit length"));
                }
            }
            let n = cnt(CurveType::Circle);
            if [circles.center.len(), circles.axis.len(), circles.ref_direction.len(), circles.radius.len()].iter().any(|&l| l != n) {
                self.fail("BA_530", format!("{what} circle arrays must each have {n} entries"));
            }
            positive(self, "BA_531", "circle", &circles.radius, false);
            self.frame(["BA_532", "BA_533", "BA_534"], "circle", &circles.axis, &circles.ref_direction);
            let n = cnt(CurveType::Ellipse);
            let e = ellipses;
            if [e.center.len(), e.axis.len(), e.ref_direction.len(), e.x_radius.len(), e.y_radius.len()].iter().any(|&l| l != n) {
                self.fail("BA_550", format!("{what} ellipse arrays must each have {n} entries"));
            }
            positive(self, "BA_551", "ellipse xRadius", &e.x_radius, false);
            positive(self, "BA_552", "ellipse yRadius", &e.y_radius, false);
            self.frame(["BA_553", "BA_554", "BA_555"], "ellipse", &e.axis, &e.ref_direction);
        }
    }

    // ───────────── point geometry ─────────────

    fn points(&mut self) {
        let b = self.b;
        let nv = b.vertex_point_type.iter().filter(|t| **t == VertexPointType::BrepPoint).count();
        if nv > 0 {
            self.require_authored("BA_305", &["brep:vertexPoint:point:position"]);
        }
        if b.vertex_points.len() != nv && (nv > 0 || !b.vertex_points.is_empty()) {
            self.fail("BA_320", format!("brep:vertexPoint:point:position has {} points for {nv} BrepPointAPI vertices", b.vertex_points.len()));
        }
        let point_shells = (0..b.shell_point_type.len())
            .filter(|&i| {
                b.shell_point_type[i] == ShellPointType::BrepPoint
                    && b.shell_faceuse_count.get(i) == Some(&0)
                    && b.shell_wire_edge_count.get(i) == Some(&0)
            })
            .count();
        if b.shell_points.len() != point_shells {
            self.fail("BA_325", format!("brep:shellPoint:point:position has {} points for {point_shells} point shells", b.shell_points.len()));
        }
        // Vertices must lie inside their Brep's extent.
        let spans = b.spans();
        for (bi, s) in spans.iter().enumerate() {
            let (Some(lo), Some(hi)) = (b.extent.get(2 * bi), b.extent.get(2 * bi + 1)) else { continue };
            for v in s.vertices.clone() {
                if let Some(p) = b.vertex_points.get(v)
                    && (0..3).any(|a| less(p[a], lo[a]) || greater(p[a], hi[a])) {
                        self.fail("BA_310", format!("vertex {v} at {p:?} lies outside the extent of Brep {bi}"));
                    }
            }
        }
    }

    // ───────────── applied API schemas (BA_583) ─────────────

    fn required_apis(&mut self) {
        if !self.from_file {
            return; // the writer adds required APIs automatically
        }
        let mut need = self.b.clone();
        need.api_schemas.clear();
        for api in need.api_schemas_for_write() {
            if !self.b.api_schemas.contains(&api) {
                self.fail("BA_583", format!("data requires the applied API schema '{api}', which is missing"));
            }
        }
    }

    // ───────────── applied API vs data usage (BA_290 … BA_526) ─────────────

    fn schema_consistency(&mut self) {
        if !self.from_file {
            return;
        }
        let b = self.b;
        let has = |api: &str| b.api_schemas.iter().any(|a| a == api);
        let edge_n = |t: CurveType| b.edge_curve_type.iter().filter(|x| **x == t).count();
        let wire_n = |t: CurveType| b.wire_edge_curve_type.iter().filter(|x| **x == t).count();
        let face_n = |t: SurfaceType| b.face_surface_type.iter().filter(|x| **x == t).count();
        // (code, API, entities using it, has data, what)
        let rules: [(&'static str, &str, usize, bool, &str); 10] = [
            ("BA_305", "BrepPointAPI:vertexPoint", b.vertex_point_type.iter().filter(|t| **t == VertexPointType::BrepPoint).count(), !b.vertex_points.is_empty(), "vertices"),
            ("BA_370", "BrepCurve3dNurbAPI:edge3dNurb", edge_n(CurveType::Nurbs), !b.edge_nurbs.order.is_empty(), "edges"),
            ("BA_290", "BrepCurve3dNurbAPI:wireEdge3dNurb", wire_n(CurveType::Nurbs), !b.wire_edge_nurbs.order.is_empty(), "wire edges"),
            ("BA_415", "BrepCurveUvNurbAPI", b.edgeuse_edge_index.len(), !b.uv_curves.order.is_empty(), "edgeuses"),
            ("BA_470", "BrepSurfaceNurbAPI", face_n(SurfaceType::Nurbs), !b.surface_nurbs.u_order.is_empty(), "faces"),
            ("BA_485", "BrepSurfaceSphereAPI", face_n(SurfaceType::Sphere), !b.spheres.center.is_empty(), "faces"),
            ("BA_495", "BrepSurfacePlaneAPI", face_n(SurfaceType::Plane), !b.planes.origin.is_empty(), "faces"),
            ("BA_505", "BrepSurfaceCylinderAPI", face_n(SurfaceType::Cylinder), !b.cylinders.origin.is_empty(), "faces"),
            ("BA_516", "BrepSurfaceConeAPI", face_n(SurfaceType::Cone), !b.cones.origin.is_empty(), "faces"),
            ("BA_526", "BrepSurfaceTorusAPI", face_n(SurfaceType::Torus), !b.tori.origin.is_empty(), "faces"),
        ];
        for (code, api, users, has_data, what) in rules {
            // UV curves are optional: only an applied API without data is an error.
            let data_required = users > 0 && (code != "BA_415" || has(api));
            if data_required && !has_data {
                self.fail(code, format!("{users} {what} need {api} data, but none is authored"));
            }
            if has(api) && users == 0 {
                self.fail(code, format!("{api} is applied, but no {what} use it"));
            }
        }
    }

    // ───────────── extra geometric checks (warnings) ─────────────

    fn geometric_extras(&mut self) {
        let b = self.b;
        // RS_001: loops are head-to-tail chains of edgeuses (via edge vertices).
        let mut eu = 0usize;
        for (l, &n) in b.loop_edgeuse_count.iter().enumerate() {
            let ids = eu..eu + n as usize;
            eu = ids.end;
            let ends: Option<Vec<(i64, i64)>> = ids
                .clone()
                .map(|i| {
                    let e = *b.edgeuse_edge_index.get(i)? as usize;
                    let [s, t] = *b.edge_vertex_indices.get(e)?;
                    Some(if b.edgeuse_orientation.get(i)? == &Orientation::Opposite { (t, s) } else { (s, t) })
                })
                .collect();
            let Some(ends) = ends else { continue };
            for k in 0..ends.len() {
                if ends[k].1 != ends[(k + 1) % ends.len()].0 {
                    self.warn("RS_001", format!("loop {l}: edgeuse {} does not end where the next one starts", ids.start + k));
                    break;
                }
            }
        }
        // RS_002: vertex positions agree with the ends of line, circle and NURBS edge curves.
        let tol = b.intersect_tol_3d.iter().cloned().fold(0.0, f64::max).max(1e-7) * 10.0;
        let slots = b.edge_curve_slots();
        let mut nurbs_offsets = Vec::new();
        let (mut cv, mut kn) = (0usize, 0usize);
        for (o, n) in b.edge_nurbs.order.iter().zip(&b.edge_nurbs.vertex_count) {
            nurbs_offsets.push((cv, kn));
            cv += *n as usize;
            kn += (*n + *o) as usize;
        }
        for (e, (ty, slot)) in slots.iter().enumerate() {
            let (Some(&[vs, ve]), Some(r)) = (b.edge_vertex_indices.get(e), b.edge_range.get(2 * e..2 * e + 2)) else { continue };
            let (Some(&ps), Some(&pe)) = (b.vertex_points.get(vs as usize), b.vertex_points.get(ve as usize)) else { continue };
            let ends = match ty {
                CurveType::Line => match (b.edge_lines.origin.get(*slot), b.edge_lines.direction.get(*slot)) {
                    (Some(o), Some(d)) => Some((
                        [o[0] + r[0] * d[0], o[1] + r[0] * d[1], o[2] + r[0] * d[2]],
                        [o[0] + r[1] * d[0], o[1] + r[1] * d[1], o[2] + r[1] * d[2]],
                    )),
                    _ => None,
                },
                CurveType::Circle => {
                    let c = &b.edge_circles;
                    match (c.center.get(*slot), c.axis.get(*slot), c.ref_direction.get(*slot), c.radius.get(*slot)) {
                        (Some(o), Some(a), Some(x), Some(&rad)) => {
                            // C(t) = center + radius (cos t · ref + sin t · (axis × ref))
                            let y = cross3(*a, *x);
                            let at = |t: f64| [0, 1, 2].map(|k| o[k] + rad * (t.cos() * x[k] + t.sin() * y[k]));
                            Some((at(r[0]), at(r[1])))
                        }
                        _ => None,
                    }
                }
                CurveType::Nurbs => {
                    let c = &b.edge_nurbs;
                    nurbs_offsets.get(*slot).and_then(|&(cv, kn)| {
                        let (o, n) = (*c.order.get(*slot)? as usize, *c.vertex_count.get(*slot)? as usize);
                        let cvs = c.control_vertices.get(cv..cv + n)?;
                        let ws = c.weights.get(cv..cv + n)?;
                        let ks = c.knots.get(kn..kn + n + o)?;
                        Some((nurbs_eval(o, ks, cvs, ws, r[0])?, nurbs_eval(o, ks, cvs, ws, r[1])?))
                    })
                }
                _ => None,
            };
            if let Some((a, z)) = ends
                && (len3(sub3(a, ps)) > tol || len3(sub3(z, pe)) > tol) {
                    self.warn("RS_002", format!("edge {e}: curve ends do not meet its vertices {vs} and {ve}"));
                }
        }
    }
}

/// Rational de Boor evaluation of a NURBS curve of any dimension.
/// `knots` has `cvs.len() + order` entries. Returns None for invalid input.
#[allow(clippy::needless_range_loop)] // index-based de Boor reads clearer
pub fn nurbs_eval<const D: usize>(order: usize, knots: &[f64], cvs: &[[f64; D]], weights: &[f64], t: f64) -> Option<[f64; D]> {
    let n = cvs.len();
    if order == 0 || n < order || knots.len() != n + order || weights.len() != n {
        return None;
    }
    let p = order - 1;
    // Find span k with knots[k] <= t < knots[k+1], clamped to the valid range.
    let mut k = p;
    while k + 1 < n && knots[k + 1] <= t {
        k += 1;
    }
    let mut d: Vec<[f64; D]> = Vec::with_capacity(order);
    let mut w: Vec<f64> = Vec::with_capacity(order);
    for j in 0..order {
        let i = k + j - p;
        let wi = weights[i];
        let mut q = [0.0; D];
        for a in 0..D {
            q[a] = cvs[i][a] * wi;
        }
        d.push(q);
        w.push(wi);
    }
    for r in 1..=p {
        for j in (r..=p).rev() {
            let i = k + j - p;
            let denom = knots[i + order - r] - knots[i];
            let alpha = if denom.abs() < 1e-30 { 0.0 } else { (t - knots[i]) / denom };
            for a in 0..D {
                d[j][a] = (1.0 - alpha) * d[j - 1][a] + alpha * d[j][a];
            }
            w[j] = (1.0 - alpha) * w[j - 1] + alpha * w[j];
        }
    }
    if w[p].abs() < 1e-30 {
        return None;
    }
    let mut out = [0.0; D];
    for a in 0..D {
        out[a] = d[p][a] / w[p];
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn de_boor_matches_known_values() {
        // Quadratic Bezier from (0,0) via (1,2) to (2,0): midpoint is (1, 1).
        let cvs = [[0.0, 0.0], [1.0, 2.0], [2.0, 0.0]];
        let knots = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let p = nurbs_eval(3, &knots, &cvs, &[1.0; 3], 0.5).unwrap();
        assert!((p[0] - 1.0).abs() < 1e-12 && (p[1] - 1.0).abs() < 1e-12);
        assert_eq!(nurbs_eval(3, &knots, &cvs, &[1.0; 3], 1.0).unwrap(), [2.0, 0.0]);
        // Rational quarter circle: weights 1, √2/2, 1 → point at 45° has radius 1.
        let cvs = [[1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let w = [1.0, std::f64::consts::FRAC_1_SQRT_2, 1.0];
        let p = nurbs_eval(3, &knots, &cvs, &w, 0.5).unwrap();
        assert!(((p[0] * p[0] + p[1] * p[1]).sqrt() - 1.0).abs() < 1e-12);
    }
}
