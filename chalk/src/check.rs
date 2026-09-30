use crate::pack::{self, PackMeta};
use crate::versions::{self, Minecraft};
use crate::{PackRoot, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// What a valid pack supports, for reporting and for choosing test targets.
pub struct Support {
    pub meta: PackMeta,
    pub minecraft: Vec<Minecraft>,
}

/// Checks `pack.mcmeta` against every game version it claims, parses every JSON file,
/// and returns the Chalk-tested Minecraft versions the pack loads on.
pub fn support(root: &PackRoot) -> Result<Support> {
    let pack_dir = root.pack_dir();
    let meta = pack::read(&pack_dir)?;
    check_json(&pack_dir)?;
    let minecraft: Vec<Minecraft> = versions::environments()?
        .minecraft
        .into_iter()
        .filter(|minecraft| meta.formats.contains(minecraft.data_format))
        .collect();
    if minecraft.is_empty() {
        return Err(format!(
            "pack.mcmeta covers formats {}, which matches no version Chalk tests",
            meta.formats
        )
        .into());
    }
    Ok(Support { meta, minecraft })
}

fn check_json(pack_dir: &Path) -> Result<()> {
    let mut errors = Vec::new();
    for path in files(pack_dir)? {
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let text = fs::read_to_string(&path)?;
            if let Err(error) = serde_json::from_str::<serde_json::Value>(&text) {
                errors.push(format!("{}: {error}", path.display()));
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
