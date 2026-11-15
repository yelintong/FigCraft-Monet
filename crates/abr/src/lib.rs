//! Photoshop `.abr` brush files for Monet, plus the external brush library.
//!
//! The reader covers the legacy v1/v2 layout and the sectioned v6+ layout (8BIM sections,
//! PackBits-compressed grayscale tips, action descriptors and preset folders). It is implemented
//! from the published layout, with `storytold/photocraft`'s ABR import pipeline as the reference
//! for which preset settings matter. Nothing is copied from Photoshop's own code or assets.
//!
//! Monet paints with the tip bitmaps this crate decodes: see `figcraft-render`'s brush-stroke
//! compositor. The engine no longer contains any built-in brush shapes.

mod io;
pub mod descriptor;
pub mod format;
pub mod library;
pub mod mapping;

pub use descriptor::{Descriptor, Id, Value};
pub use format::{AbrFile, AbrSample, LegacyBrush, LegacyTip, parse, write_v12, write_v6};
pub use library::{BrushInfo, BrushLibrary, LEGACY_IDS, bootstrap, brushes_dir, ensure_defaults, global, import, infos, install, load_dir, resolve, warnings};
pub use mapping::{BrushDef, GrayTip, brushes_of};

#[cfg(test)]
mod tests;
