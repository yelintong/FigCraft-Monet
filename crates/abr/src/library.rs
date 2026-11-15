//! The external brush library. Monet ships no brushes compiled into the engine: every brush is a
//! preset read from an `.abr` file in the user's brushes directory.
//!
//! The directory is `MONET_BRUSHES_DIR` when set, otherwise `<APPDATA>/Monet/brushes` on Windows
//! and `<HOME>/.local/share/monet/brushes` elsewhere. The desktop shell installs the factory
//! `monet-defaults.abr` into it on first run (see [`ensure_defaults`]); after that it is an ordinary
//! folder the user can fill by importing `.abr` files or by dropping them in directly.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use crate::format::parse;
use crate::mapping::{BrushDef, brushes_of};

/// The eight ids older Monet documents may contain. The factory `.abr` uses these as preset names,
/// so `resolve` keeps those documents painting instead of dropping their brush strokes.
pub const LEGACY_IDS: &[&str] = &["taper", "ink", "ribbon", "bristle", "dots", "stars", "confetti", "leaves"];

/// What the UI needs to list a brush (a small tip thumbnail and its labels).
#[derive(Clone, Debug)]
pub struct BrushInfo {
    pub id: String,
    pub name: String,
    pub group: String,
    pub folder: Vec<String>,
    pub diameter: f32,
    pub spacing: f32,
    /// 32 × 32 grayscale bytes, 255 = ink.
    pub thumbnail: Vec<u8>,
}

#[derive(Clone, Default)]
pub struct BrushLibrary {
    brushes: Vec<Arc<BrushDef>>,
    by_id: HashMap<String, usize>,
    warnings: Vec<String>,
}

impl BrushLibrary {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parse one `.abr` file and append its presets. `group` is usually the file's stem, the way
    /// Photoshop's Import Brushes puts them in a folder named after the file.
    pub fn add_bytes(&mut self, bytes: &[u8], group: &str) -> Result<usize, String> {
        let file = parse(bytes)?;
        let (defs, mut warnings) = brushes_of(&file, group);
        let n = defs.len();
        for b in defs {
            self.by_id.insert(b.id.clone(), self.brushes.len());
            self.brushes.push(Arc::new(b));
        }
        self.warnings.append(&mut warnings);
        Ok(n)
    }

    pub fn add_file(&mut self, path: &Path) -> Result<usize, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let group = path.file_stem().and_then(OsStr::to_str).unwrap_or("Brushes").to_string();
        self.add_bytes(&bytes, &group)
    }

    /// Look up a brush by its full id.
    pub fn find(&self, id: &str) -> Option<Arc<BrushDef>> {
        self.by_id.get(id).and_then(|&i| self.brushes.get(i)).cloned()
    }

    /// Look up a brush by full id, by one of the eight legacy preset ids, or by its name.
    /// A document saved before ABR support stays usable.
    pub fn resolve(&self, id_or_name: &str) -> Option<Arc<BrushDef>> {
        if let Some(b) = self.find(id_or_name) {
            return Some(b);
        }
        let legacy = LEGACY_IDS.contains(&id_or_name);
        self.brushes
            .iter()
            .find(|b| b.name.eq_ignore_ascii_case(id_or_name) && (!legacy || b.group.contains("monet-defaults")))
            .or_else(|| self.brushes.iter().find(|b| b.name.eq_ignore_ascii_case(id_or_name)))
            .cloned()
    }

    pub fn infos(&self) -> Vec<BrushInfo> {
        self.brushes
            .iter()
            .map(|b| BrushInfo {
                id: b.id.clone(),
                name: b.name.clone(),
                group: b.group.clone(),
                folder: b.folder.clone(),
                diameter: b.diameter,
                spacing: b.spacing,
                thumbnail: b.tip.thumbnail(32),
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.brushes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.brushes.is_empty()
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn first(&self) -> Option<Arc<BrushDef>> {
        self.brushes.first().cloned()
    }

    /// Parse every `.abr` in `dir`, in file-name order. A missing directory is an empty library.
    pub fn from_dir(dir: &Path) -> Self {
        let mut lib = Self::empty();
        let Ok(rd) = std::fs::read_dir(dir) else { return lib };
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(OsStr::to_str).is_some_and(|e| e.eq_ignore_ascii_case("abr")))
            .collect();
        files.sort();
        for p in files {
            if let Err(e) = lib.add_file(&p) {
                lib.warnings.push(format!("{}: {e}", p.display()));
            }
        }
        lib
    }
}

static GLOBAL: OnceLock<RwLock<Arc<BrushLibrary>>> = OnceLock::new();

/// The library every renderer and UI reads. Headless sessions and tests see an empty library
/// until something installs one.
pub fn global() -> &'static RwLock<Arc<BrushLibrary>> {
    GLOBAL.get_or_init(|| RwLock::new(Arc::new(BrushLibrary::empty())))
}

pub fn install(lib: BrushLibrary) {
    if let Ok(mut g) = global().write() {
        *g = Arc::new(lib);
    }
}

pub fn resolve(id: &str) -> Option<Arc<BrushDef>> {
    global().read().ok().and_then(|g| g.resolve(id))
}

pub fn infos() -> Vec<BrushInfo> {
    global().read().map(|g| g.infos()).unwrap_or_default()
}

pub fn warnings() -> Vec<String> {
    global().read().map(|g| g.warnings().to_vec()).unwrap_or_default()
}

/// Where external `.abr` files live.
pub fn brushes_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("MONET_BRUSHES_DIR") {
        return PathBuf::from(p);
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("Monet").join("brushes");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local").join("share").join("monet").join("brushes");
    }
    PathBuf::from("brushes")
}

fn has_abr(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().any(|e| e.path().extension().and_then(OsStr::to_str).is_some_and(|x| x.eq_ignore_ascii_case("abr"))))
        .unwrap_or(false)
}

/// On first run, put the factory `.abr` in the user's brushes directory (an ordinary external
/// file, never compiled into the binary) and load the directory. Returns the number of brushes.
pub fn ensure_defaults(default_bytes: &[u8]) -> Result<usize, String> {
    let dir = brushes_dir();
    if !has_abr(&dir) {
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join("monet-defaults.abr");
        std::fs::write(&path, default_bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    load_dir(&dir)
}

/// Rescan the brushes directory and install the result.
pub fn load_dir(dir: &Path) -> Result<usize, String> {
    let lib = BrushLibrary::from_dir(dir);
    let n = lib.len();
    let warnings = lib.warnings().to_vec();
    install(lib);
    for w in warnings {
        eprintln!("monet brushes: {w}");
    }
    Ok(n)
}

/// Import one `.abr` file: copy it into the brushes directory (keeping the original untouched),
/// then rescan. Returns the number of brushes in the library afterwards.
pub fn import(path: &Path) -> Result<usize, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = brushes_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return Err("the brush file has no name".to_string());
    };
    let mut dest = dir.join(name);
    if dest.exists() {
        let stem = path.file_stem().and_then(OsStr::to_str).unwrap_or("brushes");
        let ext = path.extension().and_then(OsStr::to_str).unwrap_or("abr");
        for i in 2..=100 {
            let candidate = dir.join(format!("{stem}-{i}.{ext}"));
            if !candidate.exists() {
                dest = candidate;
                break;
            }
            if i == 100 {
                return Err("too many imported copies of this brush file".to_string());
            }
        }
    }
    std::fs::write(&dest, &bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
    load_dir(&dir)
}

/// Desktop startup: load the user's brushes, installing the factory `.abr` on first run.
///
/// The factory file is read from disk (an asset next to the executable, or the repository's
/// `assets/brushes` in a development build) — never compiled into the binary. A headless session
/// never calls this, and a missing asset simply leaves the library empty.
pub fn bootstrap() -> Result<usize, String> {
    let dir = brushes_dir();
    if has_abr(&dir) {
        return load_dir(&dir);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("MONET_ASSETS_DIR") {
        candidates.push(PathBuf::from(p).join("brushes").join("monet-defaults.abr"));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        candidates.push(parent.join("assets").join("brushes").join("monet-defaults.abr"));
        candidates.push(parent.join("..").join("assets").join("brushes").join("monet-defaults.abr"));
    }
    candidates.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/brushes/monet-defaults.abr")));
    for p in candidates {
        if let Ok(bytes) = std::fs::read(&p) {
            return ensure_defaults(&bytes);
        }
    }
    load_dir(&dir)
}
