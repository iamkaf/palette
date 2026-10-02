//! Modrinth modpacks for installs and pack pins.

use crate::{Context, Result, USER_AGENT};
use reqwest::Url;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

pub const API_BASE: &str = "https://api.modrinth.com/v2";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub project_type: String,
}

impl Project {
    fn display(&self) -> &str {
        first_non_empty(&[&self.title, &self.slug, &self.id])
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Version {
    pub id: String,
    pub version_number: String,
    /// `release`, `beta`, or `alpha`.
    pub version_type: String,
    pub game_versions: Vec<String>,
    pub files: Vec<File>,
}

impl Version {
    /// The primary `.mrpack` file, or the first one.
    fn primary_mrpack(&self) -> Option<&File> {
        let mut packs = self.files.iter().filter(|file| {
            file.filename.to_lowercase().ends_with(".mrpack")
                || file.url.to_lowercase().contains(".mrpack")
        });
        let first = packs.clone().next();
        packs.find(|file| file.primary).or(first)
    }

    /// The last listed game version, usually the newest in the range.
    pub fn minecraft(&self) -> &str {
        self.game_versions.last().map_or("", String::as_str)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct File {
    pub url: String,
    pub filename: String,
    pub primary: bool,
    pub hashes: BTreeMap<String, String>,
    pub size: u64,
}

/// A modpack version ready to pin and install.
#[derive(Debug)]
pub struct Pack {
    pub project: Project,
    pub version: Version,
    pub file: File,
    /// `modrinth:slug:version`.
    pub pin: String,
}

/// Whether a newer pack version is available.
pub struct UpdateCheck {
    pub latest_version: String,
    pub latest_name: String,
    pub latest_minecraft: String,
    pub available: bool,
}

pub struct Client {
    http: reqwest::blocking::Client,
    api_base: String,
}

impl Client {
    pub fn new() -> Result<Self> {
        Self::with_api_base(API_BASE)
    }

    pub fn with_api_base(api_base: &str) -> Result<Self> {
        Ok(Self {
            http: crate::http::client(Duration::from_secs(60))?,
            api_base: api_base.to_owned(),
        })
    }

    pub fn project(&self, id_or_slug: &str) -> Result<Project> {
        let id_or_slug = id_or_slug.trim();
        if id_or_slug.is_empty() {
            return Err("empty project id/slug".into());
        }
        let body = self.get(&["project", id_or_slug])?;
        let project: Project = serde_json::from_slice(&body).context("modrinth project json")?;
        if project.slug.is_empty() && project.id.is_empty() {
            return Err(format!("modrinth project not found: {id_or_slug}").into());
        }
        Ok(project)
    }

    /// Versions newest first, as the API returns them.
    pub fn versions(&self, id_or_slug: &str) -> Result<Vec<Version>> {
        let body = self.get(&["project", id_or_slug, "version"])?;
        serde_json::from_slice(&body).context("modrinth versions json")
    }

    fn modpack(&self, slug: &str) -> Result<Project> {
        let project = self.project(slug)?;
        if !project.project_type.eq_ignore_ascii_case("modpack") {
            return Err(format!(
                "{:?} is a Modrinth {}, not a modpack",
                project.display(),
                project.project_type
            )
            .into());
        }
        Ok(project)
    }

    /// Picks a modpack version (empty or `latest` for the newest release, a
    /// version ID, or a version number) and its `.mrpack` file.
    pub fn resolve_modpack(&self, slug: &str, version: &str) -> Result<Pack> {
        let project = self.modpack(slug)?;
        let versions = self.versions(&project.id)?;
        if versions.is_empty() {
            return Err(format!("modpack {} has no versions", project.display()).into());
        }
        let picked = select_version(&versions, version).context(project.display())?;
        let file = picked.primary_mrpack().cloned().ok_or_else(|| {
            format!(
                "{} {}: no .mrpack file on this version",
                project.display(),
                picked.version_number
            )
        })?;
        let slug = first_non_empty(&[&project.slug, &project.id]).to_owned();
        // Always pin the exact version. A slug-only pin would let every refresh move
        // the server to a newer pack, possibly a new Minecraft version; `pastel update`
        // is the deliberate way forward.
        let pinned = match version {
            "" | "latest" => picked.version_number.as_str(),
            version => version,
        };
        let pin = format!("modrinth:{slug}:{pinned}");
        Ok(Pack {
            version: picked.clone(),
            project,
            file,
            pin,
        })
    }

    /// Compares an installed version with the newest release that has an `.mrpack`.
    pub fn check_update(&self, slug: &str, baseline: &str) -> Result<UpdateCheck> {
        let slug = slug.trim();
        if slug.is_empty() {
            return Err("empty modrinth slug".into());
        }
        let project = self.modpack(slug)?;
        let versions = self.versions(&project.id)?;
        let latest = select_version(&versions, "")?;
        // Labels that don't compare as dotted versions update whenever they differ.
        let available = baseline.is_empty()
            || compare_version_labels(baseline, &latest.version_number).is_lt()
            || (baseline != latest.version_number && baseline != latest.id);
        Ok(UpdateCheck {
            latest_version: latest.version_number.clone(),
            latest_name: first_non_empty(&[&project.title, &project.slug]).to_owned(),
            latest_minecraft: latest.minecraft().to_owned(),
            available,
        })
    }

    /// Versions that include an `.mrpack`, newest first.
    pub fn mrpack_versions(&self, slug: &str) -> Result<(Project, Vec<Version>)> {
        let project = self.modpack(slug)?;
        let versions: Vec<Version> = self
            .versions(&project.id)?
            .into_iter()
            .filter(|version| version.primary_mrpack().is_some())
            .collect();
        if versions.is_empty() {
            return Err(format!("no .mrpack versions for {}", project.display()).into());
        }
        Ok((project, versions))
    }

    fn get(&self, segments: &[&str]) -> Result<Vec<u8>> {
        let mut url = Url::parse(&self.api_base).map_err(|error| error.to_string())?;
        url.path_segments_mut()
            .map_err(|()| format!("invalid Modrinth API base {}", self.api_base))?
            .pop_if_empty()
            .extend(segments);
        let response = self
            .http
            .get(url.clone())
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .send()?;
        let status = response.status();
        let mut body = Vec::new();
        response.take(8 << 20).read_to_end(&mut body)?;
        if status == reqwest::StatusCode::NOT_FOUND {
            let last = segments.last().copied().unwrap_or_default();
            return Err(format!("not found: {last}").into());
        }
        if status != reqwest::StatusCode::OK {
            return Err(format!("GET {url}: {status}").into());
        }
        Ok(body)
    }
}

fn select_version<'a>(versions: &'a [Version], want: &str) -> Result<&'a Version> {
    let want = want.trim();
    if want.is_empty() || want == "latest" {
        // Prefer a release with an .mrpack, else the newest version with one.
        return versions
            .iter()
            .find(|version| {
                version.version_type.eq_ignore_ascii_case("release")
                    && version.primary_mrpack().is_some()
            })
            .or_else(|| {
                versions
                    .iter()
                    .find(|version| version.primary_mrpack().is_some())
            })
            .ok_or_else(|| "no version with an .mrpack file".into());
    }
    versions
        .iter()
        .find(|version| version.id == want || version.version_number == want)
        .ok_or_else(|| format!("version {want:?} not found").into())
}

/// Reads the project slug and optional version from a modrinth.com page URL:
/// `/modpack/<slug>`, `/modpack/<slug>/version/<version>`, or `/project/<id>`.
pub fn parse_page_url(raw: &str) -> Option<(String, String)> {
    let url = Url::parse(raw.trim()).ok()?;
    let host = url.host_str()?;
    if url.port().is_some() || !matches!(host, "modrinth.com" | "www.modrinth.com") {
        return None;
    }
    let parts: Vec<&str> = url.path().trim_matches('/').split('/').collect();
    if parts.len() < 2
        || !matches!(
            parts[0].to_lowercase().as_str(),
            "modpack" | "mod" | "project"
        )
        || parts[1].is_empty()
    {
        return None;
    }
    let version = if parts.len() >= 4 && parts[2].eq_ignore_ascii_case("version") {
        parts[3]
    } else {
        ""
    };
    Some((parts[1].to_owned(), version.to_owned()))
}

/// Parses `modrinth:slug` or `modrinth:slug:version`.
pub fn parse_ref(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim();
    let prefix = raw.get(..9)?;
    if !prefix.eq_ignore_ascii_case("modrinth:") {
        return None;
    }
    let rest = &raw[9..];
    if rest.is_empty() {
        return None;
    }
    Some(match rest.split_once(':') {
        Some((slug, version)) => (slug.to_owned(), version.to_owned()),
        None => (rest.to_owned(), String::new()),
    })
}

/// Whether `text` could be a Modrinth project slug rather than a path, URL,
/// or coordinate.
pub fn looks_like_slug(text: &str) -> bool {
    let text = text.trim();
    (2..=64).contains(&text.len())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Parses the `aristea@0.1.4` and `aristea:0.1.4` shorthands. Maven
/// coordinates and `modrinth:` refs don't match.
pub fn parse_slug_version(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with("http://") || raw.starts_with("https://") {
        return None;
    }
    if raw
        .get(..9)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("modrinth:"))
    {
        return None;
    }
    if let Some(at) = raw.find('@')
        && at > 0
    {
        let (slug, version) = (&raw[..at], &raw[at + 1..]);
        if looks_like_slug(slug) && !version.trim().is_empty() && !version.contains('@') {
            return Some((slug.to_owned(), version.trim().to_owned()));
        }
        return None;
    }
    if raw.matches(':').count() != 1 {
        return None;
    }
    let (slug, version) = raw.split_once(':')?;
    if !looks_like_slug(slug) || version.trim().is_empty() {
        return None;
    }
    // A dotted first segment looks like a Maven group; only accept it when the
    // rest still reads as a pack version.
    if slug.contains('.') && version.contains(['/', '\\']) {
        return None;
    }
    Some((slug.to_owned(), version.trim().to_owned()))
}

/// Builds `modrinth:slug` or `modrinth:slug:version`.
pub fn pin(slug: &str, version: &str) -> String {
    let (slug, version) = (slug.trim(), version.trim());
    if version.is_empty() {
        format!("modrinth:{slug}")
    } else {
        format!("modrinth:{slug}:{version}")
    }
}

/// Best-effort dotted comparison where each segment's leading digits count.
fn compare_version_labels(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let a = a.trim().trim_start_matches('v');
    let b = b.trim().trim_start_matches('v');
    if a == b {
        return Ordering::Equal;
    }
    let a: Vec<&str> = a.split('.').collect();
    let b: Vec<&str> = b.split('.').collect();
    for index in 0..a.len().max(b.len()) {
        let left = label_segment(&a, index);
        let right = label_segment(&b, index);
        let order = match (leading_number(left), leading_number(right)) {
            (Some(left), Some(right)) => left.cmp(&right),
            _ => left.cmp(right),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

fn label_segment<'a>(parts: &[&'a str], index: usize) -> &'a str {
    let part = parts.get(index).copied().unwrap_or("");
    part.split('-').next().unwrap_or(part)
}

fn leading_number(text: &str) -> Option<u64> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    Some(digits.bytes().fold(0u64, |n, digit| {
        n.wrapping_mul(10).wrapping_add(u64::from(digit - b'0'))
    }))
}

fn first_non_empty<'a>(values: &[&'a str]) -> &'a str {
    values
        .iter()
        .copied()
        .find(|value| !value.trim().is_empty())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    fn pair(slug: &str, version: &str) -> Option<(String, String)> {
        Some((slug.to_owned(), version.to_owned()))
    }

    #[test]
    fn parses_page_urls() {
        assert_eq!(
            parse_page_url("https://modrinth.com/modpack/aristea"),
            pair("aristea", "")
        );
        assert_eq!(
            parse_page_url("https://www.modrinth.com/modpack/aristea/version/1.2.3"),
            pair("aristea", "1.2.3")
        );
        assert_eq!(
            parse_page_url("https://modrinth.com/project/AABBCCDD"),
            pair("AABBCCDD", "")
        );
        assert_eq!(parse_page_url("https://example.com/modpack/x"), None);
    }

    #[test]
    fn parses_modrinth_refs() {
        assert_eq!(parse_ref("modrinth:aristea"), pair("aristea", ""));
        assert_eq!(
            parse_ref("modrinth:aristea:1.0.0"),
            pair("aristea", "1.0.0")
        );
        assert_eq!(parse_ref("aristea"), None);
    }

    #[test]
    fn recognizes_slugs() {
        assert!(looks_like_slug("aristea") && looks_like_slug("my-pack_1"));
        for text in ["com.example:x:1", "https://x", "a/b", "a@b"] {
            assert!(!looks_like_slug(text), "{text}");
        }
    }

    #[test]
    fn parses_slug_shorthands() {
        assert_eq!(
            parse_slug_version("aristea:0.1.4"),
            pair("aristea", "0.1.4")
        );
        assert_eq!(
            parse_slug_version("aristea@0.1.4"),
            pair("aristea", "0.1.4")
        );
        assert_eq!(
            parse_slug_version("com.example.modpacks:example-pack:1.1.0"),
            None
        );
        assert_eq!(parse_slug_version("modrinth:aristea:1.0"), None);
        assert_eq!(parse_slug_version("aristea"), None);
    }

    fn version(id: &str, number: &str, kind: &str) -> Version {
        Version {
            id: id.to_owned(),
            version_number: number.to_owned(),
            version_type: kind.to_owned(),
            files: vec![File {
                url: "u".to_owned(),
                filename: "p.mrpack".to_owned(),
                primary: true,
                ..File::default()
            }],
            ..Version::default()
        }
    }

    #[test]
    fn latest_prefers_a_release() {
        let versions = [
            version("b", "2.0.0", "beta"),
            version("a", "1.0.0", "release"),
        ];
        assert_eq!(select_version(&versions, "").unwrap().id, "a");
        assert_eq!(select_version(&versions, "2.0.0").unwrap().id, "b");
    }

    #[test]
    fn compares_version_labels() {
        assert_eq!(compare_version_labels("1.7.2", "1.7.3"), Ordering::Less);
        assert_eq!(compare_version_labels("1.7.3", "1.7.3"), Ordering::Equal);
        assert_eq!(compare_version_labels("2.0.0", "1.9.9"), Ordering::Greater);
    }

    #[test]
    fn builds_pins() {
        assert_eq!(pin("aristea", ""), "modrinth:aristea");
        assert_eq!(pin("aristea", "1.2.3"), "modrinth:aristea:1.2.3");
    }
}
