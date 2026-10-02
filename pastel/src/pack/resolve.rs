use super::mrpack::is_index_json;
use super::{LoadedMrpack, Manifest, PinKind, classify_pin};
use crate::fetch::Algorithm;
use crate::maven::{self, Coordinate};
use crate::{Context, Result, http, modrinth};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_PACK_BYTES: u64 = 512 << 20;

/// A pack ready to apply.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub manifest: Manifest,
    /// The pin recorded in state, such as `modrinth:aristea:0.2.0`.
    pub coordinate: String,
    pub mrpack: LoadedMrpack,
}

/// Loads a pack from a Modrinth pin, Maven coordinate, local path, `file:`
/// URL, or https URL. Remote packs are cached in `cache_dir` so their
/// overrides can be applied; without one, only the index is read.
pub fn resolve(raw: &str, repositories: &[String], cache_dir: Option<&Path>) -> Result<Resolved> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("empty pack reference".into());
    }
    match classify_pin(raw) {
        PinKind::File => {
            let path = strip_file_url(raw);
            resolve_path(Path::new(path), format!("file:{path}"))
        }
        PinKind::Url => resolve_url(raw, cache_dir),
        PinKind::Modrinth => resolve_modrinth_pin(raw, cache_dir),
        PinKind::Maven => resolve_maven(raw, repositories, cache_dir),
        PinKind::Path if Path::new(raw).exists() => resolve_path(Path::new(raw), format!("file:{raw}")),
        PinKind::Path => Err(format!(
            "unrecognized pack reference {raw:?} (want .mrpack path/URL, modrinth:slug, slug@version, or Maven coordinate)"
        )
        .into()),
    }
}

/// A Modrinth page URL, `modrinth:` ref, `slug@version` shorthand, or bare slug.
fn resolve_modrinth_pin(raw: &str, cache_dir: Option<&Path>) -> Result<Resolved> {
    let (slug, version, pin) = if let Some((slug, version)) = modrinth::parse_page_url(raw) {
        let pin = format!("modrinth:{slug}");
        (slug, version, pin)
    } else if let Some((slug, version)) = modrinth::parse_ref(raw) {
        (slug, version, raw.to_owned())
    } else if let Some((slug, version)) = modrinth::parse_slug_version(raw) {
        let pin = modrinth::pin(&slug, &version);
        (slug, version, pin)
    } else {
        (raw.to_owned(), String::new(), modrinth::pin(raw, ""))
    };
    let pack = modrinth::Client::new()?
        .resolve_modpack(&slug, &version)
        .context("modrinth")?;
    // Verify the size and hash Modrinth published before parsing or caching.
    let data = http_get(&pack.file.url).context(format_args!("fetch pack {}", pack.file.url))?;
    if pack.file.size > 0 && data.len() as u64 != pack.file.size {
        return Err(format!(
            "modrinth pack size mismatch (want {} bytes, got {})",
            pack.file.size,
            data.len()
        )
        .into());
    }
    verify_modrinth_pack(&data, &pack.file.hashes)?;
    let mut resolved = resolve_bytes(&data, pin, cache_dir)?;
    // Prefer the API's title and version when the index leaves them out.
    if resolved.manifest.name.is_empty() {
        resolved.manifest.name = pack.project.title;
    }
    if resolved.manifest.version.is_empty() {
        resolved.manifest.version = pack.version.version_number;
    }
    Ok(resolved)
}

fn verify_modrinth_pack(
    data: &[u8],
    hashes: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    for algorithm in Algorithm::ALL {
        let want = hashes
            .get(algorithm.key())
            .map(|hex| hex.trim().to_lowercase())
            .unwrap_or_default();
        if want.is_empty() {
            continue;
        }
        if algorithm.hex_digest(data)? != want {
            return Err(format!("modrinth pack {} mismatch", algorithm.label()).into());
        }
        return Ok(());
    }
    Err("modrinth pack metadata has no supported checksum".into())
}

fn strip_file_url(raw: &str) -> &str {
    raw.get(..7)
        .filter(|prefix| prefix.eq_ignore_ascii_case("file://"))
        .map(|_| &raw[7..])
        .or_else(|| {
            raw.get(..5)
                .filter(|prefix| prefix.eq_ignore_ascii_case("file:"))
                .map(|_| &raw[5..])
        })
        .unwrap_or(raw)
}

fn resolve_path(path: &Path, coordinate: String) -> Result<Resolved> {
    let lower = path.to_string_lossy().to_lowercase();
    if path.is_dir() || lower.ends_with(".mrpack") || lower.ends_with("modrinth.index.json") {
        return Ok(resolved(LoadedMrpack::load(path)?, coordinate));
    }
    // Look at the content of anything else.
    let data = std::fs::read(path)?;
    if data.starts_with(b"PK")
        && let Ok(pack) = LoadedMrpack::load(path)
    {
        return Ok(resolved(pack, coordinate));
    }
    if is_index_json(&data) {
        return Ok(resolved(LoadedMrpack::load(path)?, coordinate));
    }
    Err("pack is not a Modrinth .mrpack".into())
}

fn resolve_url(raw: &str, cache_dir: Option<&Path>) -> Result<Resolved> {
    // The pack decides which jars the server runs, so it must not travel over plain HTTP.
    if !raw.to_lowercase().starts_with("https://") {
        return Err(format!("pack URLs must use https://: {raw}").into());
    }
    let data = http_get(raw).context(format_args!("fetch pack {raw}"))?;
    resolve_bytes(&data, raw.to_owned(), cache_dir)
}

fn resolve_maven(raw: &str, repositories: &[String], cache_dir: Option<&Path>) -> Result<Resolved> {
    let mut coordinate = Coordinate::parse(raw)?;
    let repositories = maven::normalize_repositories(repositories);
    if repositories.is_empty() {
        return Err(format!(
            "{} — pack {raw:?} is a Maven coordinate",
            maven::NO_REPOSITORIES
        )
        .into());
    }
    let client = maven::Client::new(&repositories)?;
    if coordinate.version == "latest" {
        coordinate.version = client
            .latest_version(&coordinate.group, &coordinate.artifact)
            .context("resolve latest")?;
    }
    let display = coordinate.display();
    let data = client
        .fetch_pack(&coordinate)
        .context(format_args!("fetch pack {display}"))?;
    resolve_bytes(&data, display, cache_dir)
}

fn resolve_bytes(data: &[u8], coordinate: String, cache_dir: Option<&Path>) -> Result<Resolved> {
    let pack = LoadedMrpack::from_bytes(data)?;
    if !data.starts_with(b"PK") {
        return Ok(resolved(pack, coordinate));
    }
    let Some(cache_dir) = cache_dir else {
        // Callers that only inspect the index, such as install's probe, skip the cache.
        return Ok(resolved(pack, coordinate));
    };
    // Overrides are read from the cached zip, so a pack that can't be cached
    // must fail loudly instead of silently skipping them.
    let path = cache_pack(cache_dir, data).context("cache pack")?;
    Ok(resolved(LoadedMrpack::load(&path)?, coordinate))
}

fn resolved(mrpack: LoadedMrpack, coordinate: String) -> Resolved {
    Resolved {
        manifest: mrpack.manifest(),
        coordinate,
        mrpack,
    }
}

fn cache_pack(cache_dir: &Path, data: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(cache_dir)?;
    let digest = Sha256::digest(data);
    let path = cache_dir.join(format!("{}.mrpack", hex::encode(&digest[..16])));
    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() == data.len() as u64) {
        return Ok(path);
    }
    let mut tmp = path.clone().into_os_string();
    tmp.push(".tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

fn http_get(url: &str) -> Result<Vec<u8>> {
    let client = http::client(Duration::from_secs(600))?;
    http::read_limited(http::get(&client, url)?, MAX_PACK_BYTES, "pack")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modrinth_packs_must_match_a_published_hash() {
        let body = b"mrpack";
        let digest = Algorithm::Sha512.hex_digest(&body[..]).unwrap();
        let hashes = std::collections::BTreeMap::from([("sha512".to_owned(), digest)]);
        verify_modrinth_pack(body, &hashes).unwrap();
        assert!(verify_modrinth_pack(b"tampered", &hashes).is_err());
        assert!(verify_modrinth_pack(body, &Default::default()).is_err());
    }

    #[test]
    fn resolves_a_local_mrpack() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pack.mrpack");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file(
            "modrinth.index.json",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        std::io::Write::write_all(
            &mut zip,
            br#"{"formatVersion": 1, "versionId": "1.0.0", "name": "Zip Pack"}"#,
        )
        .unwrap();
        zip.finish().unwrap();
        let resolved = resolve(path.to_str().unwrap(), &[], None).unwrap();
        assert_eq!(resolved.manifest.name, "Zip Pack");
        assert_eq!(resolved.coordinate, format!("file:{}", path.display()));
    }

    #[test]
    fn strips_file_urls() {
        assert_eq!(
            strip_file_url("file:///tmp/pack.mrpack"),
            "/tmp/pack.mrpack"
        );
        assert_eq!(strip_file_url("FILE:pack.mrpack"), "pack.mrpack");
    }
}
