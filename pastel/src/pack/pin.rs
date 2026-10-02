use super::LoadedMrpack;
use crate::maven::{self, Coordinate};
use crate::{Context, Result, modrinth, version};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinKind {
    Url,
    File,
    Modrinth,
    Maven,
    Path,
}

/// The single place that tells an https URL, `file:` URL, Modrinth pin (page
/// URL, `modrinth:` ref, slug shorthand, or bare slug), Maven coordinate, and
/// local path apart. It never touches the filesystem.
pub fn classify_pin(raw: &str) -> PinKind {
    let pin = raw.trim();
    if pin.is_empty() {
        return PinKind::Path;
    }
    let lower = pin.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return if modrinth::parse_page_url(pin).is_some() {
            PinKind::Modrinth
        } else {
            PinKind::Url
        };
    }
    if lower.starts_with("file:") {
        PinKind::File
    } else if modrinth::parse_ref(pin).is_some() || modrinth::parse_slug_version(pin).is_some() {
        PinKind::Modrinth
    } else if is_maven_coordinate(pin) {
        PinKind::Maven
    } else if modrinth::looks_like_slug(pin) {
        PinKind::Modrinth
    } else {
        PinKind::Path
    }
}

/// Whether `text` looks like `group:artifact:version[:classifier]` with a dotted group.
pub fn is_maven_coordinate(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.contains(['/', '\\']) {
        return false;
    }
    let parts: Vec<&str> = text.split(':').collect();
    (3..=4).contains(&parts.len()) && parts[0].contains('.')
}

/// A pack's identity on Maven: group, artifact, and optional pinned version.
#[derive(Debug, Clone)]
pub struct MavenRef {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

impl MavenRef {
    /// Parses `group:artifact[:version[:classifier]]`. Paths and `file:` URLs don't match.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() || raw.starts_with("file:") || raw.contains(['/', '\\']) {
            return None;
        }
        let parts: Vec<&str> = raw.split(':').collect();
        if !(2..=4).contains(&parts.len()) || !parts[0].contains('.') {
            return None;
        }
        Some(Self {
            group: parts[0].to_owned(),
            artifact: parts[1].to_owned(),
            version: parts.get(2).copied().unwrap_or("").to_owned(),
        })
    }

    /// `group:artifact:version`, defaulting to the pinned version.
    pub fn coordinate(&self, version: &str) -> String {
        let version = if version.is_empty() {
            &self.version
        } else {
            version
        };
        format!("{}:{}:{version}", self.group, self.artifact)
    }
}

pub struct UpdateCheck {
    pub latest_version: String,
    pub latest_minecraft: String,
    pub latest_name: String,
    pub available: bool,
}

/// Looks up the latest Maven release of the pack and compares it with
/// `baseline`, the installed version.
pub fn check_update(
    repositories: &[String],
    pin: &MavenRef,
    baseline: &str,
) -> Result<UpdateCheck> {
    let repositories = maven::normalize_repositories(repositories);
    if repositories.is_empty() {
        return Err(maven::NO_REPOSITORIES.into());
    }
    let client = maven::Client::new(&repositories)?;
    let latest = client
        .latest_version(&pin.group, &pin.artifact)
        .context("check latest pack version")?;
    let coordinate = Coordinate {
        group: pin.group.clone(),
        artifact: pin.artifact.clone(),
        version: latest.clone(),
        classifier: None,
    };
    let data = client
        .fetch_pack(&coordinate)
        .context(format_args!("fetch latest pack {latest}"))?;
    let manifest = LoadedMrpack::from_bytes(&data)?.manifest();
    // With nothing installed, any release counts as an update.
    let available = baseline.is_empty() || version::compare(baseline, &latest).is_lt();
    Ok(UpdateCheck {
        latest_minecraft: manifest.minecraft().to_owned(),
        latest_name: manifest.name,
        latest_version: latest,
        available,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_every_pin_shape() {
        let cases = [
            ("https://example.com/pack.mrpack", PinKind::Url),
            ("http://example.com/pack.mrpack", PinKind::Url),
            ("HTTPS://example.com/pack.mrpack", PinKind::Url),
            ("https://modrinth.com/modpack/aristea", PinKind::Modrinth),
            (
                "https://modrinth.com/modpack/aristea/version/1.2.3",
                PinKind::Modrinth,
            ),
            ("https://modrinth.com/project/AABBCCDD", PinKind::Modrinth),
            ("file:///tmp/pack.mrpack", PinKind::File),
            ("FILE:pack.mrpack", PinKind::File),
            ("modrinth:aristea", PinKind::Modrinth),
            ("modrinth:aristea:1.2.3", PinKind::Modrinth),
            ("aristea:0.1.4", PinKind::Modrinth),
            ("aristea@0.1.4", PinKind::Modrinth),
            ("aristea", PinKind::Modrinth),
            ("com.example.modpacks:example-pack:1.2.0", PinKind::Maven),
            (
                "com.example.modpacks:example-pack:1.2.0:shaded",
                PinKind::Maven,
            ),
            ("./packs/local.mrpack", PinKind::Path),
            ("/abs/path/pack.mrpack", PinKind::Path),
            ("C:\\servers\\pack.mrpack", PinKind::Path),
            ("", PinKind::Path),
            ("   ", PinKind::Path),
        ];
        for (raw, want) in cases {
            assert_eq!(classify_pin(raw), want, "{raw:?}");
        }
    }

    // Maven coordinates and slug shorthands stay disjoint, so classify_pin's
    // order can never misroute a pin.
    #[test]
    fn maven_coordinates_and_slug_shorthands_are_disjoint() {
        for coordinate in [
            "com.example:pack:1.0.0",
            "com.example:pack:1.0.0:shaded",
            "a.b:c:d",
        ] {
            assert_eq!(
                modrinth::parse_slug_version(coordinate),
                None,
                "{coordinate}"
            );
        }
        for shorthand in ["aristea:0.1.4", "aristea@0.1.4"] {
            assert!(!is_maven_coordinate(shorthand), "{shorthand}");
        }
    }
}
