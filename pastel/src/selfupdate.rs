//! Replaces the running Pastel executable with the latest verified release.

use crate::{Context, Result, http};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read, Write};
use std::path::Path;
use std::time::Duration;

/// Palette publishes every tool's releases here; Pastel's are tagged `pastel-v*`.
pub const RELEASES_URL: &str = "https://api.github.com/repos/iamkaf/palette/releases?per_page=100";

const TAG_PREFIX: &str = "pastel-";
const MANIFEST: &str = "release-manifest.json";
const MAX_DOWNLOAD_BYTES: u64 = 128 << 20;
const MAX_BINARY_BYTES: u64 = 64 << 20;

pub struct Outcome {
    /// Such as `v0.2.0`.
    pub version: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct ReleaseManifest {
    artifacts: Vec<Artifact>,
}

#[derive(Deserialize)]
struct Artifact {
    path: String,
    sha256: String,
}

/// Downloads, verifies, and installs the latest Pastel release over `executable`.
pub fn run(executable: &Path, releases_url: &str) -> Result<Outcome> {
    let archive = archive_name().ok_or_else(|| {
        format!(
            "self-update is not available for {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let client = http::client(Duration::from_secs(120))?;
    let releases: Vec<Release> = serde_json::from_slice(&download(&client, releases_url, 4 << 20)?)
        .context("read Pastel releases")?;
    let release = releases
        .into_iter()
        .find(|release| {
            release.tag_name.starts_with(TAG_PREFIX) && !release.draft && !release.prerelease
        })
        .ok_or("no Pastel release found")?;
    let asset = |name: &str| -> Result<&str> {
        release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .map(|asset| asset.browser_download_url.as_str())
            .ok_or_else(|| format!("{} has no {name}", release.tag_name).into())
    };

    let manifest: ReleaseManifest =
        serde_json::from_slice(&download(&client, asset(MANIFEST)?, 1 << 20)?)
            .context("read release checksums")?;
    let expected = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.path == archive)
        .map(|artifact| artifact.sha256.to_lowercase())
        .filter(|sha256| sha256.len() == 64 && hex::decode(sha256).is_ok())
        .ok_or_else(|| format!("release checksum is missing or invalid for {archive}"))?;
    let body = download(&client, asset(&archive)?, MAX_DOWNLOAD_BYTES)
        .context(format_args!("download {archive}"))?;
    if hex::encode(Sha256::digest(&body)) != expected {
        return Err(format!("checksum verification failed for {archive}").into());
    }
    let binary = extract_binary(&archive, &body)?;
    replace_executable(executable, &binary)
        .context(format_args!("replace {}", executable.display()))?;
    Ok(Outcome {
        version: release.tag_name[TAG_PREFIX.len()..].to_owned(),
    })
}

/// Palette's archive name for this computer, such as `pastel-macos-aarch64.tar.gz`.
fn archive_name() -> Option<String> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    if !matches!(arch, "x86_64" | "aarch64") {
        return None;
    }
    match os {
        "linux" | "macos" => Some(format!("pastel-{os}-{arch}.tar.gz")),
        "windows" => Some(format!("pastel-{os}-{arch}.zip")),
        _ => None,
    }
}

fn download(client: &reqwest::blocking::Client, url: &str, limit: u64) -> Result<Vec<u8>> {
    http::read_limited(http::get(client, url)?, limit, "download")
}

/// Reads the executable from the root of a release archive.
fn extract_binary(archive: &str, body: &[u8]) -> Result<Vec<u8>> {
    let mut binary = Vec::new();
    if archive.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(Cursor::new(body)).context("open release archive")?;
        let file = zip
            .by_name("pastel.exe")
            .map_err(|_| "release archive did not contain pastel.exe")?;
        file.take(MAX_BINARY_BYTES + 1).read_to_end(&mut binary)?;
    } else {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(body));
        let mut found = false;
        for entry in tar.entries().context("open release archive")? {
            let entry = entry.context("read release archive")?;
            let path = entry.path()?.into_owned();
            if entry.header().entry_type().is_file() && path == Path::new("pastel") {
                entry.take(MAX_BINARY_BYTES + 1).read_to_end(&mut binary)?;
                found = true;
                break;
            }
        }
        if !found {
            return Err("release archive did not contain pastel".into());
        }
    }
    if binary.is_empty() || binary.len() as u64 > MAX_BINARY_BYTES {
        return Err("release binary has an invalid size".into());
    }
    Ok(binary)
}

fn replace_executable(path: &Path, binary: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let mut new = tempfile::Builder::new()
        .prefix(".pastel-update-")
        .suffix(suffix)
        .tempfile_in(dir)?;
    new.write_all(binary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        new.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o755))?;
    }
    new.as_file().sync_all()?;
    // Windows can rename a running executable but not replace it.
    #[cfg(windows)]
    {
        let mut old = path.as_os_str().to_owned();
        old.push(".old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(path, &old)?;
        if let Err(error) = new.persist(path) {
            let _ = std::fs::rename(&old, path);
            return Err(error.error);
        }
        let _ = std::fs::remove_file(&old);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        new.persist(path).map(|_| ()).map_err(|error| error.error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::serve;

    fn release_archive(binary: &[u8]) -> (String, Vec<u8>) {
        let name = archive_name().expect("supported test platform");
        let mut body = Vec::new();
        if name.ends_with(".zip") {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut body));
            zip.start_file("pastel.exe", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(binary).unwrap();
            zip.finish().unwrap();
        } else {
            let gz = flate2::write::GzEncoder::new(&mut body, flate2::Compression::default());
            let mut tar = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(binary.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, "pastel", binary).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        (name, body)
    }

    /// Serves a releases list whose newest Pastel release has `sha256` for the archive.
    fn serve_release(archive: &str, body: Vec<u8>, sha256: &str) -> String {
        let manifest = format!(
            r#"{{"schemaVersion": 1, "artifacts": [{{"path": "{archive}", "sha256": "{sha256}"}}]}}"#
        );
        let base = serve(vec![
            ("/manifest".to_owned(), manifest.into_bytes()),
            ("/archive".to_owned(), body),
        ]);
        let releases = format!(
            r#"[{{"tag_name": "chalk-v0.3.0", "assets": []}},
                {{"tag_name": "pastel-v0.2.1", "draft": true, "assets": []}},
                {{"tag_name": "pastel-v0.2.0", "assets": [
                    {{"name": "{MANIFEST}", "browser_download_url": "{base}/manifest"}},
                    {{"name": "{archive}", "browser_download_url": "{base}/archive"}}]}}]"#
        );
        serve(vec![("/releases".to_owned(), releases.into_bytes())]) + "/releases"
    }

    #[test]
    fn installs_a_verified_release() {
        let want = b"new pastel binary";
        let (archive, body) = release_archive(want);
        let sha256 = hex::encode(Sha256::digest(&body));
        let releases = serve_release(&archive, body, &sha256);
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("pastel");
        std::fs::write(&executable, "old").unwrap();

        let outcome = run(&executable, &releases).unwrap();
        assert_eq!(outcome.version, "v0.2.0");
        assert_eq!(std::fs::read(&executable).unwrap(), want);
    }

    #[test]
    fn rejects_a_checksum_mismatch() {
        let (archive, body) = release_archive(b"untrusted");
        let releases = serve_release(&archive, body, &"0".repeat(64));
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("pastel");
        std::fs::write(&executable, "old").unwrap();

        assert!(run(&executable, &releases).is_err());
        assert_eq!(std::fs::read(&executable).unwrap(), b"old");
    }
}
