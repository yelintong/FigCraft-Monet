//! Brush strokes are made of stamped `.abr` tips, not built-in shapes.
//!
//! This module only works out *where* each dab lands along the path (with the stroke's width
//! profile, spacing and repeatable jitter). The tip's grayscale pixels live in `figcraft-abr` and
//! the compositor in `figcraft-render`; no brush shape is drawn in code any more. The plain stroke
//! outline below is a geometric stand-in for boolean operands, clipping and bounds calculations,
//! which still need a path rather than pixels.

use crate::width::{Polyline, polylines};
use figcraft_doc::StrokeProps;
use figcraft_geom::kurbo::{BezPath, Point, Vec2};

/// The most dabs one stroke makes; denser spacing spreads them out instead of adding more.
pub const MAX_STAMPS: usize = 5_000;

/// One dab: its centre, its direction along the path, its painted diameter and its rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stamp {
    pub at: Point,
    pub dir: Vec2,
    pub size: f64,
    pub angle: f64,
}

/// Spacing and jitter, taken from the ABR preset and the document's brush settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StampSettings {
    /// Distance between dabs as a fraction of the painted diameter.
    pub spacing: f64,
    pub size_jitter: f64,
    pub angle_jitter: f64,
    /// How far dabs stray from the path, in stroke weights.
    pub scatter: f64,
    pub seed: u32,
}

impl Default for StampSettings {
    fn default() -> Self {
        Self { spacing: 0.25, size_jitter: 0.0, angle_jitter: 0.0, scatter: 0.0, seed: 0 }
    }
}

/// A repeatable pseudo-random number in 0..1 for dab `i`, channel `k` (splitmix64).
fn rand(seed: u32, i: u64, k: u64) -> f64 {
    let mut z = u64::from(seed) ^ i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ k.wrapping_mul(0xD1B5_4A32_D192_ED03);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// The dabs of `path` at `width`, in local coordinates. `None` for a path too long to measure.
pub fn stamps(path: &BezPath, width: f64, props: &StrokeProps, s: &StampSettings) -> Option<Vec<Stamp>> {
    let lines = polylines(path)?;
    let total: f64 = lines.iter().map(Polyline::length).sum();
    // Sanitized once: at most 64 points, so every sample stays cheap.
    let profile = props.profile().map(|pr| pr.sanitized());
    let scale = |t: f64| width * profile.as_ref().map_or(1.0, |pr| pr.scale_at(t));
    let min_step = (total / MAX_STAMPS as f64).max(1e-3);
    let mut out = Vec::new();
    let mut i = 0u64;
    for line in &lines {
        let len = line.length();
        if len <= 0.0 {
            continue;
        }
        let mut d = 0.0;
        while d <= len {
            let t = d / len;
            if let Some((p, n)) = line.at(t) {
                let r = |k| rand(s.seed, i, k) * 2.0 - 1.0;
                let size = scale(t) * (1.0 + s.size_jitter * r(0)).max(0.05);
                let dir = Vec2::new(n.y, -n.x);
                let angle = dir.atan2() + s.angle_jitter * std::f64::consts::PI * r(1);
                let at = p + n * (s.scatter * width * r(2));
                out.push(Stamp { at, dir, size, angle });
                if out.len() >= MAX_STAMPS {
                    return Some(out);
                }
            }
            i += 1;
            d += (s.spacing * scale(t)).max(min_step).max(1e-3);
        }
    }
    Some(out)
}

/// The geometric stand-in for a brush stroke: boolean folding, clipping and bounds need a path,
/// and the painted pixels can be a little wider than this. The compositor never fills this path
/// for a brush stroke; it stamps the ABR tip instead.
pub fn fallback_outline(path: &BezPath, width: f64, props: &StrokeProps) -> BezPath {
    crate::stroke_center(path, width, props)
}
