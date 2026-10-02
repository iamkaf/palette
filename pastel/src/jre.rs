//! Java requirements per Minecraft version, and managed Temurin runtimes.

use crate::{Context, Result, confined, http, ui};
use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// The minimum Java major version for a Minecraft version:
/// 26.1 and newer need Java 25, 1.20.5 through 1.21.x Java 21, 1.17 through
/// 1.20.4 Java 17, and older releases Java 8. Unknown versions get Java 25.
pub fn require_major(minecraft: &str) -> u32 {
    let minecraft = minecraft.trim();
    let parts: Vec<&str> = minecraft.split('.').collect();
    let Ok(major) = parts[0].parse::<u32>() else {
        return 25;
    };
    let number = |index: usize| {
        parts
            .get(index)
            .and_then(|part| part.split('-').next())
            .and_then(|part| part.parse::<u32>().ok())
            .unwrap_or(0)
    };
    match major {
        // Year-based releases.
        26.. => 25,
        1 => {
            let (minor, patch) = (number(1), number(2));
            if minor > 20 || (minor == 20 && patch >= 5) {
                21
            } else if minor >= 17 {
                17
            } else {
                8
            }
        }
        // An unknown numbering scheme: stay conservative and modern.
        2.. => 21,
        0 => 8,
    }
}

/// A short label such as `Java 21 (for Minecraft 1.21.1)`.
pub fn format_requirement(minecraft: &str) -> String {
    let major = require_major(minecraft);
    if minecraft.is_empty() {
        format!("Java {major}")
    } else {
        format!("Java {major} (for Minecraft {minecraft})")
    }
}

static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"version "([0-9]+)(?:\.([0-9]+))?"#).expect("valid Java version pattern")
});

/// Runs `java -version` and reads the major version.
pub fn detect_major(java: &Path) -> Result<u32> {
    let output = Command::new(java)
        .arg("-version")
        .output()
        .context(format_args!("couldn't run {} -version", java.display()))?;
    // Some JVMs print the version to stdout.
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    if !output.status.success() {
        return Err(format!(
            "couldn't run {} -version: {}",
            java.display(),
            output.status
        )
        .into());
    }
    let captures = VERSION
        .captures(&text)
        .ok_or_else(|| format!("couldn't parse Java version from: {}", text.trim()))?;
    let major: u32 = captures[1].parse().unwrap_or(0);
    // Legacy 1.8.0_xxx style.
    if major == 1
        && let Some(minor) = captures
            .get(2)
            .and_then(|minor| minor.as_str().parse().ok())
        && minor > 0
    {
        return Ok(minor);
    }
    Ok(major)
}

/// A Java executable that provides at least `required`: the configured one or
/// the system's when new enough, otherwise a managed runtime.
pub fn ensure(root: &Path, required: u32, configured: Option<&str>) -> Result<PathBuf> {
    match configured.filter(|java| !java.is_empty() && *java != "java") {
        Some(java) => match detect_major(Path::new(java)) {
            Ok(major) if major >= required => {
                ui::detail(&format!("using configured Java {major} ({java})"));
                return Ok(PathBuf::from(java));
            }
            Ok(major) => ui::warn(&format!(
                "configured java is too old (have {major}, need {required}) — using managed JRE"
            )),
            Err(error) => ui::warn(&format!(
                "configured java unusable ({error}) — using managed JRE"
            )),
        },
        None => {
            if let Ok(major) = detect_major(Path::new("java"))
                && major >= required
            {
                ui::detail(&format!("using system Java {major}"));
                return Ok(PathBuf::from("java"));
            }
        }
    }
    let java = ensure_managed(root, required)?;
    let major = detect_major(&java)?;
    if major < required {
        return Err(format!("managed Java reports version {major}, need {required}").into());
    }
    ui::detail(&format!("using managed Java {major}"));
    Ok(java)
}

fn ensure_managed(root: &Path, major: u32) -> Result<PathBuf> {
    let jre_dir = root.join(".pastel").join("jre");
    let base = jre_dir.join(major.to_string());
    if let Some(java) = find_java(&base) {
        return Ok(java);
    }
    ui::step(&format!(
        "Downloading Java {major} for this Minecraft version…"
    ));
    ui::detail("Pastel keeps a private JRE under .pastel/jre/ (like a launcher)");
    fs::create_dir_all(&jre_dir)?;

    let platform = Platform::current()?;
    let package =
        resolve_package(major, &platform).context(format_args!("resolve Java {major}"))?;
    let staging = jre_dir.join(format!("{major}.install-tmp"));
    remove_dir_if_present(&staging)?;
    fs::create_dir_all(&staging)?;
    let result = (|| -> Result<PathBuf> {
        let archive = staging.join(format!("download{}", platform.extension));
        download_verified(&package.link, &archive, &package.checksum, package.size)
            .context(format_args!("download Java {major}"))?;
        ui::step("Installing Java…");
        extract(&archive, &staging).context(format_args!("extract Java {major}"))?;
        if find_java(&staging).is_none() {
            return Err(format!("Java {major} installed but bin/java not found").into());
        }
        fs::remove_file(&archive)?;
        remove_dir_if_present(&base)?;
        fs::rename(&staging, &base)?;
        find_java(&base).ok_or_else(|| format!("no java under {}", base.display()).into())
    })();
    let _ = remove_dir_if_present(&staging);
    let java = result?;
    ui::ok(&format!("Java {major} ready"));
    Ok(java)
}

fn remove_dir_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

struct Platform {
    os: &'static str,
    arch: &'static str,
    extension: &'static str,
}

impl Platform {
    fn current() -> Result<Self> {
        let (os, extension) = match std::env::consts::OS {
            "linux" => ("linux", ".tar.gz"),
            "macos" => ("mac", ".tar.gz"),
            "windows" => ("windows", ".zip"),
            other => return Err(format!("unsupported OS {other} for managed Java").into()),
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "aarch64",
            other => return Err(format!("unsupported CPU {other} for managed Java").into()),
        };
        Ok(Self {
            os,
            arch,
            extension,
        })
    }
}

#[derive(Debug, Deserialize)]
struct Package {
    #[serde(default)]
    link: String,
    #[serde(default)]
    checksum: String,
    #[serde(default)]
    size: u64,
}

fn resolve_package(major: u32, platform: &Platform) -> Result<Package> {
    #[derive(Deserialize)]
    struct Release {
        #[serde(default)]
        binaries: Vec<Binary>,
    }
    #[derive(Deserialize)]
    struct Binary {
        package: Package,
    }
    let url = format!(
        "https://api.adoptium.net/v3/assets/feature_releases/{major}/ga?architecture={}&heap_size=normal&image_type=jre&jvm_impl=hotspot&os={}&page=0&page_size=1&project=jdk&sort_order=DESC&vendor=eclipse",
        platform.arch, platform.os
    );
    let client = http::client(Duration::from_secs(60))?;
    let response = http::get(&client, &url).context("Temurin metadata")?;
    let body = http::read_limited(response, 4 << 20, "Temurin metadata")?;
    let releases: Vec<Release> = serde_json::from_slice(&body)?;
    let package = releases
        .into_iter()
        .next()
        .and_then(|release| release.binaries.into_iter().next())
        .map(|binary| binary.package)
        .ok_or("no matching Temurin JRE found")?;
    if package.link.is_empty() || package.checksum.is_empty() || package.size == 0 {
        return Err("Temurin package metadata is incomplete".into());
    }
    if package.checksum.len() != 64 || hex::decode(&package.checksum).is_err() {
        return Err("Temurin package checksum is invalid".into());
    }
    Ok(package)
}

/// Downloads `url` to `dest`, showing progress, and keeps it only when its
/// size and SHA-256 match.
fn download_verified(url: &str, dest: &Path, sha256: &str, size: u64) -> Result<()> {
    let client = http::client(Duration::from_secs(15 * 60))?;
    let mut response = http::get(&client, url)?;
    let total = response.content_length();
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let result = (|| -> Result<()> {
        let mut file = fs::File::create(&tmp)?;
        let mut hasher = Sha256::new();
        let mut written = 0u64;
        let mut buffer = vec![0; 32 * 1024];
        let mut last_print: Option<Instant> = None;
        loop {
            let read = response.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])?;
            hasher.update(&buffer[..read]);
            written += read as u64;
            if last_print.is_none_or(|at| at.elapsed() > Duration::from_millis(500)) {
                ui::detail(&match total {
                    Some(total) if total > 0 => format!(
                        "download {:.0}% ({} / {})",
                        written as f64 * 100.0 / total as f64,
                        human_bytes(written),
                        human_bytes(total)
                    ),
                    _ => format!("download {}…", human_bytes(written)),
                });
                last_print = Some(Instant::now());
            }
        }
        file.sync_all()?;
        if size > 0 && written != size {
            return Err(format!("size mismatch (want {size} bytes, got {written})").into());
        }
        let got = hex::encode(hasher.finalize());
        if !got.eq_ignore_ascii_case(sha256) {
            return Err(format!("SHA-256 mismatch (want {sha256}, got {got})").into());
        }
        fs::rename(&tmp, dest)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn human_bytes(bytes: u64) -> String {
    const UNIT: u64 = 1024;
    if bytes < UNIT {
        return format!("{bytes} B");
    }
    let mut div = UNIT;
    let mut exp = 0;
    let mut n = bytes / UNIT;
    while n >= UNIT {
        div *= UNIT;
        exp += 1;
        n /= UNIT;
    }
    let prefix = ['K', 'M', 'G', 'T', 'P', 'E'][exp];
    format!("{:.1} {prefix}iB", bytes as f64 / div as f64)
}

fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let name = archive.to_string_lossy().to_lowercase();
    let root = confined::Root::open(dest)?;
    if name.ends_with(".zip") {
        unzip(archive, &root)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        untar_gz(archive, &root)
    } else {
        Err(format!("unknown archive type: {}", archive.display()).into())
    }
}

fn untar_gz(archive: &Path, root: &confined::Root) -> Result<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(archive)?));
    for entry in tar.entries()? {
        let mut entry = entry?;
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let rel = safe_archive_path(&name)?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            root.create_dir_all(&rel)?;
        } else if kind.is_file() {
            let mode = entry.header().mode().unwrap_or(0o644) & 0o777;
            let mut out = root.create_file(&rel, Some(mode))?;
            io::copy(&mut entry, &mut out)?;
        } else if kind.is_symlink() {
            let target = entry
                .link_name_bytes()
                .map(|bytes| String::from_utf8_lossy(&bytes).replace('\\', "/"))
                .unwrap_or_default();
            check_symlink(&name, &rel, &target)?;
            symlink(root, Path::new(&target), &rel)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn symlink(root: &confined::Root, target: &Path, rel: &Path) -> Result<()> {
    Ok(root.symlink(target, rel)?)
}

#[cfg(not(unix))]
fn symlink(_: &confined::Root, _: &Path, rel: &Path) -> Result<()> {
    Err(format!("symbolic links are not supported here: {}", rel.display()).into())
}

/// Refuses links that are absolute or climb out of the archive.
fn check_symlink(name: &str, rel: &Path, target: &str) -> Result<()> {
    let illegal =
        || -> crate::Error { format!("illegal symlink in archive: {name} -> {target}").into() };
    let target_path = Path::new(target);
    if target.starts_with('/')
        || target_path.is_absolute()
        || target.as_bytes().get(1) == Some(&b':')
    {
        return Err(illegal());
    }
    let mut depth: i64 = rel.components().count() as i64 - 1;
    for component in target_path.components() {
        match component {
            Component::ParentDir => depth -= 1,
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            _ => return Err(illegal()),
        }
        if depth < 0 {
            return Err(illegal());
        }
    }
    Ok(())
}

fn unzip(archive: &Path, root: &confined::Root) -> Result<()> {
    let mut zip = zip::ZipArchive::new(fs::File::open(archive)?)?;
    for number in 0..zip.len() {
        let mut entry = zip.by_index(number)?;
        let name = entry.name().to_owned();
        let rel = safe_archive_path(&name)?;
        if entry.is_dir() {
            root.create_dir_all(&rel)?;
            continue;
        }
        if entry.is_symlink() {
            return Err(format!("symbolic links are not supported in zip archive: {name}").into());
        }
        let mode = entry.unix_mode().map(|mode| mode & 0o777);
        let mut out = root.create_file(&rel, mode)?;
        io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

/// A clean relative path for an archive entry, or an error when it would
/// leave the destination.
fn safe_archive_path(name: &str) -> Result<PathBuf> {
    let illegal = || -> crate::Error { format!("illegal path in archive: {name}").into() };
    let raw = name.trim().replace('\\', "/");
    if raw.is_empty()
        || raw.starts_with('/')
        || raw.contains('\0')
        || raw.as_bytes().get(1) == Some(&b':')
    {
        return Err(illegal());
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(illegal());
                }
            }
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err(illegal());
    }
    Ok(parts.iter().collect())
}

/// Finds `bin/java` (or `java.exe`) under `root`, preferring ones in a `bin` folder.
fn find_java(root: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "java.exe" } else { "java" };
    let mut found = Vec::new();
    collect_named(root, name, &mut found);
    let java = found
        .iter()
        .find(|path| {
            path.parent()
                .and_then(Path::file_name)
                .is_some_and(|parent| parent == "bin")
        })
        .or(found.first())?
        .clone();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&java, fs::Permissions::from_mode(0o755));
    }
    Some(java)
}

fn collect_named(dir: &Path, name: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            collect_named(&path, name, found);
        } else if path.file_name().is_some_and(|file| file == name) {
            found.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::Algorithm;

    #[test]
    fn java_floors_follow_minecraft_versions() {
        let cases = [
            ("", 25),
            ("26.1", 25),
            ("26.1.2", 25),
            ("26.2", 25),
            ("27.0", 25),
            ("1.20.4", 17),
            ("1.20.5", 21),
            ("1.21", 21),
            ("1.16.5", 8),
            ("1.17", 17),
            ("1.18.2", 17),
        ];
        for (minecraft, want) in cases {
            assert_eq!(require_major(minecraft), want, "{minecraft:?}");
        }
    }

    #[test]
    fn archive_paths_stay_inside() {
        for bad in [
            "",
            ".",
            "../escape",
            "a/../../escape",
            "/absolute",
            "C:\\escape",
        ] {
            assert!(safe_archive_path(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            safe_archive_path("jdk/bin/java").unwrap(),
            Path::new("jdk").join("bin").join("java")
        );
        assert!(check_symlink("a/b", Path::new("a/b"), "../lib").is_ok());
        assert!(check_symlink("a/b", Path::new("a/b"), "../../escape").is_err());
        assert!(check_symlink("a", Path::new("a"), "/etc/passwd").is_err());
    }

    fn serve_once(body: &[u8]) -> String {
        crate::testing::serve(vec![("/runtime.tar.gz".to_owned(), body.to_vec())])
            + "/runtime.tar.gz"
    }

    #[test]
    fn downloads_keep_only_verified_runtimes() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"verified runtime archive";
        let sha256 = Algorithm::Sha256.hex_digest(&body[..]).unwrap();
        let dest = dir.path().join("runtime.tar.gz");
        download_verified(&serve_once(body), &dest, &sha256, body.len() as u64).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), body);

        let tampered = dir.path().join("tampered.tar.gz");
        let zeros = "0".repeat(64);
        assert!(download_verified(&serve_once(b"tampered"), &tampered, &zeros, 8).is_err());
        assert!(!tampered.exists());
    }
}
