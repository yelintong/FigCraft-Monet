//! Photoshop brush files (`.abr`), implemented from the published format layout. All numbers are
//! big-endian.
//!
//! ```text
//! v1 / v2   := version u16 (1|2), count u16, count × brush
//! brush     := type u16 (1 computed, 2 sampled), length u32, body[length]
//! computed  := misc u32, spacing u16 (%), diameter u16, roundness u16 (%), angle i16, hardness u16 (%)
//! sampled   := misc u32, spacing u16 (%), [v2: name (u32 length + UTF-16)],
//!              anti-alias u8, short bounds 4 × i16, bounds 4 × i32, depth u16 (8|16),
//!              compression u8 (0 raw, 1 PackBits with a u16 row-length table)
//!
//! v6+       := version u16 (6..=10), subversion u16 (1|2), sections…
//! section   := "8BIM", key [4], length u32, data[length] (padded to 4)
//! "samp"    := entries: length u32, Pascal uuid, skip (10 / 264), sample body
//! "desc"    := version u32 (16) + ActionDescriptor; key `Brsh` lists the brush presets
//! "phry"    := version u32 (16) + ActionDescriptor; `hierarchy` lists folders and preset tokens
//! ```
//!
//! A damaged brush is skipped with a warning when the rest of the file is still readable; parsing
//! never panics and every decoded tip is capped by [`MAX_EDGE`] and [`MAX_TOTAL_BYTES`].

use crate::descriptor::{Descriptor, Value, VersionedDescriptor};
use crate::io::{Reader, Result, Writer};

/// Largest sampled tip edge accepted (Photoshop's own limit is 5000 px).
pub const MAX_EDGE: u32 = 8192;
/// Most brushes read from one file.
pub const MAX_BRUSHES: usize = 20_000;
/// Cap on decoded sample bytes over the whole file.
pub const MAX_TOTAL_BYTES: usize = if cfg!(target_pointer_width = "64") { 2 << 30 } else { 512 << 20 };
/// Deepest folder nesting read from a preset hierarchy.
pub const MAX_FOLDER_DEPTH: usize = 32;

/// A sampled tip: one grayscale plane where the maximum value is full paint.
#[derive(Clone, Debug, PartialEq)]
pub struct AbrSample {
    /// The uuid a v6+ preset's `sampledData` refers to.
    pub id: String,
    pub width: u32,
    pub height: u32,
    /// Bits per sample: 8 or 16.
    pub depth: u16,
    /// Decoded samples, row-major; 16-bit samples are big-endian.
    pub data: Vec<u8>,
}

impl AbrSample {
    /// Samples as 0..1 paint amounts.
    pub fn to_unit(&self) -> Vec<f32> {
        if self.depth == 16 {
            self.data.chunks_exact(2).map(|c| f32::from(u16::from_be_bytes([c[0], c[1]])) / 65535.0).collect()
        } else {
            self.data.iter().map(|&v| f32::from(v) / 255.0).collect()
        }
    }
}

/// A legacy (v1/v2) tip.
#[derive(Clone, Debug, PartialEq)]
pub enum LegacyTip {
    Computed { diameter: u16, hardness: u16, angle: i16, roundness: u16 },
    Sampled(AbrSample),
}

/// A legacy (v1/v2) brush.
#[derive(Clone, Debug, PartialEq)]
pub struct LegacyBrush {
    /// Name (v2 sampled brushes only; empty otherwise).
    pub name: String,
    /// Spacing in percent of the diameter (0 = spacing off).
    pub spacing: u16,
    pub anti_alias: bool,
    pub tip: LegacyTip,
}

/// A parsed brush file.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AbrFile {
    pub version: u16,
    /// v6+ subversion (1 or 2); 0 for v1/v2.
    pub subversion: u16,
    pub legacy: Vec<LegacyBrush>,
    pub samples: Vec<AbrSample>,
    /// `brushPreset` objects, in file order.
    pub presets: Vec<Descriptor>,
    /// Folder path of each preset, parallel to [`AbrFile::presets`].
    pub folders: Vec<Vec<String>>,
    /// Problems that did not stop the parse.
    pub warnings: Vec<String>,
}

fn row_bytes(w: u32, depth: u16) -> usize {
    (w as usize).saturating_mul(usize::from(depth / 8))
}

/// PackBits (Apple / TIFF) decoding of one row into exactly `expected` bytes.
fn packbits_decode(src: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(expected);
    let mut i = 0usize;
    while out.len() < expected {
        let h = *src.get(i).ok_or_else(|| "PackBits row ended early".to_string())? as i8;
        i += 1;
        if h >= 0 {
            let len = h as usize + 1;
            let lit = src.get(i..i.saturating_add(len)).ok_or_else(|| "PackBits literal truncated".to_string())?;
            if out.len().saturating_add(len) > expected {
                return Err("PackBits literal overflows the row".to_string());
            }
            out.extend_from_slice(lit);
            i += len;
        } else if h != -128 {
            let len = (1 - h as isize) as usize;
            let b = *src.get(i).ok_or_else(|| "PackBits run truncated".to_string())?;
            i += 1;
            if out.len().saturating_add(len) > expected {
                return Err("PackBits run overflows the row".to_string());
            }
            out.resize(out.len() + len, b);
        }
    }
    Ok(out)
}

/// A legal (literal-only) PackBits stream. Writing uncompressed rows is valid and keeps the
/// writer simple; real Photoshop files use runs, which [`packbits_decode`] handles.
fn packbits_encode(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() + src.len().div_ceil(128));
    for chunk in src.chunks(128) {
        out.push((chunk.len() - 1) as u8);
        out.extend_from_slice(chunk);
    }
    out
}

fn decode_planes(compression: u8, data: &[u8], w: u32, h: u32, depth: u16) -> Result<Vec<u8>> {
    let rb = row_bytes(w, depth);
    let total = rb.saturating_mul(h as usize);
    match compression {
        0 => {
            if data.len() < total {
                return Err(format!("raw brush tip ends after {} of {total} bytes", data.len()));
            }
            Ok(data[..total].to_vec())
        }
        1 => {
            let mut r = Reader::new(data);
            let mut counts = Vec::with_capacity(h as usize);
            for _ in 0..h {
                counts.push(r.u16()? as usize);
            }
            let mut out = Vec::with_capacity(total);
            for c in counts {
                let row = r.take(c)?;
                out.extend_from_slice(&packbits_decode(row, rb)?);
            }
            if out.len() != total {
                return Err(format!("RLE brush tip decoded {} of {total} bytes", out.len()));
            }
            Ok(out)
        }
        c => Err(format!("unsupported brush sample compression {c}")),
    }
}

fn encode_planes(decoded: &[u8], w: u32, h: u32, depth: u16, rle: bool) -> Vec<u8> {
    if !rle {
        return decoded.to_vec();
    }
    let rb = row_bytes(w, depth);
    let mut rows = Vec::with_capacity(h as usize);
    for y in 0..h as usize {
        let start = y.saturating_mul(rb);
        let row = decoded.get(start..start.saturating_add(rb)).unwrap_or(&[]);
        rows.push(packbits_encode(row));
    }
    let mut out = Vec::new();
    for r in &rows {
        out.extend_from_slice(&(r.len() as u16).to_be_bytes());
    }
    for r in rows {
        out.extend_from_slice(&r);
    }
    out
}

/// Read bounds, depth, compression and the (possibly RLE-compressed) grayscale plane.
fn read_sample_body(r: &mut Reader<'_>, id: String, budget: &mut usize) -> Result<AbrSample> {
    let (top, left, bottom, right) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
    let w = i64::from(right) - i64::from(left);
    let h = i64::from(bottom) - i64::from(top);
    if w <= 0 || h <= 0 {
        return Err(format!("brush sample bounds {w}×{h}"));
    }
    if w > i64::from(MAX_EDGE) || h > i64::from(MAX_EDGE) {
        return Err("brush sample larger than MAX_EDGE".to_string());
    }
    let (w, h) = (w as u32, h as u32);
    let depth = r.u16()?;
    if depth != 8 && depth != 16 {
        return Err(format!("brush sample depth {depth}"));
    }
    let compression = r.u8()?;
    let need = row_bytes(w, depth).saturating_mul(h as usize);
    *budget = budget.checked_sub(need).ok_or_else(|| "abr samples exceed MAX_TOTAL_BYTES".to_string())?;
    let rest = r.take(r.left())?;
    let data = decode_planes(compression, rest, w, h, depth)?;
    Ok(AbrSample { id, width: w, height: h, depth, data })
}

fn read_ucs2(r: &mut Reader<'_>) -> Result<String> {
    let n = r.u32()? as usize;
    if n > 4096 {
        return Err("abr name length".to_string());
    }
    let bytes = r.take(n.saturating_mul(2))?;
    let mut units = Vec::with_capacity(n);
    for c in bytes.chunks_exact(2) {
        units.push(u16::from_be_bytes([c[0], c[1]]));
    }
    while units.last() == Some(&0) {
        units.pop();
    }
    Ok(String::from_utf16_lossy(&units))
}

fn parse_v12(r: &mut Reader<'_>, version: u16, out: &mut AbrFile) -> Result<()> {
    let count = r.u16()?;
    let mut budget = MAX_TOTAL_BYTES;
    for i in 0..count {
        if r.left() == 0 {
            out.warnings.push(format!("file ends after {i} of {count} brushes"));
            break;
        }
        let ty = r.u16()?;
        let len = r.u32()? as usize;
        let body = match r.take(len) {
            Ok(b) => b,
            Err(_) => {
                out.warnings.push(format!("brush {}: truncated", i + 1));
                break;
            }
        };
        let mut b = Reader::new(body);
        let parsed: Result<Option<LegacyBrush>> = (|| {
            match ty {
                1 => {
                    b.u32()?; // misc
                    let spacing = b.u16()?;
                    let diameter = b.u16()?;
                    let roundness = b.u16()?;
                    let angle = b.u16()? as i16;
                    let hardness = b.u16()?;
                    Ok(Some(LegacyBrush {
                        name: String::new(),
                        spacing,
                        anti_alias: true,
                        tip: LegacyTip::Computed { diameter, hardness, angle, roundness },
                    }))
                }
                2 => {
                    b.u32()?; // misc
                    let spacing = b.u16()?;
                    let name = if version == 2 { read_ucs2(&mut b)? } else { String::new() };
                    let anti_alias = b.u8()? != 0;
                    b.skip(8)?; // short bounds (superseded by the 32-bit bounds)
                    let tip = read_sample_body(&mut b, String::new(), &mut budget)?;
                    Ok(Some(LegacyBrush { name, spacing, anti_alias, tip: LegacyTip::Sampled(tip) }))
                }
                other => {
                    out.warnings.push(format!("brush {}: unknown brush type {other} skipped", i + 1));
                    Ok(None)
                }
            }
        })();
        match parsed {
            Ok(Some(lb)) => out.legacy.push(lb),
            Ok(None) => {}
            Err(e) if e.contains("MAX_TOTAL_BYTES") => return Err(e),
            Err(e) => out.warnings.push(format!("brush {}: {e}; skipped", i + 1)),
        }
    }
    Ok(())
}

fn read_samp(data: &[u8], subversion: u16, out: &mut AbrFile, budget: &mut usize) -> Result<()> {
    let mut r = Reader::new(data);
    while r.left() >= 4 {
        if out.samples.len() >= MAX_BRUSHES {
            out.warnings.push("too many sampled tips; the rest were skipped".to_string());
            break;
        }
        let len = r.u32()? as usize;
        if len == 0 {
            break;
        }
        let padded = len.checked_add(3).ok_or_else(|| "abr sample length overflow".to_string())? & !3;
        let take = padded.min(r.left());
        let body = match r.take(take) {
            Ok(b) if b.len() >= len => b,
            _ => {
                out.warnings.push(format!("sampled tip {}: truncated", out.samples.len() + 1));
                break;
            }
        };
        let mut b = Reader::new(body);
        let parsed: Result<AbrSample> = (|| {
            let n = b.u8()? as usize;
            let id = String::from_utf8_lossy(b.take(n)?).to_string();
            b.skip(if subversion == 1 { 10 } else { 264 })?;
            read_sample_body(&mut b, id, budget)
        })();
        match parsed {
            Ok(s) => out.samples.push(s),
            Err(e) if e.contains("MAX_TOTAL_BYTES") => return Err(e),
            Err(e) => out.warnings.push(format!("sampled tip {}: {e}; skipped", out.samples.len() + 1)),
        }
    }
    Ok(())
}

fn read_desc(data: &[u8], out: &mut AbrFile) {
    match VersionedDescriptor::parse_prefix(data) {
        Ok(d) => match d.get("Brsh") {
            Some(Value::List(items)) => {
                for v in items.iter().take(MAX_BRUSHES) {
                    if let Some(preset) = v.descriptor() {
                        out.presets.push(preset.clone());
                    }
                }
            }
            _ => out.warnings.push("brush settings (desc) hold no brush list".to_string()),
        },
        Err(e) => out.warnings.push(format!("brush settings (desc) unreadable: {e}")),
    }
}

/// Pair `phry` tokens with the presets read from `desc`. A damaged hierarchy never fails the
/// parse: the presets it affects land at the top level.
fn read_phry(data: &[u8], presets: usize, warnings: &mut Vec<String>) -> Option<Vec<Vec<String>>> {
    let d = match VersionedDescriptor::parse_prefix(data) {
        Ok(d) => d,
        Err(e) => {
            warnings.push(format!("brush folders (phry) unreadable: {e}; brushes imported without folders"));
            return None;
        }
    };
    let Some(Value::List(items)) = d.get("hierarchy") else {
        warnings.push("brush folders (phry) hold no hierarchy; brushes imported without folders".to_string());
        return None;
    };
    let mut stack: Vec<String> = Vec::new();
    let mut out: Vec<Vec<String>> = Vec::with_capacity(presets.min(MAX_BRUSHES));
    let mut listed = 0usize;
    for v in items {
        let Some(token) = v.descriptor() else { continue };
        if token.class_is("Grup") {
            if stack.len() >= MAX_FOLDER_DEPTH {
                continue;
            }
            let name: String = token.text("Nm  ").unwrap_or_default().trim().chars().take(255).collect();
            stack.push(if name.is_empty() { "Group".to_string() } else { name });
        } else if token.class_is("groupEnd") {
            if stack.pop().is_none() {
                warnings.push("brush folders (phry): a folder end without a folder was ignored".to_string());
            }
        } else if token.class_is("preset") {
            listed += 1;
            if out.len() < presets {
                out.push(stack.clone());
            }
        }
    }
    if listed != presets {
        warnings.push(format!("brush folders (phry) list {listed} brushes but the file holds {presets}"));
        out.resize(presets, Vec::new());
    }
    Some(out)
}

fn parse_v6(r: &mut Reader<'_>, out: &mut AbrFile) -> Result<()> {
    out.subversion = r.u16()?;
    if !matches!(out.subversion, 1 | 2) {
        return Err(format!("brush file subversion {}", out.subversion));
    }
    let mut budget = MAX_TOTAL_BYTES;
    let mut hierarchy: Option<Vec<u8>> = None;
    while r.left() >= 12 {
        let sig = r.arr::<4>()?;
        if &sig != b"8BIM" {
            out.warnings.push(format!("unexpected data at byte {} ignored", r.pos() - 4));
            break;
        }
        let key = r.arr::<4>()?;
        let len = r.u32()? as usize;
        let data: Vec<u8> = match r.take(len) {
            Ok(d) => d.to_vec(),
            Err(_) => {
                out.warnings.push(format!("section {} truncated", String::from_utf8_lossy(&key)));
                r.take(r.left())?.to_vec()
            }
        };
        // Sections are padded to 4 bytes; tolerate writers that do not pad.
        let pad = (4 - len % 4) % 4;
        if pad > 0 && r.b.get(r.pos()..r.pos() + 4) != Some(b"8BIM") && r.b.get(r.pos() + pad..r.pos() + pad + 4) == Some(b"8BIM") {
            r.p += pad;
        }
        match &key {
            b"samp" => read_samp(&data, out.subversion, out, &mut budget)?,
            b"desc" => read_desc(&data, out),
            b"phry" => hierarchy = Some(data),
            b"patt" | b"lPdc" => out.warnings.push(format!("section {} is not used by Monet", String::from_utf8_lossy(&key))),
            k => out.warnings.push(format!("section {} ignored", String::from_utf8_lossy(k))),
        }
    }
    if let Some(h) = hierarchy.as_ref()
        && let Some(folders) = read_phry(h, out.presets.len(), &mut out.warnings)
        && folders.iter().any(|f| !f.is_empty())
    {
        out.folders = folders;
    }
    Ok(())
}

/// Parse an `.abr` file.
pub fn parse(data: &[u8]) -> Result<AbrFile> {
    let mut r = Reader::new(data);
    let version = r.u16()?;
    let mut out = AbrFile { version, ..Default::default() };
    match version {
        1 | 2 => parse_v12(&mut r, version, &mut out)?,
        6..=10 => parse_v6(&mut r, &mut out)?,
        v => return Err(format!("unsupported brush file version {v}")),
    }
    if out.legacy.is_empty() && out.samples.is_empty() && out.presets.is_empty() {
        let why = out.warnings.first().map(|w| format!(" ({w})")).unwrap_or_default();
        return Err(format!("the brush file holds no readable brushes{why}"));
    }
    Ok(out)
}

// ------------------------------------------------------------------ writing

fn write_sample_body(w: &mut Writer, s: &AbrSample, rle: bool) -> Result<()> {
    if s.width == 0 || s.height == 0 || s.width > MAX_EDGE || s.height > MAX_EDGE || !matches!(s.depth, 8 | 16) {
        return Err("brush sample size or depth".to_string());
    }
    for v in [0i32, 0, s.height as i32, s.width as i32] {
        w.i32(v);
    }
    w.u16(s.depth);
    w.u8(u8::from(rle));
    w.bytes(&encode_planes(&s.data, s.width, s.height, s.depth, rle));
    Ok(())
}

/// Write a v1 or v2 file. Sampled tips are PackBits-compressed when `rle`.
pub fn write_v12(version: u16, brushes: &[LegacyBrush], rle: bool) -> Result<Vec<u8>> {
    if !matches!(version, 1 | 2) || brushes.len() > usize::from(u16::MAX) {
        return Err(format!("cannot write brush file version {version}"));
    }
    let mut w = Writer::new();
    w.u16(version);
    w.u16(brushes.len() as u16);
    for b in brushes {
        let mut body = Writer::new();
        body.u32(0); // misc
        body.u16(b.spacing);
        let ty = match &b.tip {
            LegacyTip::Computed { diameter, hardness, angle, roundness } => {
                for v in [*diameter, *roundness, *angle as u16, *hardness] {
                    body.u16(v);
                }
                1
            }
            LegacyTip::Sampled(s) => {
                if version == 2 {
                    let units: Vec<u16> = b.name.encode_utf16().chain(std::iter::once(0)).collect();
                    body.u32(units.len() as u32);
                    for u in units {
                        body.u16(u);
                    }
                }
                body.u8(u8::from(b.anti_alias));
                for v in [0u16, 0, s.height.min(0xffff) as u16, s.width.min(0xffff) as u16] {
                    body.u16(v);
                }
                write_sample_body(&mut body, s, rle)?;
                2
            }
        };
        w.u16(ty);
        w.u32(body.b.len() as u32);
        w.bytes(&body.b);
    }
    Ok(w.b)
}

fn section(w: &mut Writer, key: &[u8; 4], data: &[u8]) {
    w.bytes(b"8BIM");
    w.bytes(key);
    let pad = (4 - data.len() % 4) % 4;
    w.u32((data.len() + pad) as u32);
    w.bytes(data);
    w.bytes(&vec![0u8; pad]);
}

fn hierarchy_descriptor(folders: &[Vec<String>]) -> Descriptor {
    let text = |s: &str| Value::Text(s.to_string());
    let mut items = Vec::new();
    for path in folders {
        for name in path.iter().take(MAX_FOLDER_DEPTH) {
            items.push(Value::Descriptor(Descriptor::new("Grup").with("Nm  ", text(name))));
        }
        items.push(Value::Descriptor(Descriptor::new("preset")));
        for _ in path.iter().take(MAX_FOLDER_DEPTH) {
            items.push(Value::Descriptor(Descriptor::new("groupEnd")));
        }
    }
    Descriptor::new("null").with("hierarchy", Value::List(items))
}

/// Write a v6 file: sampled tips and their preset descriptors, optionally with a preset hierarchy.
pub fn write_v6(subversion: u16, samples: &[AbrSample], presets: &[Descriptor], folders: &[Vec<String>], rle: bool) -> Result<Vec<u8>> {
    if !matches!(subversion, 1 | 2) {
        return Err(format!("cannot write brush file subversion {subversion}"));
    }
    if !folders.is_empty() && folders.len() != presets.len() {
        return Err(format!("{} folder paths for {} brush presets", folders.len(), presets.len()));
    }
    let mut w = Writer::new();
    w.u16(6);
    w.u16(subversion);

    let mut samp = Writer::new();
    for s in samples {
        let mut e = Writer::new();
        let id = s.id.as_bytes();
        let n = id.len().min(255);
        e.u8(n as u8);
        e.bytes(&id[..n]);
        e.bytes(&vec![0u8; if subversion == 1 { 10 } else { 264 }]);
        write_sample_body(&mut e, s, rle)?;
        samp.u32(e.b.len() as u32);
        let pad = (4 - e.b.len() % 4) % 4;
        samp.bytes(&e.b);
        samp.bytes(&vec![0u8; pad]);
    }
    section(&mut w, b"samp", &samp.b);

    let list = Value::List(presets.iter().cloned().map(Value::Descriptor).collect());
    section(&mut w, b"desc", &VersionedDescriptor::to_bytes(&Descriptor::new("null").with("Brsh", list)));
    if folders.iter().any(|f| !f.is_empty()) {
        section(&mut w, b"phry", &VersionedDescriptor::to_bytes(&hierarchy_descriptor(folders)));
    }
    Ok(w.b)
}
