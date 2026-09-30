use crate::source::{self, Pack};
use crate::versions::{Minecraft, Versions, versions};
use crate::{PackRoot, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// A valid pack and the Chalk-tested Minecraft versions it supports.
pub struct Support {
    pub versions: Versions,
    pub pack: Pack,
    pub minecraft: Vec<Minecraft>,
}

/// Reads the pack's sources, parses every JSON file, and returns what it supports.
pub fn support(root: &PackRoot) -> Result<Support> {
    let versions = versions()?;
    let pack = source::load(root, &versions)?;
    check_json(&pack)?;
    let minecraft: Vec<Minecraft> = versions
        .minecraft
        .iter()
        .filter(|minecraft| pack.formats.contains(minecraft.data_format))
        .cloned()
        .collect();
    if minecraft.is_empty() {
        return Err(format!(
            "Minecraft {} includes no version Chalk tests",
            pack.minecraft
        )
        .into());
    }
    Ok(Support {
        versions,
        pack,
        minecraft,
    })
}

fn check_json(pack: &Pack) -> Result<()> {
    let mut errors = Vec::new();
    let all = pack
        .files
        .iter()
        .chain(pack.overlays.iter().flat_map(|overlay| &overlay.files));
    for file in all {
        if file.path.ends_with(".json") {
            let text = fs::read_to_string(&file.source)?;
            if let Err(error) = serde_json::from_str::<serde_json::Value>(&text) {
                errors.push(format!("{}: {error}", file.source.display()));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n").into())
    }
}

/// Every file under `dir`, sorted so archives and reports are stable.
pub fn files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}
