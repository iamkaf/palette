//! Pack artifacts in a Maven repository layout.

use crate::{Context, Result, http, version};
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;

pub const NO_REPOSITORIES: &str = "no Maven repositories configured (set repositories = [\"https://…\"] in server.pastel, or pin pack to a full https://…/.mrpack URL)";

const MAX_ARTIFACT_BYTES: u64 = 512 << 20;

/// Trims bases, drops empties and duplicates, and keeps their order. Empty
/// input stays empty: there is no default host.
pub fn normalize_repositories(bases: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    bases
        .iter()
        .map(|base| base.trim().trim_end_matches('/').to_owned())
        .filter(|base| !base.is_empty() && seen.insert(base.clone()))
        .collect()
}

/// `group:artifact:version[:classifier]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Coordinate {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub classifier: Option<String>,
}

impl Coordinate {
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        let parts: Vec<&str> = text.split(':').collect();
        if !(3..=4).contains(&parts.len()) {
            return Err(format!(
                "invalid maven coordinate {text:?} (want group:artifact:version[:classifier])"
            )
            .into());
        }
        if parts[..3].iter().any(|part| part.is_empty()) {
            return Err(format!("invalid maven coordinate {text:?}").into());
        }
        Ok(Self {
            group: parts[0].to_owned(),
            artifact: parts[1].to_owned(),
            version: parts[2].to_owned(),
            classifier: parts.get(3).map(|part| (*part).to_owned()),
        })
    }

    /// The repository-relative path of the artifact with extension `ext`.
    pub fn path(&self, ext: &str) -> String {
        let mut name = format!("{}-{}", self.artifact, self.version);
        if let Some(classifier) = &self.classifier {
            name.push('-');
            name.push_str(classifier);
        }
        format!(
            "{}/{}/{}/{name}.{ext}",
            self.group.replace('.', "/"),
            self.artifact,
            self.version
        )
    }

    pub fn url(&self, base: &str, ext: &str) -> String {
        format!("{}/{}", base.trim_end_matches('/'), self.path(ext))
    }

    /// `group:artifact:version[:classifier]`.
    pub fn display(&self) -> String {
        let mut text = format!("{}:{}:{}", self.group, self.artifact, self.version);
        if let Some(classifier) = &self.classifier {
            text.push(':');
            text.push_str(classifier);
        }
        text
    }
}

/// GETs against an ordered list of repositories.
pub struct Client {
    bases: Vec<String>,
    http: reqwest::blocking::Client,
}

impl Client {
    pub fn new(bases: &[String]) -> Result<Self> {
        Ok(Self {
            bases: normalize_repositories(bases),
            http: http::client(Duration::from_secs(120))?,
        })
    }

    /// Downloads an artifact from the first repository that has it.
    pub fn fetch(&self, coordinate: &Coordinate, ext: &str) -> Result<Vec<u8>> {
        let mut last = crate::Error::from(NO_REPOSITORIES);
        for base in &self.bases {
            match self.get_bytes(&coordinate.url(base, ext)) {
                Ok(data) => return Ok(data),
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    /// Downloads pack bytes. Packs are published as `.mrpack` only.
    pub fn fetch_pack(&self, coordinate: &Coordinate) -> Result<Vec<u8>> {
        self.fetch(coordinate, "mrpack")
            .context("fetch pack .mrpack")
    }

    /// The release pointer, or the newest listed version.
    pub fn latest_version(&self, group: &str, artifact: &str) -> Result<String> {
        let (versions, release) = self.list_versions(group, artifact)?;
        if !release.is_empty() {
            return Ok(release);
        }
        versions
            .into_iter()
            .next()
            .ok_or_else(|| format!("no versions for {group}:{artifact}").into())
    }

    /// All versions, newest first, and the release pointer when one is set.
    pub fn list_versions(&self, group: &str, artifact: &str) -> Result<(Vec<String>, String)> {
        if self.bases.is_empty() {
            return Err(NO_REPOSITORIES.into());
        }
        let group_path = group.replace('.', "/");
        let mut last = None;
        for base in &self.bases {
            let url = format!("{base}/{group_path}/{artifact}/maven-metadata.xml");
            let body = match self.get_bytes(&url) {
                Ok(body) => body,
                Err(error) => {
                    last = Some(error);
                    continue;
                }
            };
            let metadata: Metadata = quick_xml::de::from_str(&String::from_utf8_lossy(&body))
                .context("parse maven-metadata")?;
            let versioning = metadata.versioning;
            let release = if versioning.release.is_empty() {
                versioning.latest
            } else {
                versioning.release
            };
            let mut versions = versioning.versions.version;
            if versions.is_empty() {
                if !release.is_empty() {
                    return Ok((vec![release.clone()], release));
                }
                last = Some(format!("no versions in {url}").into());
                continue;
            }
            sort_newest_first(&mut versions);
            return Ok((versions, release));
        }
        Err(last.unwrap_or_else(|| NO_REPOSITORIES.into()))
    }

    fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        // The pack decides which jars the server runs, so it must not travel over plain HTTP.
        if !url
            .get(..8)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
        {
            return Err(format!("Maven repository URLs must use https://: {url}").into());
        }
        let response = http::get(&self.http, url)?;
        http::read_limited(response, MAX_ARTIFACT_BYTES, "artifact")
            .context(format_args!("GET {url}"))
    }
}

/// A stable insertion sort. `version::compare` compares a segment as a number
/// or as text depending on both sides, which isn't a total order, and the
/// standard library's sorts may panic on such a comparison.
fn sort_newest_first(versions: &mut [String]) {
    for next in 1..versions.len() {
        let mut index = next;
        while index > 0 && version::compare(&versions[index], &versions[index - 1]).is_gt() {
            versions.swap(index, index - 1);
            index -= 1;
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Metadata {
    versioning: Versioning,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Versioning {
    latest: String,
    release: String,
    versions: Versions,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Versions {
    version: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_artifact_paths() {
        let coordinate = Coordinate::parse("com.example.modpacks:example-pack:1.0.1").unwrap();
        let path = "com/example/modpacks/example-pack/1.0.1/example-pack-1.0.1.mrpack";
        assert_eq!(coordinate.path("mrpack"), path);
        assert_eq!(
            coordinate.url("https://maven.example.com/", "mrpack"),
            format!("https://maven.example.com/{path}")
        );
        let classified = Coordinate::parse("com.example.tools:example:1.0.0:linux-amd64").unwrap();
        assert_eq!(
            classified.path("bin"),
            "com/example/tools/example/1.0.0/example-1.0.0-linux-amd64.bin"
        );
    }

    #[test]
    fn normalizes_repositories_without_a_default() {
        let bases = [
            "https://a.example/",
            "",
            "https://b.example",
            "https://a.example",
        ]
        .map(String::from);
        assert_eq!(
            normalize_repositories(&bases),
            ["https://a.example", "https://b.example"]
        );
        assert!(normalize_repositories(&[]).is_empty());
    }

    #[test]
    fn a_client_without_repositories_fails() {
        let client = Client::new(&[]).unwrap();
        let coordinate = Coordinate::parse("g.x:a:1").unwrap();
        let error = client.fetch_pack(&coordinate).unwrap_err();
        assert!(error.to_string().contains("no Maven repositories"));
    }

    #[test]
    fn sorts_newest_first_even_when_schemes_mix() {
        let mut versions = ["1.2.0", "1.10.0", "1.9.1"].map(String::from).to_vec();
        sort_newest_first(&mut versions);
        assert_eq!(versions, ["1.10.0", "1.9.1", "1.2.0"]);
        // Mixed segments like these made the standard sort panic past 20 items.
        let mut mixed: Vec<String> = (0..40)
            .map(|patch| match patch % 3 {
                0 => format!("1.0.{patch}+1.21.1"),
                _ => format!("1.0.{patch}"),
            })
            .collect();
        sort_newest_first(&mut mixed);
        assert_eq!(mixed.len(), 40);
    }

    #[test]
    fn reads_versions_from_maven_metadata() {
        let metadata: Metadata = quick_xml::de::from_str(
            "<metadata><groupId>g</groupId><versioning><latest>1.10.0</latest><versions>\
             <version>1.2.0</version><version>1.10.0</version></versions></versioning></metadata>",
        )
        .unwrap();
        assert_eq!(metadata.versioning.latest, "1.10.0");
        assert!(metadata.versioning.release.is_empty());
        assert_eq!(metadata.versioning.versions.version, ["1.2.0", "1.10.0"]);
    }
}
