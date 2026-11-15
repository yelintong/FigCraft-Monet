//! Photoshop ActionDescriptor (OSType key/value trees), as used by ABR v6+ `desc` and `phry`
//! sections. Only the OSTypes an ABR file can hold are decoded, but every one the published
//! layout describes is handled so a real Photoshop brush file never fails on an unknown value.

use crate::io::{Reader, Result, Writer};

/// A class / key / type identifier: a length-prefixed string, where a stored length of 0 means a
/// four-character code follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Id {
    Code([u8; 4]),
    Str(Vec<u8>),
}

impl Id {
    pub fn new(s: &str) -> Self {
        let b = s.as_bytes();
        match b {
            [a, b, c, d] if !matches!(s, "warp" | "list" | "time") => Id::Code([*a, *b, *c, *d]),
            _ => Id::Str(b.to_vec()),
        }
    }

    pub fn is(&self, s: &str) -> bool {
        self.as_bytes() == s.as_bytes()
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Id::Code(c) => c,
            Id::Str(s) => s,
        }
    }

    fn read(r: &mut Reader<'_>) -> Result<Self> {
        let n = r.u32()? as usize;
        if n == 0 {
            Ok(Id::Code(r.arr::<4>()?))
        } else {
            Ok(Id::Str(r.take(n)?.to_vec()))
        }
    }

    fn write(&self, w: &mut Writer) {
        match self {
            Id::Code(c) => {
                w.u32(0);
                w.bytes(c);
            }
            Id::Str(s) => {
                w.u32(s.len() as u32);
                w.bytes(s);
            }
        }
    }
}

/// One OSType-tagged value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Reference(Vec<Vec<u8>>),
    Descriptor(Descriptor),
    GlobalObject(Descriptor),
    List(Vec<Value>),
    Double(f64),
    UnitFloat { unit: [u8; 4], value: f64 },
    UnitFloats { unit: [u8; 4], values: Vec<f64> },
    Text(String),
    Enumerated { type_id: Id, value: Id },
    Integer(i32),
    LargeInteger(i64),
    Boolean(bool),
    Class { kind: [u8; 4], name: String, class_id: Id },
    Raw { kind: [u8; 4], data: Vec<u8> },
    ObjectArray { prefix: u32, body: Descriptor },
}

impl Value {
    pub fn number(&self) -> Option<f64> {
        match self {
            Value::Double(v) | Value::UnitFloat { value: v, .. } => Some(*v),
            Value::Integer(v) => Some(f64::from(*v)),
            Value::LargeInteger(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<String> {
        match self {
            Value::Text(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn descriptor(&self) -> Option<&Descriptor> {
        match self {
            Value::Descriptor(d) | Value::GlobalObject(d) | Value::ObjectArray { body: d, .. } => Some(d),
            _ => None,
        }
    }

    fn tag(&self) -> [u8; 4] {
        match self {
            Value::Reference(_) => *b"obj ",
            Value::Descriptor(_) => *b"Objc",
            Value::GlobalObject(_) => *b"GlbO",
            Value::List(_) => *b"VlLs",
            Value::Double(_) => *b"doub",
            Value::UnitFloat { .. } => *b"UntF",
            Value::UnitFloats { .. } => *b"UnFl",
            Value::Text(_) => *b"TEXT",
            Value::Enumerated { .. } => *b"enum",
            Value::Integer(_) => *b"long",
            Value::LargeInteger(_) => *b"comp",
            Value::Boolean(_) => *b"bool",
            Value::Class { kind, .. } => *kind,
            Value::Raw { kind, .. } => *kind,
            Value::ObjectArray { .. } => *b"ObAr",
        }
    }
}

/// A descriptor: an optional name, a class id and ordered key/value items.
#[derive(Clone, Debug, PartialEq)]
pub struct Descriptor {
    pub name: String,
    pub class_id: Id,
    pub items: Vec<(Id, Value)>,
}

impl Descriptor {
    pub fn new(class_id: &str) -> Self {
        Self { name: String::new(), class_id: Id::new(class_id), items: Vec::new() }
    }

    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.items.push((Id::new(key), value));
        self
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.items.iter().find(|(k, _)| k.is(key)).map(|(_, v)| v)
    }

    pub fn num(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::number)
    }

    /// A percentage as a 0..1 fraction.
    pub fn pct(&self, key: &str) -> Option<f32> {
        self.num(key).map(|v| (v / 100.0) as f32)
    }

    pub fn text(&self, key: &str) -> Option<String> {
        self.get(key).and_then(Value::text)
    }

    pub fn obj(&self, key: &str) -> Option<&Descriptor> {
        self.get(key).and_then(Value::descriptor)
    }

    pub fn boolean(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Some(Value::Boolean(b)) => Some(*b),
            _ => None,
        }
    }

    pub fn class_is(&self, s: &str) -> bool {
        self.class_id.is(s)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        write_descriptor(self, &mut w);
        w.b
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        read_descriptor(&mut r, 0)
    }
}

/// A descriptor prefixed by `u32` version 16 (the only version Photoshop writes in ABR files).
pub struct VersionedDescriptor;

impl VersionedDescriptor {
    pub fn parse_prefix(data: &[u8]) -> Result<Descriptor> {
        let mut r = Reader::new(data);
        let version = r.u32()?;
        if version != 16 {
            return Err(format!("descriptor version {version}, expected 16"));
        }
        read_descriptor(&mut r, 0)
    }

    pub fn to_bytes(d: &Descriptor) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(16);
        write_descriptor(d, &mut w);
        w.b
    }
}

const MAX_DEPTH: usize = 64;

fn read_descriptor(r: &mut Reader<'_>, depth: usize) -> Result<Descriptor> {
    if depth > MAX_DEPTH {
        return Err("descriptor nesting too deep".to_string());
    }
    let name = r.unicode()?;
    let class_id = Id::read(r)?;
    let n = r.u32()? as usize;
    if n > r.left() / 10 {
        return Err(format!("descriptor item count {n} exceeds the remaining data"));
    }
    let mut items = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let key = Id::read(r)?;
        let value = read_value(r, depth + 1)?;
        items.push((key, value));
    }
    Ok(Descriptor { name, class_id, items })
}

fn read_value(r: &mut Reader<'_>, depth: usize) -> Result<Value> {
    if depth > MAX_DEPTH {
        return Err("descriptor nesting too deep".to_string());
    }
    let tag = r.arr::<4>()?;
    Ok(match &tag {
        b"obj " => {
            let n = r.u32()? as usize;
            let mut items = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                let start = r.pos();
                skip_reference_item(r)?;
                items.push(r.b.get(start..r.pos()).unwrap_or(&[]).to_vec());
            }
            Value::Reference(items)
        }
        b"Objc" => Value::Descriptor(read_descriptor(r, depth + 1)?),
        b"GlbO" => Value::GlobalObject(read_descriptor(r, depth + 1)?),
        b"VlLs" => {
            let n = r.u32()? as usize;
            let mut items = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                items.push(read_value(r, depth + 1)?);
            }
            Value::List(items)
        }
        b"doub" => Value::Double(r.f64()?),
        b"UntF" => Value::UnitFloat { unit: r.arr::<4>()?, value: r.f64()? },
        b"UnFl" => {
            let unit = r.arr::<4>()?;
            let n = r.u32()? as usize;
            let mut values = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                values.push(r.f64()?);
            }
            Value::UnitFloats { unit, values }
        }
        b"TEXT" => Value::Text(r.unicode()?),
        b"enum" => Value::Enumerated { type_id: Id::read(r)?, value: Id::read(r)? },
        b"long" => Value::Integer(r.i32()?),
        b"comp" => Value::LargeInteger(r.i64()?),
        b"bool" => Value::Boolean(r.u8()? != 0),
        b"type" | b"GlbC" => {
            let name = r.unicode()?;
            let class_id = Id::read(r)?;
            Value::Class { kind: tag, name, class_id }
        }
        b"alis" | b"Pth " | b"tdta" => {
            let n = r.u32()? as usize;
            Value::Raw { kind: tag, data: r.take(n)?.to_vec() }
        }
        b"ObAr" => Value::ObjectArray { prefix: r.u32()?, body: read_descriptor(r, depth + 1)? },
        other => return Err(format!("unsupported descriptor OSType {:?}", String::from_utf8_lossy(other))),
    })
}

/// Consume one `obj ` reference item, keeping its bytes for a lossless round trip.
fn skip_reference_item(r: &mut Reader<'_>) -> Result<()> {
    let ty = r.arr::<4>()?;
    // Every item starts with a class (Unicode name + id) except the plain class form.
    match &ty {
        b"Clss" => {
            r.skip_unicode_and_id()?;
        }
        b"prop" => {
            r.skip_unicode_and_id()?;
            Id::read(r)?;
        }
        b"Enmr" => {
            r.skip_unicode_and_id()?;
            Id::read(r)?;
            Id::read(r)?;
        }
        b"rele" | b"Idnt" | b"indx" => {
            r.skip_unicode_and_id()?;
            r.u32()?;
        }
        b"name" => {
            r.skip_unicode_and_id()?;
            r.unicode()?;
        }
        other => return Err(format!("unsupported reference item {:?}", String::from_utf8_lossy(other))),
    }
    Ok(())
}

fn write_descriptor(d: &Descriptor, w: &mut Writer) {
    w.unicode(&d.name);
    d.class_id.write(w);
    w.u32(d.items.len() as u32);
    for (k, v) in &d.items {
        k.write(w);
        write_value(v, w);
    }
}

fn write_value(v: &Value, w: &mut Writer) {
    w.bytes(&v.tag());
    match v {
        Value::Reference(items) => {
            w.u32(items.len() as u32);
            for it in items {
                w.bytes(it);
            }
        }
        Value::Descriptor(d) | Value::GlobalObject(d) | Value::ObjectArray { body: d, .. } => {
            if let Value::ObjectArray { prefix, .. } = v {
                w.u32(*prefix);
            }
            write_descriptor(d, w);
        }
        Value::List(items) => {
            w.u32(items.len() as u32);
            for it in items {
                write_value(it, w);
            }
        }
        Value::Double(x) => w.f64(*x),
        Value::UnitFloat { unit, value } => {
            w.bytes(unit);
            w.f64(*value);
        }
        Value::UnitFloats { unit, values } => {
            w.bytes(unit);
            w.u32(values.len() as u32);
            for x in values {
                w.f64(*x);
            }
        }
        Value::Text(s) => w.unicode(s),
        Value::Enumerated { type_id, value } => {
            type_id.write(w);
            value.write(w);
        }
        Value::Integer(x) => w.i32(*x),
        Value::LargeInteger(x) => w.i64(*x),
        Value::Boolean(b) => w.u8(u8::from(*b)),
        Value::Class { kind: _, name, class_id } => {
            w.unicode(name);
            class_id.write(w);
        }
        Value::Raw { data, .. } => {
            w.u32(data.len() as u32);
            w.bytes(data);
        }
    }
}
