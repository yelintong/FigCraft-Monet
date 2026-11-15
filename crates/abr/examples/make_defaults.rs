//! Generate the factory `monet-defaults.abr`: the eight brush shapes Monet used to hard-code in
//! `crates/pathops/src/brush.rs`, rasterised into real ABR sampled tips.
//!
//! This is a build-time tool, not engine code. Its output is an ordinary external `.abr` file
//! (`assets/brushes/monet-defaults.abr`) that the desktop shell copies into the user's brushes
//! directory on first run. Nothing here is compiled into the editor.
//!
//! `cargo run -p figcraft-abr --example make_defaults -- [output.abr]`

use std::f64::consts::PI;

use figcraft_abr::{AbrSample, LegacyBrush, LegacyTip, write_v12};

const NAMES: [&str; 8] = ["taper", "ink", "ribbon", "bristle", "dots", "stars", "confetti", "leaves"];
const EDGE: u32 = 96;

/// The shape as it was drawn in `pathops::brush::shape()`, now evaluated per sample in -0.5..0.5.
fn inside(kind: usize, x: f64, y: f64) -> bool {
    let u = x + 0.5; // stretch shapes span x 0..1 along the path
    match kind {
        // taper: a lens, blunt at both ends.
        0 => u >= 0.0 && u <= 1.0 && y.abs() <= 0.5 * (PI * u).sin().max(0.0).powf(0.8),
        // ink: a blunt start, uneven edges and a long tail.
        1 => {
            if !(0.0..=1.0).contains(&u) {
                return false;
            }
            let w = (u / 0.04).min(1.0) * ((1.0 - u) / 0.3).min(1.0);
            let top = -0.5 * w + 0.04 * (9.0 * PI * u).sin() * w;
            let bottom = 0.45 * w + 0.04 * (13.0 * PI * u + 1.0).sin() * w;
            y >= top.min(bottom) && y <= top.max(bottom)
        }
        // ribbon: edges that cross three times.
        2 => u >= 0.0 && u <= 1.0 && y.abs() <= (0.5 * (1.5 * PI * u).cos()).abs(),
        // bristle: four thin strands with ragged ends.
        3 => {
            if !(0.0..=1.0).contains(&u) {
                return false;
            }
            (0..4).any(|k| {
                let (kf, yk) = (k as f64, -0.375 + 0.25 * k as f64);
                let (x0, x1) = (0.03 * kf, 1.0 - 0.04 * (3.0 - kf));
                let t = ((u - x0) / (x1 - x0)).clamp(0.0, 1.0);
                let h = 0.09 * (PI * t).sin().max(0.0).sqrt();
                (y - yk).abs() <= h
            })
        }
        // dots: a circle one unit wide.
        4 => x * x + y * y <= 0.25,
        // stars: a five-point star.
        5 => star_inside(x, y),
        // confetti: a small square.
        6 => x.abs() <= 0.3 && y.abs() <= 0.3,
        // leaves: a lens centred on the origin.
        _ => {
            let t = 1.0 - (x / 0.5).powi(2);
            t >= 0.0 && y.abs() <= 0.3 * t.sqrt()
        }
    }
}

fn star_inside(x: f64, y: f64) -> bool {
    let pts: Vec<(f64, f64)> = (0..10)
        .map(|i| {
            let a = PI / 2.0 + i as f64 * PI / 5.0;
            let r = if i % 2 == 0 { 0.5 } else { 0.2 };
            (r * a.cos(), r * a.sin())
        })
        .collect();
    let mut inside = false;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let (xi, yi) = pts[i];
        let (xj, yj) = pts[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// 4×4 supersampled coverage, 255 = ink.
fn raster(kind: usize) -> Vec<u8> {
    const SS: u32 = 4;
    let mut out = Vec::with_capacity((EDGE * EDGE) as usize);
    for py in 0..EDGE {
        for px in 0..EDGE {
            let mut hits = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (f64::from(px) + (f64::from(sx) + 0.5) / f64::from(SS)) / f64::from(EDGE) - 0.5;
                    let y = (f64::from(py) + (f64::from(sy) + 0.5) / f64::from(SS)) / f64::from(EDGE) - 0.5;
                    if inside(kind, x, y) {
                        hits += 1;
                    }
                }
            }
            out.push((hits * 255 / (SS * SS)) as u8);
        }
    }
    out
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "assets/brushes/monet-defaults.abr".to_string());
    let brushes: Vec<LegacyBrush> = NAMES
        .iter()
        .enumerate()
        .map(|(i, name)| LegacyBrush {
            name: (*name).to_string(),
            spacing: 20,
            anti_alias: true,
            tip: LegacyTip::Sampled(AbrSample { id: String::new(), width: EDGE, height: EDGE, depth: 8, data: raster(i) }),
        })
        .collect();
    let bytes = write_v12(2, &brushes, true).expect("the factory brush file can be written");
    if let Some(parent) = std::path::Path::new(&out).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).expect("create the output directory");
    }
    std::fs::write(&out, &bytes).expect("write the factory brush file");
    println!("{out}: {} brushes, {} bytes", brushes.len(), bytes.len());
}
