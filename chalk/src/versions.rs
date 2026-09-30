use crate::Result;
use crate::pack::PackFormat;
use serde::Deserialize;

const ENVIRONMENTS: &str = include_str!("environments.toml");

/// The Minecraft versions Chalk can test on and the game each one runs as.
#[derive(Deserialize)]
pub struct Environments {
    pub fabric_loader: String,
    pub minecraft: Vec<Minecraft>,
}

#[derive(Clone, Deserialize)]
pub struct Minecraft {
    pub version: String,
    #[serde(deserialize_with = "deserialize_format")]
    pub data_format: PackFormat,
    /// The `teakit-fabric` version built for this Minecraft version.
    pub teakit: String,
    /// Modrinth `project:version` pairs installed on both sides.
    pub mods: Vec<String>,
}

pub fn environments() -> Result<Environments> {
    toml::from_str(ENVIRONMENTS)
        .map_err(|error| format!("invalid built-in environments.toml: {error}").into())
}

fn deserialize_format<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<PackFormat, D::Error> {
    let (major, minor) = <(u32, u32)>::deserialize(deserializer)?;
    Ok(PackFormat { major, minor })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_versions_are_newest_first() {
        let environments = environments().expect("environments.toml parses");
        let formats: Vec<PackFormat> = environments
            .minecraft
            .iter()
            .map(|minecraft| minecraft.data_format)
            .collect();
        let mut sorted = formats.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(formats, sorted);
    }
}
