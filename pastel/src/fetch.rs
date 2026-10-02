//! Hash-verified downloads of pack files.

use crate::pack::PackFile;
use crate::{Result, http};
use reqwest::blocking::Client;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

const MAX_MANAGED_FILE_BYTES: u64 = 2 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Sha512,
    Sha256,
    Sha1,
}

impl Algorithm {
    /// Strongest first.
    pub const ALL: [Algorithm; 3] = [Algorithm::Sha512, Algorithm::Sha256, Algorithm::Sha1];

    pub fn key(self) -> &'static str {
        match self {
            Algorithm::Sha512 => "sha512",
            Algorithm::Sha256 => "sha256",
            Algorithm::Sha1 => "sha1",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Algorithm::Sha512 => "SHA-512",
            Algorithm::Sha256 => "SHA-256",
            Algorithm::Sha1 => "SHA-1",
        }
    }

    /// The lowercase hex digest of everything `reader` yields.
    pub fn hex_digest(self, reader: impl Read) -> io::Result<String> {
        match self {
            Algorithm::Sha512 => digest::<Sha512>(reader),
            Algorithm::Sha256 => digest::<Sha256>(reader),
            Algorithm::Sha1 => digest::<Sha1>(reader),
        }
    }
}

fn digest<D: Digest + Write>(mut reader: impl Read) -> io::Result<String> {
    let mut hasher = D::new();
    io::copy(&mut reader, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

/// Whether the file at `path` has the expected digest.
pub fn file_matches(path: &Path, algorithm: Algorithm, want: &str) -> io::Result<bool> {
    let got = algorithm.hex_digest(fs::File::open(path)?)?;
    Ok(got.eq_ignore_ascii_case(want))
}

/// Makes `dest` match the pack entry, downloading it when it's missing or
/// different. Returns whether the file changed.
pub fn ensure_file(client: &Client, file: &PackFile, dest: &Path) -> Result<bool> {
    let (algorithm, want) = file.preferred_hash()?;
    if dest.is_file() && file_matches(dest, algorithm, want)? {
        return Ok(false);
    }
    if file.downloads.is_empty() {
        return Err(format!("{}: no download sources", file.path).into());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".pastel-tmp");
    let tmp = TempPath(tmp.into());

    let mut last = None;
    for url in &file.downloads {
        if let Err(error) = download_to(client, url, &tmp.0, file.file_size) {
            last = Some(error);
            continue;
        }
        match file_matches(&tmp.0, algorithm, want) {
            Ok(true) => {
                fs::rename(&tmp.0, dest)?;
                return Ok(true);
            }
            Ok(false) => {
                let got = algorithm
                    .hex_digest(fs::File::open(&tmp.0)?)
                    .unwrap_or_default();
                last = Some(
                    format!(
                        "hash mismatch from {url} (want {} {want}, got {got})",
                        algorithm.key()
                    )
                    .into(),
                );
            }
            Err(error) => last = Some(error.into()),
        }
    }
    let error = last.unwrap_or_else(|| "all sources failed".into());
    Err(error.context(&file.path))
}

fn download_to(client: &Client, url: &str, dest: &Path, expected_size: u64) -> Result<()> {
    let response = http::get(client, url)?;
    let limit = if expected_size > 0 {
        if let Some(length) = response.content_length()
            && length != expected_size
        {
            return Err(format!(
                "GET {url}: size mismatch (want {expected_size} bytes, got {length})"
            )
            .into());
        }
        expected_size
    } else {
        MAX_MANAGED_FILE_BYTES
    };
    let mut out = fs::File::create(dest)?;
    let written = io::copy(&mut response.take(limit + 1), &mut out)?;
    if written > limit {
        return Err(format!("GET {url}: file is too large").into());
    }
    if expected_size > 0 && written != expected_size {
        return Err(format!(
            "GET {url}: size mismatch (want {expected_size} bytes, got {written})"
        )
        .into());
    }
    Ok(())
}

/// Removes a temporary download when it goes out of scope.
struct TempPath(std::path::PathBuf);

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
