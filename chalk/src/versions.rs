use crate::Result;
use crate::pack::{FormatRange, PackFormat};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;

const ENVIRONMENTS: &str = include_str!("environments.toml");

/// Minecraft releases Chalk knows, and the ones it can run tests on.
pub struct Versions {
    pub fabric_loader: String,
    /// Every release a pack can name, oldest first.
    pub releases: Vec<Release>,
    /// The releases Chalk tests on, newest first.
    pub minecraft: Vec<Minecraft>,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub data_format: PackFormat,
}

/// A release Chalk tests on and the game it runs as.
#[derive(Clone)]
pub struct Minecraft {
    pub version: String,
    pub data_format: PackFormat,
    /// The `teakit-fabric` version built for this Minecraft version.
    pub teakit: String,
    /// Modrinth `project:version` pairs installed on both sides.
    pub mods: Vec<String>,
}

#[derive(Deserialize)]
struct File {
    fabric_loader: String,
    releases: BTreeMap<String, (u32, u32)>,
    minecraft: Vec<Environment>,
}

#[derive(Deserialize)]
struct Environment {
    version: String,
    teakit: String,
    mods: Vec<String>,
}

pub fn versions() -> Result<Versions> {
    let file: File = toml::from_str(ENVIRONMENTS)
        .map_err(|error| format!("invalid built-in environments.toml: {error}"))?;
    let mut releases: Vec<Release> = file
        .releases
        .into_iter()
        .map(|(version, (major, minor))| Release {
            version,
            data_format: PackFormat { major, minor },
        })
        .collect();
    // Formats only grow, so they order releases without parsing version numbers.
    releases.sort_by_key(|release| release.data_format);
    let mut minecraft = Vec::new();
    for environment in file.minecraft {
        let data_format = releases
            .iter()
            .find(|release| release.version == environment.version)
            .map(|release| release.data_format)
            .ok_or_else(|| {
                format!(
                    "environments.toml has no release entry for {}",
                    environment.version
                )
            })?;
        minecraft.push(Minecraft {
            version: environment.version,
            data_format,
            teakit: environment.teakit,
            mods: environment.mods,
        });
    }
    Ok(Versions {
        fabric_loader: file.fabric_loader,
        releases,
        minecraft,
    })
}

impl Versions {
    fn release(&self, version: &str) -> Result<&Release> {
        self.releases.iter().find(|release| release.version == version).ok_or_else(|| {
            let oldest = self.releases.first().map(|release| release.version.as_str()).unwrap_or_default();
            let newest = self.releases.last().map(|release| release.version.as_str()).unwrap_or_default();
            format!("Chalk doesn't know Minecraft {version}; name a release from {oldest} to {newest}").into()
        })
    }

    /// The oldest and newest releases whose formats fall inside `range`, for naming it.
    pub fn span(&self, range: FormatRange) -> Option<(&str, &str)> {
        let inside = || {
            self.releases
                .iter()
                .filter(move |release| range.contains(release.data_format))
        };
        Some((
            inside().next()?.version.as_str(),
            inside().next_back()?.version.as_str(),
        ))
    }

    /// Resolves a range of releases to the data formats it covers. Bounds are whole major
    /// formats, so a range also covers the snapshots and minor updates between releases.
    /// Open ends take the pack's own range as `bounds`.
    pub fn resolve(
        &self,
        range: &VersionRange,
        bounds: Option<FormatRange>,
    ) -> Result<FormatRange> {
        let min = match &range.from {
            Some(version) => PackFormat {
                major: self.release(version)?.data_format.major,
                minor: 0,
            },
            None => bounds.ok_or("the pack's own range needs both ends")?.min,
        };
        let max = match &range.to {
            Some(version) => PackFormat {
                major: self.release(version)?.data_format.major,
                minor: u32::MAX,
            },
            None => bounds.ok_or("the pack's own range needs both ends")?.max,
        };
        if min > max {
            return Err(format!("{range} starts after it ends").into());
        }
        if let Some(bounds) = bounds
            && !(bounds.contains(min) && bounds.contains(max))
        {
            return Err(format!("{range} reaches outside the pack's Minecraft versions").into());
        }
        Ok(FormatRange { min, max })
    }
}

/// Releases written as `A-B`, `A`, `-B` (up to B), or `A-` (A and newer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionRange {
    pub from: Option<String>,
    pub to: Option<String>,
}

impl VersionRange {
    pub fn parse(text: &str) -> Result<Self> {
        let version = |part: &str| (!part.is_empty()).then(|| part.to_owned());
        let range = match text.split_once('-') {
            Some((from, to)) => Self {
                from: version(from),
                to: version(to),
            },
            None => Self {
                from: version(text),
                to: version(text),
            },
        };
        let valid = |part: &Option<String>| {
            part.as_deref()
                .is_none_or(|version| version.chars().all(|c| c.is_ascii_digit() || c == '.'))
        };
        if (range.from.is_none() && range.to.is_none()) || !valid(&range.from) || !valid(&range.to)
        {
            return Err(format!(
                "{text} is not a Minecraft version range; use 1.21.1-26.2, 26.2, -26.2, or 26.2-"
            )
            .into());
        }
        Ok(range)
    }
}

impl fmt::Display for VersionRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.from, &self.to) {
            (Some(from), Some(to)) if from == to => write!(f, "{from}"),
            (from, to) => write!(
                f,
                "{}-{}",
                from.as_deref().unwrap_or(""),
                to.as_deref().unwrap_or("")
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formats(min: (u32, u32), max: (u32, u32)) -> FormatRange {
        FormatRange {
            min: PackFormat {
                major: min.0,
                minor: min.1,
            },
            max: PackFormat {
                major: max.0,
                minor: max.1,
            },
        }
    }

    #[test]
    fn tested_versions_are_newest_first() {
        let versions = versions().expect("environments.toml parses");
        let formats: Vec<PackFormat> = versions
            .minecraft
            .iter()
            .map(|minecraft| minecraft.data_format)
            .collect();
        let mut sorted = formats.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(formats, sorted);
    }

    #[test]
    fn ranges_cover_whole_major_formats() {
        let versions = versions().expect("versions");
        let range = VersionRange::parse("1.21.1-26.3").expect("range");
        assert_eq!(
            versions.resolve(&range, None).expect("resolves"),
            formats((48, 0), (121, u32::MAX))
        );
    }

    #[test]
    fn open_ends_take_the_pack_bounds() {
        let versions = versions().expect("versions");
        let pack = formats((48, 0), (121, u32::MAX));
        let up_to = VersionRange::parse("-26.2").expect("range");
        assert_eq!(
            versions.resolve(&up_to, Some(pack)).expect("resolves"),
            formats((48, 0), (107, u32::MAX))
        );
        let from = VersionRange::parse("26.2-").expect("range");
        assert_eq!(
            versions.resolve(&from, Some(pack)).expect("resolves"),
            formats((107, 0), (121, u32::MAX))
        );
    }

    #[test]
    fn a_variant_cannot_reach_outside_the_pack() {
        let versions = versions().expect("versions");
        let pack = formats((94, 0), (121, u32::MAX));
        let range = VersionRange::parse("1.21.1-26.2").expect("range");
        assert!(versions.resolve(&range, Some(pack)).is_err());
    }

    #[test]
    fn unknown_releases_and_malformed_ranges_are_rejected() {
        let versions = versions().expect("versions");
        assert!(
            versions
                .resolve(&VersionRange::parse("1.20.1-26.3").expect("range"), None)
                .is_err()
        );
        assert!(VersionRange::parse("-").is_err());
        assert!(VersionRange::parse("latest").is_err());
    }
}
