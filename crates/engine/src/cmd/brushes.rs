//! External `.abr` brush library commands. Every surface (UI, CLI, control channel, MCP) uses
//! these instead of talking to the filesystem directly.

use serde_json::{Value, json};
use std::path::Path;

use super::CommandSpec;
use crate::{Result, Session, bad, spec};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "brush.list",
            "List brushes",
            "{}  → every brush installed from external .abr files (id, name, group, folder, diameter, spacing) and any import warnings",
            false,
            list
        ),
        spec!(
            "brush.import",
            "Import brushes",
            "{path (an .abr file)}  copies the file into the brushes directory and reloads the library → {brushes}",
            false,
            import
        ),
        spec!("brush.reload", "Reload brushes", "{}  rescans the brushes directory → {brushes}", false, reload),
    ]
}

fn list(_: &mut Session, _: &Value) -> Result<Value> {
    let brushes: Vec<Value> = figcraft_abr::infos()
        .iter()
        .map(|b| json!({"id": b.id, "name": b.name, "group": b.group, "folder": b.folder, "diameter": b.diameter, "spacing": b.spacing}))
        .collect();
    Ok(json!({"brushes": brushes, "warnings": figcraft_abr::warnings()}))
}

fn import(_: &mut Session, p: &Value) -> Result<Value> {
    let path = p.get("path").and_then(Value::as_str).ok_or_else(|| bad("brush.import", "path (an .abr file) is required"))?;
    let n = figcraft_abr::import(Path::new(path)).map_err(|e| bad("brush.import", e))?;
    Ok(json!({"brushes": n}))
}

fn reload(_: &mut Session, _: &Value) -> Result<Value> {
    let n = figcraft_abr::load_dir(&figcraft_abr::brushes_dir()).map_err(|e| bad("brush.reload", e))?;
    Ok(json!({"brushes": n}))
}
