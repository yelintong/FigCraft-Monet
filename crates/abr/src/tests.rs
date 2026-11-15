//! Round trips through both ABR layouts, damage handling and library behaviour.

use crate::descriptor::{Descriptor, Value};
use crate::format::{AbrSample, LegacyBrush, LegacyTip, parse, write_v12, write_v6};
use crate::library::{BrushLibrary, LEGACY_IDS};
use crate::mapping::{GrayTip, brushes_of};

fn sample(id: &str, w: u32, h: u32, data: Vec<u8>) -> AbrSample {
    AbrSample { id: id.to_string(), width: w, height: h, depth: 8, data }
}

#[test]
fn v12_round_trips_raw_and_packbits() {
    let tip = sample("", 8, 4, (0..32).map(|i| (i * 7) as u8).collect());
    let brushes = vec![
        LegacyBrush { name: "Ink".into(), spacing: 20, anti_alias: true, tip: LegacyTip::Sampled(tip.clone()) },
        LegacyBrush {
            name: String::new(),
            spacing: 0,
            anti_alias: true,
            tip: LegacyTip::Computed { diameter: 24, hardness: 80, angle: -15, roundness: 70 },
        },
    ];
    for rle in [false, true] {
        let bytes = write_v12(2, &brushes, rle).unwrap();
        let f = parse(&bytes).unwrap();
        assert_eq!(f.version, 2);
        assert_eq!(f.legacy.len(), 2, "rle={rle}");
        assert_eq!(f.legacy[0].name, "Ink");
        assert_eq!(f.legacy[0].spacing, 20);
        match &f.legacy[0].tip {
            LegacyTip::Sampled(s) => assert_eq!(s.data, tip.data, "rle={rle}"),
            other => panic!("expected a sampled tip, got {other:?}"),
        }
        match &f.legacy[1].tip {
            LegacyTip::Computed { diameter, hardness, angle, roundness } => assert_eq!((*diameter, *hardness, *angle, *roundness), (24, 80, -15, 70)),
            other => panic!("expected a computed tip, got {other:?}"),
        }
    }
}

fn preset(name: &str, id: &str, spacing: f64) -> Descriptor {
    Descriptor::new("brushPreset")
        .with("Nm  ", Value::Text(name.to_string()))
        .with(
            "Brsh",
            Value::Descriptor(
                Descriptor::new("sampledBrush")
                    .with("Dmtr", Value::UnitFloat { unit: *b"#Pxl", value: 24.0 })
                    .with("Spcn", Value::UnitFloat { unit: *b"#Prc", value: spacing })
                    .with("sampledData", Value::Text(id.to_string())),
            ),
        )
        .with("useTipDynamics", Value::Boolean(true))
        .with("szVr", Value::Descriptor(Descriptor::new("brVr").with("jitter", Value::UnitFloat { unit: *b"#Prc", value: 40.0 })))
}

#[test]
fn v6_round_trips_samples_presets_and_folders() {
    let s = sample("tip-1", 4, 4, vec![255; 16]);
    let p = preset("Chalk", "tip-1", 15.0);
    let bytes = write_v6(1, &[s], &[p], &[vec!["Dry".to_string()]], false).unwrap();
    let f = parse(&bytes).unwrap();
    assert_eq!(f.version, 6);
    assert_eq!(f.subversion, 1);
    assert_eq!(f.samples.len(), 1);
    assert_eq!(f.samples[0].id, "tip-1");
    assert_eq!(f.samples[0].data, vec![255; 16]);
    assert_eq!(f.presets.len(), 1);
    assert_eq!(f.folders, vec![vec!["Dry".to_string()]]);

    let (defs, warnings) = brushes_of(&f, "chalk-pack.abr");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].name, "Chalk");
    assert!((defs[0].spacing - 0.15).abs() < 1e-6);
    assert!((defs[0].size_jitter - 0.4).abs() < 1e-6);
    assert!((defs[0].diameter - 24.0).abs() < 1e-6);
    assert_eq!(defs[0].folder, vec!["Dry".to_string()]);
}

#[test]
fn v6_rle_tips_decode() {
    let s = sample("rle-tip", 16, 8, (0..128).map(|i| (i % 5) as u8 * 51).collect());
    let p = preset("RLE", "rle-tip", 25.0);
    let bytes = write_v6(2, &[s.clone()], &[p], &[], true).unwrap();
    let f = parse(&bytes).unwrap();
    assert_eq!(f.samples.len(), 1);
    assert_eq!(f.samples[0].data, s.data);
}

#[test]
fn malformed_files_return_errors_without_panicking() {
    for bytes in [
        vec![],
        vec![0],
        vec![0, 99],
        vec![0, 1, 0, 5],
        vec![0, 2, 0, 1, 0, 2, 0, 0, 0, 100],
        vec![0, 6, 0, 3],
        vec![0, 6, 0, 1, b'8', b'B', b'I', b'M', b's', b'a', b'm', b'p', 0, 0, 0, 250],
        vec![0xff; 256],
    ] {
        let _ = parse(&bytes);
    }
}

#[test]
fn inverted_ink_polarity_is_detected() {
    // Some writers store ink as black: a white border with a black centre.
    let mut data = vec![255u8; 16 * 16];
    for y in 4..12 {
        for x in 4..12 {
            data[y * 16 + x] = 0;
        }
    }
    let tip = GrayTip::from_sample(&sample("", 16, 16, data));
    assert!(tip.at(0.5, 0.5) > 0.9, "the centre should be ink");
    assert!(tip.at(0.05, 0.05) < 0.1, "the border should be clear");
}

#[test]
fn computed_tip_is_a_soft_ellipse() {
    let tip = GrayTip::computed(64, 50, 0, 100);
    assert_eq!(tip.width, 64);
    assert!(tip.at(0.5, 0.5) > 0.9);
    assert!(tip.at(0.02, 0.02) < 0.1);
    assert!(!tip.thumbnail(16).iter().all(|&a| a == 0));
}

#[test]
fn library_loads_external_files_and_resolves_legacy_ids() {
    let brushes: Vec<LegacyBrush> = LEGACY_IDS
        .iter()
        .map(|name| LegacyBrush {
            name: (*name).to_string(),
            spacing: 20,
            anti_alias: true,
            tip: LegacyTip::Sampled(sample("", 2, 2, vec![255; 4])),
        })
        .collect();
    let bytes = write_v12(2, &brushes, false).unwrap();
    let mut lib = BrushLibrary::empty();
    assert_eq!(lib.add_bytes(&bytes, "monet-defaults").unwrap(), 8);
    assert_eq!(lib.len(), 8);
    assert!(lib.resolve("ink").is_some(), "an old document's `ink` preset still paints");
    assert!(lib.resolve("taper").is_some());
    let ink = lib.resolve("ink").unwrap();
    assert!(lib.find(&ink.id).is_some(), "the full ABR id resolves too");
    assert_eq!(lib.infos().len(), 8);
}

/// The factory brushes are an ordinary file on disk, not data compiled into the engine.
#[test]
fn factory_defaults_is_a_real_external_abr_file() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/brushes/monet-defaults.abr");
    let bytes = std::fs::read(path).unwrap();
    let f = parse(&bytes).unwrap();
    assert_eq!(f.version, 2);
    assert_eq!(f.legacy.len(), 8);
    let (defs, warnings) = brushes_of(&f, "monet-defaults");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(defs.len(), 8);
    for b in &defs {
        assert!(b.tip.alpha.iter().any(|&a| a > 0), "{} has ink", b.name);
    }
    let mut lib = BrushLibrary::empty();
    lib.add_bytes(&bytes, "monet-defaults").unwrap();
    for id in LEGACY_IDS {
        assert!(lib.resolve(id).is_some(), "legacy id {id} must keep painting");
    }
}
