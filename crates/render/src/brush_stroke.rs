//! Raster brush strokes: stamp the grayscale tip of an external `.abr` preset along the path.
//!
//! The document stays vector (it records the path, the stroke weight and the brush id). The dabs
//! are composited here, into an 8-bit alpha mask, then filled with the stroke's paint through a
//! local pixmap. This keeps ABR's soft edges, texture and irregular tips instead of tracing them
//! into a hard vector outline. The mask is cached per geometry / preset / device transform.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use figcraft_abr::BrushDef;
use figcraft_doc::{Brush, Node, StrokeProps};
use figcraft_geom::kurbo::{BezPath, PathEl, Point};
use figcraft_geom::{Affine, Rect};
use figcraft_pathops::brush::{Stamp, StampSettings, stamps};
use tiny_skia::{FilterQuality, IntSize, Mask, Pixmap, PixmapPaint, Transform};

use crate::draw::{Ctx, ts_transform};
use crate::paint::{prepare_at, ts_blend};

/// Most device pixels one stroke's mask may cover (a guard against a huge stroke at high zoom).
const MAX_MASK_PIXELS: usize = 16 << 20;
const MAX_CACHED_MASKS: usize = 256;

/// The pixmap-space rectangle a brush stroke's mask covers.
struct MaskArea {
    x0: i32,
    y0: i32,
    w: u32,
    h: u32,
}

fn cache() -> &'static Mutex<HashMap<u64, Arc<Vec<u8>>>> {
    static C: OnceLock<Mutex<HashMap<u64, Arc<Vec<u8>>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn settings(def: &BrushDef, b: &Brush) -> StampSettings {
    // The document's `spacing` is a user override; its default means "use the ABR preset's own
    // spacing" (both are fractions of the painted diameter, which is the stroke weight).
    let default_spacing = Brush::default().spacing;
    let spacing = if (b.spacing - default_spacing).abs() > 1e-9 { b.spacing } else { f64::from(def.spacing) };
    StampSettings {
        spacing,
        size_jitter: f64::from(def.size_jitter.max(b.size_jitter as f32)),
        angle_jitter: f64::from(def.angle_jitter.max(b.rotation_jitter as f32)),
        scatter: f64::from(def.scatter.max(b.scatter as f32)),
        seed: if b.seed == 0 { def.seed } else { b.seed },
    }
}

fn hash_geometry<H: Hasher>(geom: &BezPath, h: &mut H) {
    let point = |p: &Point, h: &mut H| {
        p.x.to_bits().hash(h);
        p.y.to_bits().hash(h);
    };
    for el in geom.elements() {
        match el {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => point(p, h),
            PathEl::QuadTo(a, b) => {
                point(a, h);
                point(b, h);
            }
            PathEl::CurveTo(a, b, c) => {
                point(a, h);
                point(b, h);
                point(c, h);
            }
            PathEl::ClosePath => 0u8.hash(h),
        }
    }
}

fn cache_key(geom: &BezPath, def: &BrushDef, b: &Brush, props: &StrokeProps, xf: Affine, area: &MaskArea, s: &StampSettings) -> u64 {
    let mut h = DefaultHasher::new();
    hash_geometry(geom, &mut h);
    def.id.hash(&mut h);
    b.preset.hash(&mut h);
    props.weight.to_bits().hash(&mut h);
    for p in props.profile().map(|p| p.points.as_slice()).unwrap_or_default() {
        (p.t.to_bits(), p.scale.to_bits(), p.bias.to_bits()).hash(&mut h);
    }
    (s.spacing.to_bits(), s.size_jitter.to_bits(), s.angle_jitter.to_bits(), s.scatter.to_bits(), s.seed).hash(&mut h);
    // The view's translation belongs in the key: two small previews at different page positions
    // share x0/y0/w/h, and without the coefficients the first mask is reused at the wrong place.
    let coeffs = xf.as_coeffs().map(|v| (v * 1000.0) as i64);
    (coeffs, area.x0, area.y0, area.w, area.h).hash(&mut h);
    h.finish()
}

/// The node-local bounds of the dabs and their clipped pixmap-space rectangle.
fn stroke_rect(dabs: &[Stamp], def: &BrushDef, xf: Affine, pix: &Pixmap) -> Option<(Rect, MaskArea)> {
    let aspect = def.tip.aspect().clamp(0.01, 100.0);
    let grow = aspect.max(1.0 / aspect).max(1.0) * 0.75 + f64::from(def.scatter.max(1.0));
    let mut box_local: Option<Rect> = None;
    for d in dabs {
        let r = d.size * grow;
        let rect = Rect::new(d.at.x - r, d.at.y - r, d.at.x + r, d.at.y + r);
        box_local = Some(box_local.map_or(rect, |b| b.union(rect)));
    }
    let local = box_local?;
    let corners = [Point::new(local.x0, local.y0), Point::new(local.x1, local.y0), Point::new(local.x1, local.y1), Point::new(local.x0, local.y1)];
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for c in corners {
        let q = xf * c;
        min_x = min_x.min(q.x);
        min_y = min_y.min(q.y);
        max_x = max_x.max(q.x);
        max_y = max_y.max(q.y);
    }
    if !(min_x.is_finite() && min_y.is_finite() && max_x.is_finite() && max_y.is_finite()) {
        return None;
    }
    let x0 = (min_x.floor() as i64 - 2).max(0) as i32;
    let y0 = (min_y.floor() as i64 - 2).max(0) as i32;
    let x1 = (max_x.ceil() as i64 + 2).min(i64::from(pix.width())) as i32;
    let y1 = (max_y.ceil() as i64 + 2).min(i64::from(pix.height())) as i32;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((local, MaskArea { x0, y0, w: (x1 - x0) as u32, h: (y1 - y0) as u32 }))
}

/// Stamp every dab into an 8-bit alpha plane of `area.w × area.h` device pixels at its origin.
fn build_mask(dabs: &[Stamp], def: &BrushDef, xf: Affine, scale: f64, area: &MaskArea) -> Vec<u8> {
    let mut alpha = vec![0u8; (area.w as usize).saturating_mul(area.h as usize)];
    let aspect = def.tip.aspect().clamp(0.01, 100.0);
    let roundness = f64::from(def.roundness.clamp(0.01, 1.0));
    let extra_angle = f64::from(def.angle).to_radians();
    for d in dabs {
        let c = xf * d.at;
        let cx = c.x - f64::from(area.x0);
        let cy = c.y - f64::from(area.y0);
        let size = (d.size * scale).max(1.0);
        let tw = (size * aspect).max(1.0);
        let th = (size * roundness).max(1.0);
        let angle = d.angle + extra_angle;
        let (ca, sa) = (angle.cos(), angle.sin());
        let radius = (tw * tw + th * th).sqrt() * 0.5 + 1.0;
        let px0 = ((cx - radius).floor() as i64).max(0) as u32;
        let py0 = ((cy - radius).floor() as i64).max(0) as u32;
        let px1 = ((cx + radius).ceil() as i64).min(i64::from(area.w)) as u32;
        let py1 = ((cy + radius).ceil() as i64).min(i64::from(area.h)) as u32;
        for py in py0..py1 {
            for px in px0..px1 {
                let (dx, dy) = (f64::from(px) + 0.5 - cx, f64::from(py) + 0.5 - cy);
                let (rx, ry) = (dx * ca + dy * sa, -dx * sa + dy * ca);
                let mut u = rx / tw + 0.5;
                let mut v = ry / th + 0.5;
                if def.flip_x {
                    u = 1.0 - u;
                }
                if def.flip_y {
                    v = 1.0 - v;
                }
                let ink = def.tip.at(u, v);
                if ink <= 0.0 {
                    continue;
                }
                let at = (py as usize * area.w as usize) + px as usize;
                if let Some(a) = alpha.get_mut(at) {
                    *a = (*a).max((ink * 255.0 + 0.5) as u8);
                }
            }
        }
    }
    alpha
}

/// Paint `n`'s brush stroke, or return `false` so the caller draws the plain geometric outline.
pub fn paint(ctx: &Ctx<'_, '_>, pix: &mut Pixmap, n: &Node, geom: &BezPath, xf: Affine, w: f64, h: f64) -> bool {
    let Some(b) = &n.stroke.brush else { return false };
    let Some(def) = figcraft_abr::resolve(&b.preset) else { return false };
    let scale = xf.determinant().abs().sqrt();
    if !(scale.is_finite() && scale > 0.0) {
        return false;
    }
    let s = settings(&def, b);
    let Some(dabs) = stamps(geom, n.stroke.weight, &n.stroke, &s) else { return false };
    if dabs.is_empty() {
        return true; // an empty path paints nothing, but never a plain band
    }
    let Some((local, area)) = stroke_rect(&dabs, &def, xf, pix) else { return false };
    if area.w == 0 || area.h == 0 || (area.w as usize).saturating_mul(area.h as usize) > MAX_MASK_PIXELS {
        return false;
    }
    let key = cache_key(geom, &def, b, &n.stroke, xf, &area, &s);
    let alpha = {
        let mut c = cache().lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(hit) = c.get(&key) {
            hit.clone()
        } else {
            let built = Arc::new(build_mask(&dabs, &def, xf, scale, &area));
            if c.len() >= MAX_CACHED_MASKS {
                c.clear();
            }
            c.insert(key, built.clone());
            built
        }
    };
    let Some(size) = IntSize::from_wh(area.w, area.h) else { return false };
    let Some(mask) = Mask::from_vec(alpha.as_ref().clone(), size) else { return false };
    let Some(mut layer) = Pixmap::new(area.w, area.h) else { return false };
    let local_xf = ts_transform(Affine::translate((-f64::from(area.x0), -f64::from(area.y0))) * xf);
    let Some(rect) = tiny_skia::Rect::from_xywh(local.x0 as f32, local.y0 as f32, local.width() as f32, local.height() as f32) else { return false };
    for p in &n.strokes {
        let Some(prep) = prepare_at(p, w, h, scale, ctx.doc, ctx.time, ctx.preview) else { continue };
        let paint = tiny_skia::Paint { shader: prep.shader(), anti_alias: false, blend_mode: tiny_skia::BlendMode::SourceOver, ..Default::default() };
        layer.fill_rect(rect, &paint, local_xf, Some(&mask));
        let blend = PixmapPaint { opacity: 1.0, blend_mode: ts_blend(p.blend), quality: FilterQuality::Bilinear };
        pix.draw_pixmap(area.x0, area.y0, layer.as_ref(), &blend, Transform::identity(), None);
    }
    true
}
