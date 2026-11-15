//! ABR brush strokes: dabs are spaced by the tip's diameter, follow the width profile, jitter
//! repeatably, and stay within the stroke's reach. The plain outline is only a geometric stand-in.

use figcraft_doc::{StrokeProps, WidthProfile};
use figcraft_geom::kurbo::{BezPath, Shape};

use crate::brush::{MAX_STAMPS, StampSettings, fallback_outline, stamps};

fn line(len: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to((0.0, 0.0));
    p.line_to((len, 0.0));
    p
}

#[test]
fn dabs_are_spaced_by_a_fraction_of_the_painted_diameter() {
    let s = StampSettings { spacing: 0.25, ..Default::default() };
    let dabs = stamps(&line(100.0), 10.0, &StrokeProps::default(), &s).unwrap();
    assert!(dabs.len() > 30, "{} dabs along 100 units", dabs.len());
    assert_eq!(dabs[0].at.x, 0.0);
    assert!((dabs[1].at.x - dabs[0].at.x - 2.5).abs() < 1e-6, "spacing = 25% of 10");
    assert!((dabs[0].size - 10.0).abs() < 1e-6);
}

#[test]
fn a_width_profile_scales_every_dab() {
    let props = StrokeProps { width_profile: Some(WidthProfile::new([(0.0, 0.2), (1.0, 2.0)])), ..Default::default() };
    let s = StampSettings { spacing: 0.25, ..Default::default() };
    let dabs = stamps(&line(200.0), 10.0, &props, &s).unwrap();
    assert!(dabs.len() > 2);
    assert!(dabs.last().unwrap().size > dabs[0].size * 3.0, "the tip grows along the path");
}

#[test]
fn jitter_is_repeatable_and_seed_dependent() {
    let s = StampSettings { spacing: 0.1, size_jitter: 1.0, angle_jitter: 1.0, scatter: 2.0, seed: 7 };
    let props = StrokeProps::default();
    let a = stamps(&line(100.0), 10.0, &props, &s).unwrap();
    let b = stamps(&line(100.0), 10.0, &props, &s).unwrap();
    assert_eq!(a, b, "the same seed replays the same stroke");
    let c = stamps(&line(100.0), 10.0, &props, &StampSettings { seed: 8, ..s }).unwrap();
    assert_ne!(a, c, "a different seed changes the jitter");
}

#[test]
fn strays_stay_near_the_path_and_the_dab_count_is_capped() {
    let s = StampSettings { spacing: 0.0001, size_jitter: 1.0, scatter: 4.0, ..Default::default() };
    let props = StrokeProps::default();
    let dabs = stamps(&line(10_000.0), 10.0, &props, &s).unwrap();
    assert!(dabs.len() <= MAX_STAMPS);
    for d in &dabs {
        assert!(d.at.y.abs() <= 40.0 + 1e-6, "scatter is at most 4 stroke weights");
        assert!(d.size.is_finite() && d.size > 0.0);
    }
}

#[test]
fn the_geometric_stand_in_is_a_plain_stroke() {
    let p = fallback_outline(&line(100.0), 10.0, &StrokeProps::default());
    let bb = p.bounding_box();
    assert!((bb.y0 + 5.0).abs() < 1e-6 && (bb.y1 - 5.0).abs() < 1e-6, "a 10-unit centred band: {bb:?}");
}
