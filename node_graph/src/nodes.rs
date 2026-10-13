//! Node types ("components" in Grasshopper terms).
//!
//! A node only describes how to compute ONE set of inputs. The graph engine
//! handles lists: it calls `solve` once per item using longest-list matching.
//! The `description` strings double as documentation for an AI agent.
//! Geometry comes only from the `Kernel` the graph passes in.

use crate::kernel::{BooleanOp, Kernel};
use crate::value::{Data, Kind, Value};
use mini_brep::{Profile, Ring, Vec3};
use std::rc::Rc;

#[derive(Debug, Clone)]
pub struct Port {
    pub name: &'static str,
    pub kind: Kind,
    /// Used when nothing is wired to this input.
    pub default: Option<Value>,
}

const fn port(name: &'static str, kind: Kind) -> Port {
    Port { name, kind, default: None }
}
fn num(name: &'static str, default: f64) -> Port {
    Port { name, kind: Kind::Number, default: Some(Value::Number(default)) }
}

pub trait NodeType {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn inputs(&self) -> Vec<Port>;
    fn outputs(&self) -> Vec<Port>;

    /// Item-level solve: one value per input → one value per output.
    fn solve(&self, kernel: &dyn Kernel, inputs: &[Value]) -> Result<Vec<Value>, String>;

    /// For parameter nodes (sliders). Returns false if the node has no value.
    fn set_number(&mut self, _value: f64) -> bool {
        false
    }
    /// Internal state shown in the text form, e.g. "value=3, min=0, max=10".
    fn state(&self) -> Option<String> {
        None
    }
    /// For parameter nodes: current value and allowed range, so a UI can
    /// show a slider.
    fn number_setting(&self) -> Option<(f64, std::ops::RangeInclusive<f64>)> {
        None
    }

    /// List-level solve. Override for nodes that consume or produce whole
    /// lists (e.g. Series). Default: longest-list matching over `solve`.
    fn solve_lists(&self, kernel: &dyn Kernel, inputs: &[Data]) -> Result<Vec<Data>, String> {
        let n_out = self.outputs().len();
        let mut out: Vec<Data> = vec![Vec::new(); n_out];
        if inputs.iter().any(|d| d.is_empty()) {
            return Ok(out); // empty input → empty output, like Grasshopper
        }
        let len = inputs.iter().map(Vec::len).max().unwrap_or(1);
        for i in 0..len {
            // Longest list: shorter lists repeat their last item.
            let args: Vec<Value> =
                inputs.iter().map(|d| d[i.min(d.len() - 1)].clone()).collect();
            for (o, v) in out.iter_mut().zip(self.solve(kernel, &args)?) {
                o.push(v);
            }
        }
        Ok(out)
    }
}

// ───────────────────────── Parameters ─────────────────────────

/// A number slider. Its value lives in the node (set via Graph::set_number).
pub struct NumberSlider {
    pub value: f64,
    pub min: f64,
    pub max: f64,
}
impl NodeType for NumberSlider {
    fn name(&self) -> &'static str { "Slider" }
    fn description(&self) -> &'static str { "A user-adjustable number within [min, max]." }
    fn inputs(&self) -> Vec<Port> { vec![] }
    fn outputs(&self) -> Vec<Port> { vec![port("N", Kind::Number)] }
    fn solve(&self, _: &dyn Kernel, _: &[Value]) -> Result<Vec<Value>, String> {
        Ok(vec![Value::Number(self.value.clamp(self.min, self.max))])
    }
    fn set_number(&mut self, value: f64) -> bool {
        self.value = value;
        true
    }
    fn state(&self) -> Option<String> {
        Some(format!("value={}, range={}..{}", self.value, self.min, self.max))
    }
    fn number_setting(&self) -> Option<(f64, std::ops::RangeInclusive<f64>)> {
        Some((self.value, self.min..=self.max))
    }
}

// ─────────────────────────── Sets ─────────────────────────────

/// start, start+step, … (count items). Produces a list from single values.
pub struct Series;
impl NodeType for Series {
    fn name(&self) -> &'static str { "Series" }
    fn description(&self) -> &'static str { "Arithmetic sequence of `count` numbers from `start` by `step`." }
    fn inputs(&self) -> Vec<Port> { vec![num("start", 0.0), num("step", 1.0), num("count", 10.0)] }
    fn outputs(&self) -> Vec<Port> { vec![port("S", Kind::Number)] }
    fn solve(&self, _: &dyn Kernel, _: &[Value]) -> Result<Vec<Value>, String> { unreachable!() }
    fn solve_lists(&self, _: &dyn Kernel, inputs: &[Data]) -> Result<Vec<Data>, String> {
        let first = |i: usize| inputs[i].first().ok_or("empty input").and_then(|v| v.as_number().map_err(|_| "not a number"));
        let (start, step, count) = (first(0)?, first(1)?, first(2)?);
        let count = count.max(0.0).round() as usize;
        Ok(vec![(0..count).map(|i| Value::Number(start + step * i as f64)).collect()])
    }
}

/// Sum of all numbers in a list (list → single item).
pub struct MassAddition;
impl NodeType for MassAddition {
    fn name(&self) -> &'static str { "MassAdd" }
    fn description(&self) -> &'static str { "Sum of every number in the input list." }
    fn inputs(&self) -> Vec<Port> { vec![port("N", Kind::Number)] }
    fn outputs(&self) -> Vec<Port> { vec![port("Sum", Kind::Number)] }
    fn solve(&self, _: &dyn Kernel, _: &[Value]) -> Result<Vec<Value>, String> { unreachable!() }
    fn solve_lists(&self, _: &dyn Kernel, inputs: &[Data]) -> Result<Vec<Data>, String> {
        let s: Result<f64, _> = inputs[0].iter().map(Value::as_number).sum();
        Ok(vec![vec![Value::Number(s?)]])
    }
}

// ───────────────────────── Geometry ───────────────────────────

fn profile(outer: Ring) -> Value {
    Value::Profile(Rc::new(Profile::new(outer)))
}

pub struct Rectangle;
impl NodeType for Rectangle {
    fn name(&self) -> &'static str { "Rectangle" }
    fn description(&self) -> &'static str { "Rectangle profile of width x by height y, at the origin on the XY plane." }
    fn inputs(&self) -> Vec<Port> { vec![num("x", 10.0), num("y", 5.0)] }
    fn outputs(&self) -> Vec<Port> { vec![port("R", Kind::Profile)] }
    fn solve(&self, _: &dyn Kernel, i: &[Value]) -> Result<Vec<Value>, String> {
        let (x, y) = (i[0].as_number()?, i[1].as_number()?);
        if x <= 0.0 || y <= 0.0 {
            return Err(format!("rectangle size must be positive, got {x} x {y}"));
        }
        let p = |a, b| Vec3::new(a, b, 0.0);
        Ok(vec![profile(Ring::Polygon(vec![p(0.0, 0.0), p(x, 0.0), p(x, y), p(0.0, y)]))])
    }
}

pub struct Polygon;
impl NodeType for Polygon {
    fn name(&self) -> &'static str { "Polygon" }
    fn description(&self) -> &'static str { "Regular polygon with `sides` corners and circumradius `radius`, centred at the origin." }
    fn inputs(&self) -> Vec<Port> { vec![num("radius", 5.0), num("sides", 6.0)] }
    fn outputs(&self) -> Vec<Port> { vec![port("P", Kind::Profile)] }
    fn solve(&self, _: &dyn Kernel, i: &[Value]) -> Result<Vec<Value>, String> {
        let r = i[0].as_number()?;
        let n = i[1].as_number()?.round() as usize;
        if n < 3 || r <= 0.0 {
            return Err(format!("polygon needs sides >= 3 and radius > 0, got {n}, {r}"));
        }
        let pts = (0..n)
            .map(|k| {
                let a = std::f64::consts::TAU * k as f64 / n as f64;
                Vec3::new(r * a.cos(), r * a.sin(), 0.0)
            })
            .collect();
        Ok(vec![profile(Ring::Polygon(pts))])
    }
}

pub struct Circle;
impl NodeType for Circle {
    fn name(&self) -> &'static str { "Circle" }
    fn description(&self) -> &'static str { "Circle profile of `radius` centred at (x, y) on the XY plane." }
    fn inputs(&self) -> Vec<Port> { vec![num("x", 0.0), num("y", 0.0), num("radius", 1.0)] }
    fn outputs(&self) -> Vec<Port> { vec![port("C", Kind::Profile)] }
    fn solve(&self, _: &dyn Kernel, i: &[Value]) -> Result<Vec<Value>, String> {
        let (x, y, radius) = (i[0].as_number()?, i[1].as_number()?, i[2].as_number()?);
        if radius <= 0.0 {
            return Err(format!("radius must be positive, got {radius}"));
        }
        Ok(vec![profile(Ring::Circle { center: Vec3::new(x, y, 0.0), radius })])
    }
}

pub struct Move;
impl NodeType for Move {
    fn name(&self) -> &'static str { "Move" }
    fn description(&self) -> &'static str { "Moves a profile by (x, y). Feed a list into x or y to make a row of copies." }
    fn inputs(&self) -> Vec<Port> { vec![port("profile", Kind::Profile), num("x", 0.0), num("y", 0.0)] }
    fn outputs(&self) -> Vec<Port> { vec![port("P", Kind::Profile)] }
    fn solve(&self, _: &dyn Kernel, i: &[Value]) -> Result<Vec<Value>, String> {
        let d = Vec3::new(i[1].as_number()?, i[2].as_number()?, 0.0);
        Ok(vec![Value::Profile(Rc::new(i[0].as_profile()?.translated(d)))])
    }
}

/// Region difference: each outer profile minus every hole profile.
pub struct Difference;
impl NodeType for Difference {
    fn name(&self) -> &'static str { "Difference" }
    fn description(&self) -> &'static str {
        "Cuts every profile in `holes` out of each `outer` profile, as through-holes. Holes must lie strictly inside the outline and apart from each other."
    }
    fn inputs(&self) -> Vec<Port> { vec![port("outer", Kind::Profile), port("holes", Kind::Profile)] }
    fn outputs(&self) -> Vec<Port> { vec![port("P", Kind::Profile)] }
    fn solve(&self, _: &dyn Kernel, _: &[Value]) -> Result<Vec<Value>, String> { unreachable!() }
    fn solve_lists(&self, _: &dyn Kernel, inputs: &[Data]) -> Result<Vec<Data>, String> {
        // The whole holes list applies to every outline, so this is not
        // item-by-item matching. An empty holes list cuts nothing.
        let holes = inputs[1]
            .iter()
            .map(|v| {
                let p = v.as_profile()?;
                if !p.holes.is_empty() {
                    return Err("a hole cannot have holes of its own".to_string());
                }
                Ok(p.outer.clone())
            })
            .collect::<Result<Vec<Ring>, String>>()?;
        let out = inputs[0]
            .iter()
            .map(|v| {
                let mut p = v.as_profile()?.clone();
                p.holes.extend(holes.iter().cloned());
                p.validate().map_err(|e| e.to_string())?;
                Ok(Value::Profile(Rc::new(p)))
            })
            .collect::<Result<Data, String>>()?;
        Ok(vec![out])
    }
}

pub struct Extrude;
impl NodeType for Extrude {
    fn name(&self) -> &'static str { "Extrude" }
    fn description(&self) -> &'static str { "Extrude a profile straight up by `height` into a B-rep solid. Its holes become through-holes." }
    fn inputs(&self) -> Vec<Port> { vec![port("profile", Kind::Profile), num("height", 1.0)] }
    fn outputs(&self) -> Vec<Port> { vec![port("S", Kind::Solid)] }
    fn solve(&self, kernel: &dyn Kernel, i: &[Value]) -> Result<Vec<Value>, String> {
        let profile = i[0].as_profile()?;
        let h = i[1].as_number()?;
        if h <= 0.0 {
            return Err(format!("height must be positive, got {h}"));
        }
        Ok(vec![Value::Solid(kernel.extrude(profile, h)?)])
    }
}

pub struct Revolve;
impl NodeType for Revolve {
    fn name(&self) -> &'static str { "Revolve" }
    fn description(&self) -> &'static str {
        "Spins a profile a full turn round the Z axis: x is the distance from the axis, y the height. A polygon gives discs, cylinders and cones; a circle clear of the axis gives a torus."
    }
    fn inputs(&self) -> Vec<Port> { vec![port("profile", Kind::Profile)] }
    fn outputs(&self) -> Vec<Port> { vec![port("S", Kind::Solid)] }
    fn solve(&self, kernel: &dyn Kernel, i: &[Value]) -> Result<Vec<Value>, String> {
        Ok(vec![Value::Solid(kernel.revolve(i[0].as_profile()?)?)])
    }
}

pub struct Sphere;
impl NodeType for Sphere {
    fn name(&self) -> &'static str { "Sphere" }
    fn description(&self) -> &'static str { "Sphere of `radius` centred at (x, y, z), poles on the Z axis." }
    fn inputs(&self) -> Vec<Port> { vec![num("x", 0.0), num("y", 0.0), num("z", 0.0), num("radius", 1.0)] }
    fn outputs(&self) -> Vec<Port> { vec![port("S", Kind::Solid)] }
    fn solve(&self, kernel: &dyn Kernel, i: &[Value]) -> Result<Vec<Value>, String> {
        let center = Vec3::new(i[0].as_number()?, i[1].as_number()?, i[2].as_number()?);
        Ok(vec![Value::Solid(kernel.sphere(center, i[3].as_number()?)?)])
    }
}

/// Solid union, difference or intersection, item by item (longest list).
pub struct SolidBoolean(pub BooleanOp);
impl NodeType for SolidBoolean {
    fn name(&self) -> &'static str {
        match self.0 {
            BooleanOp::Union => "SolidUnion",
            BooleanOp::Difference => "SolidDifference",
            BooleanOp::Intersection => "SolidIntersection",
        }
    }
    fn description(&self) -> &'static str {
        match self.0 {
            BooleanOp::Union => "A and B joined into one solid. Planar faces only for now.",
            BooleanOp::Difference => "A with B cut away. Planar faces only for now.",
            BooleanOp::Intersection => "The part A and B have in common. Planar faces only for now.",
        }
    }
    fn inputs(&self) -> Vec<Port> { vec![port("A", Kind::Solid), port("B", Kind::Solid)] }
    fn outputs(&self) -> Vec<Port> { vec![port("S", Kind::Solid)] }
    fn solve(&self, kernel: &dyn Kernel, i: &[Value]) -> Result<Vec<Value>, String> {
        Ok(vec![Value::Solid(kernel.boolean(i[0].as_solid()?, i[1].as_solid()?, self.0)?)])
    }
}

pub struct Volume;
impl NodeType for Volume {
    fn name(&self) -> &'static str { "Volume" }
    fn description(&self) -> &'static str { "Volume and surface area of a solid." }
    fn inputs(&self) -> Vec<Port> { vec![port("solid", Kind::Solid)] }
    fn outputs(&self) -> Vec<Port> { vec![port("V", Kind::Number), port("A", Kind::Number)] }
    fn solve(&self, _: &dyn Kernel, i: &[Value]) -> Result<Vec<Value>, String> {
        let b = i[0].as_solid()?;
        Ok(vec![Value::Number(b.volume()), Value::Number(b.surface_area())])
    }
}

// ─────────────────────────── Export ───────────────────────────

/// Every solid in the input list → one `.usda` layer, one BrepArray prim per
/// solid (`Solid_0`, `Solid_1`, …). The node only produces text; the caller
/// decides where to write it, so evaluation stays free of side effects.
pub struct UsdExport;
impl NodeType for UsdExport {
    fn name(&self) -> &'static str { "UsdExport" }
    fn description(&self) -> &'static str {
        "All input solids as one OpenUSD .usda layer of BrepArray prims, checked with NVIDIA's validator rules."
    }
    fn inputs(&self) -> Vec<Port> { vec![port("solids", Kind::Solid)] }
    fn outputs(&self) -> Vec<Port> { vec![port("usda", Kind::Text)] }
    fn solve(&self, _: &dyn Kernel, _: &[Value]) -> Result<Vec<Value>, String> { unreachable!() }
    fn solve_lists(&self, _: &dyn Kernel, inputs: &[Data]) -> Result<Vec<Data>, String> {
        if inputs[0].is_empty() {
            return Ok(vec![Vec::new()]);
        }
        let breps = inputs[0]
            .iter()
            .enumerate()
            .map(|(i, v)| Ok(v.as_solid()?.to_brep_array(&format!("Solid_{i}"))))
            .collect::<Result<Vec<_>, String>>()?;
        let text = usd_brep::layer_to_usda(&breps);
        // Validate what was written, read back from the text, not the in-memory value.
        let back = usd_brep::BrepArray::read_all(&text).map_err(|e| format!("exported text does not parse: {e:?}"))?;
        let issues: Vec<String> = back
            .iter()
            .flat_map(|b| usd_brep::validate(b).into_iter().map(move |i| format!("{}: {i}", b.name)))
            .collect();
        if !issues.is_empty() {
            return Err(format!("export failed validation: {}", issues.join("; ")));
        }
        Ok(vec![vec![Value::Text(Rc::new(text))]])
    }
}

/// Every node type's name, in menu order.
pub const NODE_NAMES: [&str; 16] = [
    "Slider", "Series", "MassAdd", "Rectangle", "Polygon", "Circle", "Move", "Difference", "Extrude", "Revolve", "Sphere",
    "SolidUnion", "SolidDifference", "SolidIntersection", "Volume", "UsdExport",
];

/// Registry: a new node of the type called `name` (as `NodeType::name`
/// spells it). This is how a UI menu or an AI creates nodes.
pub fn create(name: &str) -> Option<Box<dyn NodeType>> {
    Some(match name {
        "Slider" => Box::new(NumberSlider { value: 1.0, min: 0.0, max: 10.0 }),
        "Series" => Box::new(Series),
        "MassAdd" => Box::new(MassAddition),
        "Rectangle" => Box::new(Rectangle),
        "Polygon" => Box::new(Polygon),
        "Circle" => Box::new(Circle),
        "Move" => Box::new(Move),
        "Difference" => Box::new(Difference),
        "Extrude" => Box::new(Extrude),
        "Revolve" => Box::new(Revolve),
        "Sphere" => Box::new(Sphere),
        "SolidUnion" => Box::new(SolidBoolean(BooleanOp::Union)),
        "SolidDifference" => Box::new(SolidBoolean(BooleanOp::Difference)),
        "SolidIntersection" => Box::new(SolidBoolean(BooleanOp::Intersection)),
        "Volume" => Box::new(Volume),
        "UsdExport" => Box::new(UsdExport),
        _ => return None,
    })
}

/// One node of every type. This list is what an AI would be shown.
pub fn library() -> Vec<Box<dyn NodeType>> {
    NODE_NAMES.iter().filter_map(|n| create(n)).collect()
}
