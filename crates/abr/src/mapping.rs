//! Map a parsed `.abr` file onto the tip bitmaps and settings Monet paints with.
//!
//! PhotoCraft's `crates/io/src/abr_map.rs` is the reference for which descriptor keys matter:
//! `Nm  `, `Brsh` (`Dmtr`, `Angl`, `Rndn`, `Hrdn`, `Spcn`, `flipX`, `flipY`, `sampledData`),
//! shape dynamics (`szVr`, `angleDynamics`), scattering (`scatterDynamics`) and `useTipDynamics`
//! / `useScatter`. Settings without a Monet equivalent are reported as warnings, not invented.

use crate::format::{AbrFile, AbrSample, LegacyTip};

/// A grayscale brush tip. 255 is full paint, 0 is transparent.
#[derive(Clone, Debug, PartialEq)]
pub struct GrayTip {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

impl GrayTip {
    /// A sampled tip. ABR files in the wild disagree on polarity (some store ink as black, some
    /// as white), so compare the border with the centre and invert only when the border is lighter.
    pub fn from_sample(s: &AbrSample) -> Self {
        let unit = s.to_unit();
        let mut alpha: Vec<u8> = unit.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
        if inverted_ink(&alpha, s.width, s.height) {
            for a in &mut alpha {
                *a = 255 - *a;
            }
        }
        Self { width: s.width, height: s.height, alpha }
    }

    /// Photoshop's computed (elliptical) tip, rendered to the same grayscale plane.
    pub fn computed(diameter: u16, hardness: u16, angle: i16, roundness: u16) -> Self {
        let d = u32::from(diameter).clamp(1, 1024);
        let round = (f64::from(roundness) / 100.0).clamp(0.01, 1.0);
        let hard = (f64::from(hardness) / 100.0).clamp(0.0, 1.0);
        let a = f64::from(angle).to_radians();
        let (ca, sa) = (a.cos(), a.sin());
        let mut alpha = vec![0u8; (d * d) as usize];
        for y in 0..d {
            let ny = (f64::from(y) + 0.5) / f64::from(d) * 2.0 - 1.0;
            for x in 0..d {
                let nx = (f64::from(x) + 0.5) / f64::from(d) * 2.0 - 1.0;
                let (rx, ry) = (nx * ca + ny * sa, -nx * sa + ny * ca);
                let r = (rx * rx + (ry / round).powi(2)).sqrt();
                let ink = if r >= 1.0 { 0.0 } else if r <= hard { 1.0 } else { ((1.0 - r) / (1.0 - hard).max(1e-4)).clamp(0.0, 1.0) };
                alpha[(y * d + x) as usize] = (ink * 255.0 + 0.5) as u8;
            }
        }
        Self { width: d, height: d, alpha }
    }

    /// A one-pixel-wide fallback tip (used only when a file contains no usable tip at all).
    pub fn dot() -> Self {
        Self { width: 1, height: 1, alpha: vec![255] }
    }

    /// Bilinear paint amount at `u, v` in 0..1; outside the tip it is transparent.
    pub fn at(&self, u: f64, v: f64) -> f32 {
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) || self.width == 0 || self.height == 0 {
            return 0.0;
        }
        let x = u * f64::from(self.width) - 0.5;
        let y = v * f64::from(self.height) - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let get = |xi: i64, yi: i64| -> f32 {
            if xi < 0 || yi < 0 || xi >= i64::from(self.width) || yi >= i64::from(self.height) {
                return 0.0;
            }
            f32::from(self.alpha.get((yi as u32 * self.width + xi as u32) as usize).copied().unwrap_or(0)) / 255.0
        };
        let (a, b, c, d) = (get(x0 as i64, y0 as i64), get(x0 as i64 + 1, y0 as i64), get(x0 as i64, y0 as i64 + 1), get(x0 as i64 + 1, y0 as i64 + 1));
        let (fx, fy) = (fx as f32, fy as f32);
        (a * (1.0 - fx) + b * fx) * (1.0 - fy) + (c * (1.0 - fx) + d * fx) * fy
    }

    /// A square grayscale thumbnail, `edge × edge` bytes (255 = ink).
    pub fn thumbnail(&self, edge: u32) -> Vec<u8> {
        let n = edge.max(1);
        let mut out = vec![0u8; (n * n) as usize];
        let side = self.width.max(self.height).max(1);
        for y in 0..n {
            for x in 0..n {
                // Centre the (possibly non-square) tip in the square thumbnail.
                let u = (f64::from(x) + 0.5) / f64::from(n) * f64::from(side) / f64::from(self.width.max(1));
                let v = (f64::from(y) + 0.5) / f64::from(n) * f64::from(side) / f64::from(self.height.max(1));
                out[(y * n + x) as usize] = (self.at(u, v) * 255.0 + 0.5) as u8;
            }
        }
        out
    }

    /// Tip aspect (width / height); stamping scales the tip to the stroke weight by this ratio.
    pub fn aspect(&self) -> f64 {
        f64::from(self.width.max(1)) / f64::from(self.height.max(1))
    }
}

fn inverted_ink(alpha: &[u8], w: u32, h: u32) -> bool {
    if w < 4 || h < 4 {
        return false;
    }
    let at = |x: u32, y: u32| f64::from(alpha.get((y * w + x) as usize).copied().unwrap_or(0));
    let mut border = 0.0;
    let mut border_n: f64 = 0.0;
    for x in 0..w {
        for y in [0, 1, h - 2, h - 1] {
            border += at(x, y);
            border_n += 1.0;
        }
    }
    for y in 0..h {
        for x in [0, 1, w - 2, w - 1] {
            border += at(x, y);
            border_n += 1.0;
        }
    }
    let mut centre = 0.0;
    let mut centre_n: f64 = 0.0;
    for y in h / 4..h * 3 / 4 {
        for x in w / 4..w * 3 / 4 {
            centre += at(x, y);
            centre_n += 1.0;
        }
    }
    border / border_n.max(1.0_f64) > centre / centre_n.max(1.0_f64) + 25.0
}

/// One brush preset from an `.abr` file.
#[derive(Clone, Debug)]
pub struct BrushDef {
    /// Stable id saved in documents: `<file>/<index>/<name>`.
    pub id: String,
    pub name: String,
    pub group: String,
    pub folder: Vec<String>,
    pub tip: GrayTip,
    /// Distance between dabs as a fraction of the painted diameter.
    pub spacing: f32,
    /// The tip's own diameter in pixels, for display.
    pub diameter: f32,
    pub angle: f32,
    pub roundness: f32,
    pub hardness: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    /// Shape dynamics / scattering mapped onto Monet's existing brush parameters.
    pub size_jitter: f32,
    pub angle_jitter: f32,
    pub scatter: f32,
    pub seed: u32,
}

fn seed_of(s: &str) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for b in s.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn jitter(d: &crate::descriptor::Descriptor, key: &str) -> f32 {
    d.obj(key).and_then(|v| v.pct("jitter")).unwrap_or(0.0).clamp(0.0, 1.0)
}

fn preset_brush(d: &crate::descriptor::Descriptor, file: &AbrFile, group: &str, i: usize, folder: Vec<String>, name: &str) -> Option<BrushDef> {
    let shape = d.obj("Brsh")?;
    let sampled = shape.class_is("sampledBrush") || shape.get("sampledData").is_some();
    let tip_id = shape.text("sampledData").unwrap_or_default();
    let sample = file.samples.iter().find(|s| !tip_id.is_empty() && s.id == tip_id).or_else(|| file.samples.first().filter(|_| file.samples.len() == 1));
    let tip = if sampled {
        GrayTip::from_sample(sample?)
    } else {
        GrayTip::computed(
            shape.num("Dmtr").unwrap_or(16.0).clamp(1.0, 5000.0) as u16,
            (shape.pct("Hrdn").unwrap_or(1.0) * 100.0).clamp(0.0, 100.0) as u16,
            shape.num("Angl").unwrap_or(0.0) as i16,
            (shape.pct("Rndn").unwrap_or(1.0) * 100.0).clamp(1.0, 100.0) as u16,
        )
    };
    let diameter = shape.num("Dmtr").unwrap_or(f64::from(tip.width.max(tip.height))).clamp(1.0, 5000.0) as f32;
    let dynamics = d.boolean("useTipDynamics") == Some(true);
    let scattering = d.boolean("useScatter") == Some(true);
    Some(BrushDef {
        id: format!("{group}/{i:04}/{name}"),
        name: name.to_string(),
        group: group.to_string(),
        folder,
        tip,
        spacing: shape.pct("Spcn").unwrap_or(0.25).clamp(0.01, 10.0),
        diameter,
        angle: shape.num("Angl").unwrap_or(0.0) as f32,
        roundness: shape.pct("Rndn").unwrap_or(1.0).clamp(0.01, 1.0),
        hardness: shape.pct("Hrdn").unwrap_or(1.0).clamp(0.0, 1.0),
        flip_x: shape.boolean("flipX").unwrap_or(false),
        flip_y: shape.boolean("flipY").unwrap_or(false),
        size_jitter: if dynamics { jitter(d, "szVr") } else { 0.0 },
        angle_jitter: if dynamics { jitter(d, "angleDynamics") } else { 0.0 },
        scatter: if scattering { jitter(d, "scatterDynamics") * 2.0 } else { 0.0 },
        seed: seed_of(&format!("{group}/{name}")),
    })
}

/// All brushes of one file, in file order. The second value holds non-fatal warnings.
pub fn brushes_of(file: &AbrFile, group: &str) -> (Vec<BrushDef>, Vec<String>) {
    let mut out = Vec::new();
    let mut warnings = file.warnings.clone();
    if !file.legacy.is_empty() {
        for (i, b) in file.legacy.iter().enumerate() {
            let name = if b.name.trim().is_empty() { format!("Brush {}", i + 1) } else { b.name.trim().to_string() };
            let (tip, diameter) = match &b.tip {
                LegacyTip::Sampled(s) => (GrayTip::from_sample(s), f64::from(s.width.max(s.height))),
                LegacyTip::Computed { diameter, hardness, angle, roundness } => {
                    (GrayTip::computed(*diameter, *hardness, *angle, *roundness), f64::from(*diameter))
                }
            };
            let spacing = if b.spacing == 0 { 0.25 } else { f32::from(b.spacing) / 100.0 };
            out.push(BrushDef {
                id: format!("{group}/{i:04}/{name}"),
                name: name.clone(),
                group: group.to_string(),
                folder: Vec::new(),
                tip,
                spacing: spacing.clamp(0.01, 10.0),
                diameter: diameter as f32,
                angle: 0.0,
                roundness: 1.0,
                hardness: 1.0,
                flip_x: false,
                flip_y: false,
                size_jitter: 0.0,
                angle_jitter: 0.0,
                scatter: 0.0,
                seed: seed_of(&format!("{group}/{name}")),
            });
        }
    } else if file.presets.is_empty() {
        // Early v6 files: one preset per sampled tip, no settings section.
        for (i, s) in file.samples.iter().enumerate() {
            let name = format!("Sampled Brush {}", i + 1);
            out.push(BrushDef {
                id: format!("{group}/{i:04}/{name}"),
                name: name.clone(),
                group: group.to_string(),
                folder: Vec::new(),
                tip: GrayTip::from_sample(s),
                spacing: 0.25,
                diameter: s.width.max(s.height) as f32,
                angle: 0.0,
                roundness: 1.0,
                hardness: 1.0,
                flip_x: false,
                flip_y: false,
                size_jitter: 0.0,
                angle_jitter: 0.0,
                scatter: 0.0,
                seed: seed_of(&format!("{group}/{name}")),
            });
        }
    } else {
        for (i, d) in file.presets.iter().enumerate() {
            let name = d.text("Nm  ").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| format!("Brush {}", i + 1));
            let folder = file.folders.get(i).cloned().unwrap_or_default();
            match preset_brush(d, file, group, i, folder, &name) {
                Some(b) => out.push(b),
                None => warnings.push(format!("\"{name}\": its sampled tip is missing from the file; skipped")),
            }
        }
    }
    (out, warnings)
}
