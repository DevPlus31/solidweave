//! Data that flows along wires.
//!
//! Like Grasshopper, every port carries a *list* of values (`Data`), never a
//! single value. A slider outputs a list of one; a Series outputs many.

use crate::kernel::Solid;
use mini_brep::Vec3;
use std::fmt;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Number,
    Point,
    Polyline,
    Solid,
    Text,
}

#[derive(Debug, Clone)]
pub enum Value {
    Number(f64),
    Point(Vec3),
    /// Closed planar profile.
    Polyline(Rc<Vec<Vec3>>),
    /// Rc so large geometry is shared between caches, not copied. Opaque, so
    /// any kernel's solids can flow through the graph.
    Solid(Rc<dyn Solid>),
    /// e.g. an exported .usda layer.
    Text(Rc<String>),
}

pub type Data = Vec<Value>;

impl Value {
    pub fn kind(&self) -> Kind {
        match self {
            Value::Number(_) => Kind::Number,
            Value::Point(_) => Kind::Point,
            Value::Polyline(_) => Kind::Polyline,
            Value::Solid(_) => Kind::Solid,
            Value::Text(_) => Kind::Text,
        }
    }
    pub fn as_number(&self) -> Result<f64, String> {
        match self {
            Value::Number(n) => Ok(*n),
            v => Err(format!("expected Number, got {:?}", v.kind())),
        }
    }
    pub fn as_polyline(&self) -> Result<&[Vec3], String> {
        match self {
            Value::Polyline(p) => Ok(p),
            v => Err(format!("expected Polyline, got {:?}", v.kind())),
        }
    }
    pub fn as_solid(&self) -> Result<&dyn Solid, String> {
        match self {
            Value::Solid(b) => Ok(b.as_ref()),
            v => Err(format!("expected Solid, got {:?}", v.kind())),
        }
    }
    pub fn as_text(&self) -> Result<&str, String> {
        match self {
            Value::Text(t) => Ok(t),
            v => Err(format!("expected Text, got {:?}", v.kind())),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Value::Number(n) => write!(f, "{n}"),
            Value::Point(p) => write!(f, "({}, {}, {})", p.x, p.y, p.z),
            Value::Polyline(p) => write!(f, "Polyline[{} pts]", p.len()),
            Value::Solid(b) => {
                let (v, e, fc) = b.counts();
                write!(f, "Solid[V{v} E{e} F{fc}]")
            }
            Value::Text(t) => write!(f, "Text[{} lines]", t.lines().count()),
        }
    }
}
