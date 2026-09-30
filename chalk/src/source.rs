//! Reads a pack's sources: `chalk.toml` and the `datapack/` tree, where a file named
//! `frame@-26.2.json` replaces `frame.json` on Minecraft 26.2 and older.

use crate::check::files;
use crate::pack::{FormatRange, PackFormat};
use crate::versions::{VersionRange, Versions};
use crate::{PackRoot, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    description: String,
    minecraft: String,
}

/// A pack as Chalk builds it: files for every version plus generated overlays.
pub struct Pack {
    pub description: String,
    pub minecraft: VersionRange,
    pub formats: FormatRange,
    pub files: Vec<PackFile>,
    pub overlays: Vec<Overlay>,
}

pub struct PackFile {
    pub source: PathBuf,
    /// Path inside the built pack, with `/` separators.
    pub path: String,
}

/// Variant files that share one version range, built into one overlay directory.
pub struct Overlay {
    pub directory: String,
    pub formats: FormatRange,
    pub files: Vec<PackFile>,
}

impl Pack {
    /// The files a game with `format` loads: applicable overlays replace base files that
    /// share their path.
    pub fn files_for(&self, format: PackFormat) -> Vec<&PackFile> {
        let mut files: BTreeMap<&str, &PackFile> = self
            .files
            .iter()
            .map(|file| (file.path.as_str(), file))
            .collect();
        for overlay in self
            .overlays
            .iter()
            .filter(|overlay| overlay.formats.contains(format))
        {
            for file in &overlay.files {
                files.insert(file.path.as_str(), file);
            }
        }
        files.into_values().collect()
    }

    /// Namespaces the pack defines, for spotting its errors in game logs.
    pub fn namespaces(&self) -> Vec<String> {
        let mut namespaces: Vec<String> = self
            .files
            .iter()
            .chain(self.overlays.iter().flat_map(|overlay| &overlay.files))
            .filter_map(|file| {
                file.path
                    .strip_prefix("data/")?
                    .split('/')
                    .next()
                    .map(str::to_owned)
            })
            .filter(|namespace| namespace != "minecraft")
            .collect();
        namespaces.sort();
        namespaces.dedup();
        namespaces
    }
}

pub fn load(root: &PackRoot, versions: &Versions) -> Result<Pack> {
    let manifest_path = root.manifest();
    let text = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    let manifest: Manifest =
        toml::from_str(&text).map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    let minecraft = VersionRange::parse(&manifest.minecraft)?;
    let formats = versions
        .resolve(&minecraft, None)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;

    let pack_dir = root.pack_dir();
    let mut files = Vec::new();
    let mut variants: Vec<(FormatRange, PackFile)> = Vec::new();
    for source in files_in(&pack_dir)? {
        let relative = source
            .strip_prefix(&pack_dir)
            .map_err(|_| format!("{} is outside the pack", source.display()))?;
        let parts = path_parts(relative)?;
        if parts == ["pack.mcmeta"] {
            return Err(format!(
                "{} is generated from chalk.toml; move its description there and delete it",
                source.display()
            )
            .into());
        }
        let (directories, name) = parts.split_at(parts.len() - 1);
        if directories.iter().any(|directory| directory.contains('@')) {
            return Err(format!(
                "{}: only file names can name Minecraft versions",
                source.display()
            )
            .into());
        }
        match variant(&name[0]).map_err(|error| format!("{}: {error}", source.display()))? {
            None => files.push(PackFile {
                source,
                path: parts.join("/"),
            }),
            Some((file_name, range)) => {
                let range = versions
                    .resolve(&range, Some(formats))
                    .map_err(|error| format!("{}: {error}", source.display()))?;
                let path = directories
                    .iter()
                    .cloned()
                    .chain([file_name])
                    .collect::<Vec<_>>()
                    .join("/");
                variants.push((range, PackFile { source, path }));
            }
        }
    }
    check_overlaps(&variants)?;
    Ok(Pack {
        description: manifest.description,
        minecraft,
        formats,
        files,
        overlays: overlays(variants, versions),
    })
}

fn files_in(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Err(format!("{} does not exist", dir.display()).into());
    }
    files(dir)
}

fn path_parts(relative: &Path) -> Result<Vec<String>> {
    relative
        .components()
        .map(|component| match component {
            Component::Normal(part) => part
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{} is not valid UTF-8", relative.display()).into()),
            _ => Err(format!("{} is not a plain path", relative.display()).into()),
        })
        .collect()
}

/// Splits `frame@-26.2.json` into `frame.json` and its version range.
fn variant(name: &str) -> Result<Option<(String, VersionRange)>> {
    let Some((stem, rest)) = name.split_once('@') else {
        return Ok(None);
    };
    let (range, extension) = rest
        .rsplit_once('.')
        .ok_or_else(|| format!("{name} needs a file extension after its versions"))?;
    if stem.is_empty() || rest.contains('@') {
        return Err(format!("{name} should look like frame@-26.2.json").into());
    }
    Ok(Some((
        format!("{stem}.{extension}"),
        VersionRange::parse(range)?,
    )))
}

fn check_overlaps(variants: &[(FormatRange, PackFile)]) -> Result<()> {
    for (index, (range, file)) in variants.iter().enumerate() {
        for (other_range, other) in &variants[index + 1..] {
            if file.path == other.path
                && range.min <= other_range.max
                && other_range.min <= range.max
            {
                return Err(format!(
                    "{} and {} both apply to some of the same Minecraft versions",
                    file.source.display(),
                    other.source.display()
                )
                .into());
            }
        }
    }
    Ok(())
}

fn overlays(variants: Vec<(FormatRange, PackFile)>, versions: &Versions) -> Vec<Overlay> {
    let mut grouped: BTreeMap<(PackFormat, PackFormat), Vec<PackFile>> = BTreeMap::new();
    for (range, file) in variants {
        grouped
            .entry((range.min, range.max))
            .or_default()
            .push(file);
    }
    let mut overlays: Vec<Overlay> = Vec::new();
    for ((min, max), files) in grouped {
        let formats = FormatRange { min, max };
        let mut directory = match versions.span(formats) {
            Some((oldest, newest)) if oldest == newest => oldest.to_owned(),
            Some((oldest, newest)) => format!("{oldest}-{newest}"),
            None => format!("formats-{}-{}", formats.min.major, formats.max.major),
        };
        if overlays
            .iter()
            .any(|overlay| overlay.directory == directory)
        {
            directory = format!("formats-{}-{}", formats.min.major, formats.max.major);
        }
        overlays.push(Overlay {
            directory,
            formats,
            files,
        });
    }
    overlays
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::versions::versions;

    fn pack(files: &[&str], minecraft: &str) -> Result<Pack> {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("my-pack");
        fs::create_dir_all(repo.join("datapack")).expect("pack dir");
        fs::write(
            repo.join("chalk.toml"),
            format!("description = \"Test\"\nminecraft = \"{minecraft}\"\n"),
        )
        .expect("manifest");
        for file in files {
            let path = repo.join("datapack").join(file);
            fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            fs::write(path, "{}").expect("file");
        }
        load(
            &PackRoot::at(&repo).expect("root"),
            &versions().expect("versions"),
        )
    }

    #[test]
    fn a_variant_replaces_its_file_on_the_versions_it_names() {
        let pack = pack(
            &[
                "data/demo/tags/block/frame.json",
                "data/demo/tags/block/frame@-26.2.json",
                "data/demo/advancement/light@-26.2.json",
            ],
            "1.21.1-26.3",
        )
        .expect("pack");

        assert_eq!(pack.files.len(), 1);
        assert_eq!(pack.overlays.len(), 1);
        let overlay = &pack.overlays[0];
        assert_eq!(overlay.directory, "1.21-26.2");
        let paths: Vec<&str> = overlay
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                "data/demo/advancement/light.json",
                "data/demo/tags/block/frame.json"
            ]
        );
    }

    #[test]
    fn overlapping_variants_of_one_file_are_rejected() {
        let result = pack(
            &[
                "data/demo/function/a@-26.2.mcfunction",
                "data/demo/function/a@26.1.2-.mcfunction",
            ],
            "1.21.1-26.3",
        );
        assert!(result.is_err());
    }

    #[test]
    fn a_hand_written_pack_mcmeta_is_rejected() {
        assert!(pack(&["pack.mcmeta"], "1.21.1-26.3").is_err());
    }
}
