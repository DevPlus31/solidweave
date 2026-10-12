//! A small, dependency-free reader for the USDA text format.
//!
//! It parses the full layer structure (prims, nested prims, variant sets,
//! metadata, relationships, dictionaries) so that real-world files load, but
//! it only keeps what BrepArray work needs: prim types, names, paths, applied
//! API schemas, and attribute declarations with their literal values.
//! Anything else (time samples, dictionaries, connections) is skipped safely.

use std::fmt;

// ───────────────────────────── Literals ─────────────────────────────

/// A parsed USDA value.
#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Num(f64),
    Str(String),
    /// Bare word such as `true`, `None`, `inf`.
    Ident(String),
    Asset(String),
    Path(String),
    List(Vec<Lit>),
    Tuple(Vec<Lit>),
    /// A `{ ... }` block (dictionary, time samples). Contents are not kept.
    Dict,
}

impl Lit {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Lit::Num(n) => Some(*n),
            Lit::Ident(s) => match s.as_str() {
                "inf" => Some(f64::INFINITY),
                "-inf" => Some(f64::NEG_INFINITY),
                "nan" => Some(f64::NAN),
                _ => None,
            },
            _ => None,
        }
    }
}

// ───────────────────────────── Document ─────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct Layer {
    pub default_prim: Option<String>,
    pub prims: Vec<Prim>,
}

#[derive(Debug, Clone, Default)]
pub struct Prim {
    /// `def`, `over` or `class`.
    pub specifier: String,
    pub type_name: Option<String>,
    pub name: String,
    /// Absolute path, e.g. `/World/Cube`.
    pub path: String,
    /// Applied API schemas from `apiSchemas` metadata (all list ops merged).
    pub api_schemas: Vec<String>,
    pub attrs: Vec<Attr>,
    pub children: Vec<Prim>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attr {
    pub name: String,
    /// Declared value type, e.g. `uint[]`, `point3d[]`. `rel` for relationships.
    pub type_name: String,
    pub uniform: bool,
    pub value: Option<Lit>,
}

impl Prim {
    pub fn attr(&self, name: &str) -> Option<&Attr> {
        self.attrs.iter().find(|a| a.name == name)
    }

    /// Depth-first walk over this prim and all descendants.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a Prim>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }
}

impl Layer {
    /// All prims in the layer, depth first.
    pub fn all_prims(&self) -> Vec<&Prim> {
        let mut v = Vec::new();
        for p in &self.prims {
            p.walk(&mut v);
        }
        v
    }
}

// ───────────────────────────── Errors ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}
impl std::error::Error for ParseError {}

// ───────────────────────────── Tokenizer ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Num(f64),
    Str(String),
    Asset(String),
    Path(String),
    Punct(char),
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
    line: usize,
}

impl<'a> Lexer<'a> {
    fn err<T>(&self, msg: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError { line: self.line, message: msg.into() })
    }

    fn peek_byte(&self, k: usize) -> Option<u8> {
        self.s.get(self.i + k).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek_byte(0) {
            match c {
                b'\n' => {
                    self.line += 1;
                    self.i += 1;
                }
                b' ' | b'\t' | b'\r' => self.i += 1,
                b'#' => {
                    while let Some(c) = self.peek_byte(0) {
                        if c == b'\n' {
                            break;
                        }
                        self.i += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn next(&mut self) -> Result<Option<(Tok, usize)>, ParseError> {
        self.skip_ws();
        let line = self.line;
        let Some(c) = self.peek_byte(0) else { return Ok(None) };
        let tok = match c {
            b'"' | b'\'' => Tok::Str(self.string(c)?),
            b'@' => Tok::Asset(self.asset()?),
            b'<' => {
                let start = self.i + 1;
                while self.peek_byte(0).is_some_and(|c| c != b'>') {
                    self.i += 1;
                }
                if self.peek_byte(0).is_none() {
                    return self.err("unterminated path");
                }
                let p = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
                self.i += 1;
                Tok::Path(p)
            }
            b'0'..=b'9' => Tok::Num(self.number()?),
            b'-' | b'+' | b'.' => {
                let n1 = self.peek_byte(1);
                if n1.is_some_and(|d| d.is_ascii_digit() || d == b'.') {
                    Tok::Num(self.number()?)
                } else if c == b'-' && self.s[self.i + 1..].starts_with(b"inf") {
                    self.i += 4;
                    Tok::Ident("-inf".into())
                } else {
                    self.i += 1;
                    Tok::Punct(c as char)
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = self.i;
                while self
                    .peek_byte(0)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b':' || c == b'.')
                {
                    self.i += 1;
                }
                Tok::Ident(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
            }
            _ => {
                // Multi-byte UTF-8 outside strings is not valid USDA; treat as punctuation.
                let ch = std::str::from_utf8(&self.s[self.i..])
                    .ok()
                    .and_then(|s| s.chars().next())
                    .unwrap_or(c as char);
                self.i += ch.len_utf8();
                Tok::Punct(ch)
            }
        };
        Ok(Some((tok, line)))
    }

    fn number(&mut self) -> Result<f64, ParseError> {
        let start = self.i;
        if matches!(self.peek_byte(0), Some(b'-' | b'+')) {
            self.i += 1;
        }
        while let Some(c) = self.peek_byte(0) {
            let prev = self.s[self.i - 1];
            let ok = c.is_ascii_digit()
                || c == b'.'
                || c == b'e'
                || c == b'E'
                || ((c == b'-' || c == b'+') && (prev == b'e' || prev == b'E'));
            if !ok {
                break;
            }
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
        text.parse::<f64>().or_else(|_| self.err(format!("bad number '{text}'")))
    }

    fn string(&mut self, q: u8) -> Result<String, ParseError> {
        let triple = self.peek_byte(1) == Some(q) && self.peek_byte(2) == Some(q);
        self.i += if triple { 3 } else { 1 };
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek_byte(0) else { return self.err("unterminated string") };
            if c == b'\\'
                && let Some(n) = self.peek_byte(1) {
                    out.push(match n {
                        b'n' => b'\n',
                        b't' => b'\t',
                        other => other,
                    });
                    self.i += 2;
                    continue;
                }
            if c == q && (!triple || (self.peek_byte(1) == Some(q) && self.peek_byte(2) == Some(q))) {
                self.i += if triple { 3 } else { 1 };
                break;
            }
            if c == b'\n' {
                if !triple {
                    return self.err("newline in string");
                }
                self.line += 1;
            }
            out.push(c);
            self.i += 1;
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn asset(&mut self) -> Result<String, ParseError> {
        let triple = self.s[self.i..].starts_with(b"@@@");
        let delim: &[u8] = if triple { b"@@@" } else { b"@" };
        self.i += delim.len();
        let start = self.i;
        while self.i < self.s.len() && !self.s[self.i..].starts_with(delim) {
            self.i += 1;
        }
        if self.i >= self.s.len() {
            return self.err("unterminated asset path");
        }
        let a = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
        self.i += delim.len();
        Ok(a)
    }
}

// ───────────────────────────── Parser ─────────────────────────────

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
}

const LIST_OPS: [&str; 6] = ["prepend", "append", "add", "delete", "reorder", "explicit"];

impl Parser {
    fn line(&self) -> usize {
        self.toks.get(self.pos).or(self.toks.last()).map_or(0, |t| t.1)
    }
    fn err<T>(&self, msg: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError { line: self.line(), message: msg.into() })
    }
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.0)
    }
    fn peek_at(&self, k: usize) -> Option<&Tok> {
        self.toks.get(self.pos + k).map(|t| &t.0)
    }
    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|t| t.0.clone());
        self.pos += 1;
        t
    }
    fn is_punct(&self, c: char) -> bool {
        self.peek() == Some(&Tok::Punct(c))
    }
    fn eat_punct(&mut self, c: char) -> bool {
        if self.is_punct(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect_punct(&mut self, c: char) -> Result<(), ParseError> {
        if self.eat_punct(c) {
            Ok(())
        } else {
            self.err(format!("expected '{c}', found {:?}", self.peek()))
        }
    }
    fn ident(&mut self) -> Result<String, ParseError> {
        match self.bump() {
            Some(Tok::Ident(s)) => Ok(s),
            other => {
                self.pos -= 1;
                self.err(format!("expected identifier, found {other:?}"))
            }
        }
    }
    fn string(&mut self) -> Result<String, ParseError> {
        match self.bump() {
            Some(Tok::Str(s)) => Ok(s),
            other => {
                self.pos -= 1;
                self.err(format!("expected string, found {other:?}"))
            }
        }
    }

    /// Skip a balanced group starting at the current open bracket.
    fn skip_group(&mut self, open: char, close: char) -> Result<(), ParseError> {
        self.expect_punct(open)?;
        let mut depth = 1;
        while depth > 0 {
            match self.bump() {
                Some(Tok::Punct(c)) if c == open => depth += 1,
                Some(Tok::Punct(c)) if c == close => depth -= 1,
                Some(_) => {}
                None => return self.err(format!("unbalanced '{open}'")),
            }
        }
        Ok(())
    }

    fn value(&mut self) -> Result<Lit, ParseError> {
        match self.peek() {
            Some(Tok::Punct('[')) => {
                self.pos += 1;
                let mut items = Vec::new();
                while !self.eat_punct(']') {
                    items.push(self.value()?);
                    if !self.eat_punct(',') {
                        self.expect_punct(']')?;
                        break;
                    }
                }
                Ok(Lit::List(items))
            }
            Some(Tok::Punct('(')) => {
                self.pos += 1;
                let mut items = Vec::new();
                while !self.eat_punct(')') {
                    items.push(self.value()?);
                    if !self.eat_punct(',') {
                        self.expect_punct(')')?;
                        break;
                    }
                }
                Ok(Lit::Tuple(items))
            }
            Some(Tok::Punct('{')) => {
                self.skip_group('{', '}')?;
                Ok(Lit::Dict)
            }
            _ => match self.bump() {
                Some(Tok::Num(n)) => Ok(Lit::Num(n)),
                Some(Tok::Str(s)) => Ok(Lit::Str(s)),
                Some(Tok::Ident(s)) => Ok(Lit::Ident(s)),
                Some(Tok::Path(p)) => Ok(Lit::Path(p)),
                Some(Tok::Asset(a)) => {
                    if let Some(Tok::Path(_)) = self.peek() {
                        self.pos += 1; // reference target path
                    }
                    Ok(Lit::Asset(a))
                }
                other => {
                    self.pos -= 1;
                    self.err(format!("expected value, found {other:?}"))
                }
            },
        }
    }

    /// `( key = value ... )` metadata block. Returns (key, list-op, value) entries.
    fn metadata(&mut self) -> Result<Vec<(String, Lit)>, ParseError> {
        self.expect_punct('(')?;
        let mut out = Vec::new();
        loop {
            if self.eat_punct(')') {
                return Ok(out);
            }
            match self.peek() {
                Some(Tok::Str(_)) => {
                    self.pos += 1; // bare doc string
                }
                Some(Tok::Punct(';' | ',')) => self.pos += 1,
                Some(Tok::Ident(_)) => {
                    let mut key = self.ident()?;
                    if LIST_OPS.contains(&key.as_str()) && matches!(self.peek(), Some(Tok::Ident(_))) {
                        key = self.ident()?;
                    }
                    if self.eat_punct('=') {
                        out.push((key, self.value()?));
                    }
                }
                None => return self.err("unterminated metadata block"),
                other => return self.err(format!("unexpected {other:?} in metadata")),
            }
        }
    }

    fn prim(&mut self, parent_path: &str) -> Result<Prim, ParseError> {
        let specifier = self.ident()?;
        let type_name = match self.peek() {
            Some(Tok::Ident(_)) => Some(self.ident()?),
            _ => None,
        };
        let name = self.string()?;
        let path = if parent_path.is_empty() || parent_path == "/" {
            format!("/{name}")
        } else {
            format!("{parent_path}/{name}")
        };
        let mut prim = Prim { specifier, type_name, name, path, ..Default::default() };
        if self.is_punct('(') {
            for (key, val) in self.metadata()? {
                if key == "apiSchemas"
                    && let Lit::List(items) = val {
                        for it in items {
                            if let Lit::Str(s) = it
                                && !prim.api_schemas.contains(&s) {
                                    prim.api_schemas.push(s);
                                }
                        }
                    }
            }
        }
        self.expect_punct('{')?;
        let path = prim.path.clone();
        self.body(&mut prim, &path)?;
        Ok(prim)
    }

    /// Prim body up to and including the closing `}`.
    fn body(&mut self, prim: &mut Prim, path: &str) -> Result<(), ParseError> {
        loop {
            if self.eat_punct('}') {
                return Ok(());
            }
            match self.peek().cloned() {
                None => return self.err(format!("unterminated prim '{}'", prim.name)),
                Some(Tok::Punct(';')) => self.pos += 1,
                Some(Tok::Ident(w)) if matches!(w.as_str(), "def" | "over" | "class") => {
                    let child = self.prim(path)?;
                    prim.children.push(child);
                }
                Some(Tok::Ident(w)) if w == "variantSet" => {
                    self.pos += 1;
                    self.string()?;
                    self.expect_punct('=')?;
                    self.expect_punct('{')?;
                    while !self.eat_punct('}') {
                        self.string()?;
                        if self.is_punct('(') {
                            self.metadata()?;
                        }
                        self.expect_punct('{')?;
                        // Variant contents are merged into the owning prim.
                        self.body(prim, path)?;
                    }
                }
                Some(Tok::Ident(w)) if w == "reorder" => {
                    self.pos += 1;
                    self.ident()?;
                    self.expect_punct('=')?;
                    self.value()?;
                }
                Some(Tok::Ident(_)) => {
                    if let Some(a) = self.property()? {
                        prim.attrs.retain(|x| x.name != a.name);
                        prim.attrs.push(a);
                    }
                }
                other => return self.err(format!("unexpected {other:?} in prim body")),
            }
        }
    }

    fn property(&mut self) -> Result<Option<Attr>, ParseError> {
        let mut word = self.ident()?;
        if LIST_OPS.contains(&word.as_str()) {
            word = self.ident()?;
        }
        let mut uniform = false;
        loop {
            match word.as_str() {
                "custom" | "varying" | "config" => word = self.ident()?,
                "uniform" => {
                    uniform = true;
                    word = self.ident()?;
                }
                _ => break,
            }
        }
        let mut type_name = word;
        if type_name != "rel" && self.is_punct('[') && self.peek_at(1) == Some(&Tok::Punct(']')) {
            self.pos += 2;
            type_name.push_str("[]");
        }
        let name = self.ident()?;
        let value = if self.eat_punct('=') { Some(self.value()?) } else { None };
        if self.is_punct('(') {
            self.metadata()?;
        }
        // `foo.connect = </x>` and `foo.timeSamples = {..}` are not plain values.
        if let Some(base) = name.strip_suffix(".connect").or_else(|| name.strip_suffix(".timeSamples")) {
            let _ = base;
            return Ok(None);
        }
        Ok(Some(Attr { name, type_name, uniform, value }))
    }
}

/// Parse a USDA document.
pub fn parse(text: &str) -> Result<Layer, ParseError> {
    let mut lx = Lexer { s: text.as_bytes(), i: 0, line: 1 };
    let mut toks = Vec::new();
    while let Some(t) = lx.next()? {
        toks.push(t);
    }
    let mut p = Parser { toks, pos: 0 };
    let mut layer = Layer::default();
    if p.is_punct('(') {
        for (k, v) in p.metadata()? {
            if k == "defaultPrim"
                && let Lit::Str(s) = v {
                    layer.default_prim = Some(s);
                }
        }
    }
    while p.peek().is_some() {
        match p.peek() {
            Some(Tok::Ident(w)) if matches!(w.as_str(), "def" | "over" | "class") => {
                let prim = p.prim("")?;
                layer.prims.push(prim);
            }
            other => return p.err(format!("expected prim at layer root, found {other:?}")),
        }
    }
    Ok(layer)
}

// ───────────────────────────── Writing helpers ─────────────────────────────

/// Format a double the way USD does: shortest round-trip, `inf`/`nan` spelled out.
pub fn fmt_f64(v: f64) -> String {
    if v.is_nan() {
        "nan".into()
    } else if v.is_infinite() {
        if v > 0.0 { "inf".into() } else { "-inf".into() }
    } else if v == 0.0 {
        "0".into() // also normalizes -0
    } else {
        format!("{v}")
    }
}

/// Quote a string for USDA.
pub fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_prims_metadata_and_values() {
        let src = r#"#usda 1.0
(
    defaultPrim = "World"
    doc = """multi
line"""
    subLayers = [@a.usda@]
)
def Xform "World" ( kind = "component" )
{
    double3 xformOp:translate = (1, 2, -3.5e-2)
    uniform token[] xformOpOrder = ["xformOp:translate"]
    rel material:binding = </World/Mat>
    float3 xformOp:scale.timeSamples = { 1: (1,1,1), 2: (2,2,2) }
    def BrepArray "B" (
        prepend apiSchemas = ["BrepPointAPI:vertexPoint", "MaterialBindingAPI"]
        customData = { string source = "x" }
    )
    {
        uniform uint[] brep:regionCount = [2]
        uniform point3d[] brep:vertexPoint:point:position = [(0, 0, 0), (1, -1, inf)]
        uniform token[] region:type = ["voidRegion", "solidRegion"] ( doc = "d" )
        uniform double[] empty = []
    }
    variantSet "v" = { "a" { def "InVariant" {} } }
}
"#;
        let layer = parse(src).unwrap();
        assert_eq!(layer.default_prim.as_deref(), Some("World"));
        let prims = layer.all_prims();
        let names: Vec<_> = prims.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(names, ["/World", "/World/B", "/World/InVariant"]);
        let b = prims[1];
        assert_eq!(b.type_name.as_deref(), Some("BrepArray"));
        assert_eq!(b.api_schemas, ["BrepPointAPI:vertexPoint", "MaterialBindingAPI"]);
        let pos = b.attr("brep:vertexPoint:point:position").unwrap();
        assert_eq!(pos.type_name, "point3d[]");
        assert!(pos.uniform);
        let Some(Lit::List(items)) = &pos.value else { panic!() };
        assert_eq!(items[1], Lit::Tuple(vec![Lit::Num(1.0), Lit::Num(-1.0), Lit::Ident("inf".into())]));
        assert_eq!(b.attr("empty").unwrap().value, Some(Lit::List(vec![])));
        // timeSamples are skipped, plain attrs and rels kept
        assert!(prims[0].attr("xformOp:scale").is_none());
        assert_eq!(prims[0].attr("material:binding").unwrap().type_name, "rel");
    }

    #[test]
    fn reports_line_of_errors() {
        let e = parse("#usda 1.0\ndef \"A\" {\n  uint[] x = [1, 2\n").unwrap_err();
        assert!(e.line >= 3, "{e}");
    }

    #[test]
    fn number_formatting() {
        assert_eq!(fmt_f64(1.0), "1");
        assert_eq!(fmt_f64(-0.0), "0");
        assert_eq!(fmt_f64(0.00002), "0.00002");
        assert_eq!(fmt_f64(f64::NEG_INFINITY), "-inf");
    }
}
