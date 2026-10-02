//! Modrinth `.mrpack` files: `modrinth.index.json` and override layers.

use super::{Manifest, PackFile};
use crate::confined;
use crate::{Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use zip::ZipArchive;

const INDEX: &str = "modrinth.index.json";

/// Override layers a dedicated server applies, in order.
const SERVER_LAYERS: [&str; 2] = ["overrides", "server-overrides"];

/// `modrinth.index.json`, format version 1.
/// <https://support.modrinth.com/en/articles/8802351-modrinth-modpack-format-mrpack>
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MrpackIndex {
    pub format_version: u32,
    pub game: String,
    pub version_id: String,
    pub name: String,
    pub files: Vec<MrpackFile>,
    pub dependencies: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MrpackFile {
    pub path: String,
    pub hashes: HashMap<String, String>,
    pub env: Option<MrpackEnv>,
    pub downloads: Vec<String>,
    pub file_size: u64,
}

/// `required`, `optional`, or `unsupported` per side.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MrpackEnv {
    pub client: String,
    pub server: String,
}

/// Where a loaded pack's override layers live.
#[derive(Debug, Clone)]
enum Overrides {
    Zip(PathBuf),
    /// An extracted pack or a bare index next to its override folders.
    Dir(PathBuf),
    /// Parsed from memory only, for inspecting the index.
    None,
}

#[derive(Debug, Clone)]
pub struct LoadedMrpack {
    pub index: MrpackIndex,
    overrides: Overrides,
}

impl LoadedMrpack {
    /// Loads a `.mrpack` zip, a `modrinth.index.json`, or a folder containing one.
    pub fn load(path: &Path) -> Result<Self> {
        if path.is_dir() {
            let data = fs::read(path.join(INDEX)).context("mrpack dir")?;
            return Ok(Self {
                index: parse_index(&data)?,
                overrides: Overrides::Dir(path.to_path_buf()),
            });
        }
        let data = fs::read(path)?;
        if path.to_string_lossy().to_lowercase().ends_with(".mrpack") || is_zip(&data) {
            return Ok(Self {
                index: index_from_zip(&data)?,
                overrides: Overrides::Zip(path.to_path_buf()),
            });
        }
        if is_index_json(&data) {
            return Ok(Self {
                index: parse_index(&data)?,
                overrides: Overrides::Dir(path.parent().unwrap_or(Path::new(".")).to_path_buf()),
            });
        }
        Err(format!("{} is not a .mrpack or {INDEX}", path.display()).into())
    }

    /// Reads a pack from memory. Override layers are unavailable.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let index = if is_zip(data) {
            index_from_zip(data)?
        } else if is_index_json(data) {
            parse_index(data)?
        } else {
            return Err("pack is not a Modrinth .mrpack".into());
        };
        Ok(Self {
            index,
            overrides: Overrides::None,
        })
    }

    /// The dedicated server's view: files whose `env.server` isn't `unsupported`,
    /// optional ones included, since there is nobody to pick them.
    pub fn manifest(&self) -> Manifest {
        let index = &self.index;
        let files = index
            .files
            .iter()
            .filter(|file| {
                file.env
                    .as_ref()
                    .is_none_or(|env| !env.server.trim().eq_ignore_ascii_case("unsupported"))
            })
            .map(|file| PackFile {
                path: file.path.clone(),
                hashes: file
                    .hashes
                    .iter()
                    .map(|(algorithm, hex)| (algorithm.to_lowercase(), hex.to_lowercase()))
                    .collect::<BTreeMap<_, _>>(),
                downloads: file.downloads.clone(),
                file_size: file.file_size,
            })
            .collect();
        Manifest {
            name: index.name.clone(),
            version: index.version_id.clone(),
            dependencies: index.dependencies.clone(),
            files,
            launch: None,
        }
    }

    /// Copies `overrides/` and then `server-overrides/` into `root`. Returns the
    /// slash-separated paths written.
    pub fn apply_overrides(&self, root: &Path) -> Result<Vec<String>> {
        let mut written = Vec::new();
        match &self.overrides {
            Overrides::Zip(zip_path) => {
                let mut archive = ZipArchive::new(fs::File::open(zip_path)?)?;
                let root = confined::Root::open(root)?;
                for layer in SERVER_LAYERS {
                    extract_layer(&mut archive, layer, &root, &mut written)?;
                }
            }
            Overrides::Dir(dir) => {
                let root = confined::Root::open(root)?;
                for layer in SERVER_LAYERS {
                    let source = dir.join(layer);
                    if source.exists() {
                        copy_tree(&source, &source, &root, &mut written)?;
                    }
                }
            }
            Overrides::None => {}
        }
        Ok(written)
    }

    /// Jar names that override layers would place in `mods/`, without extracting.
    pub fn list_override_mod_jars(&self) -> Result<Vec<String>> {
        let mut paths = Vec::new();
        match &self.overrides {
            Overrides::Zip(zip_path) => {
                let archive = ZipArchive::new(fs::File::open(zip_path)?)?;
                for layer in SERVER_LAYERS {
                    let prefix = format!("{layer}/");
                    for name in archive.file_names() {
                        if let Some(rel) = name.strip_prefix(&prefix)
                            && !name.ends_with('/')
                        {
                            paths.push(rel.to_owned());
                        }
                    }
                }
            }
            Overrides::Dir(dir) => {
                for layer in SERVER_LAYERS {
                    let source = dir.join(layer);
                    // A missing layer is normal; any other failure would let
                    // pruning delete override jars.
                    if source.exists() {
                        list_tree(&source, &source, &mut paths)?;
                    }
                }
            }
            Overrides::None => {}
        }
        Ok(override_mod_jars(&paths))
    }
}

/// File names of jars written to `mods/` by override layers.
pub fn override_mod_jars(written: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    written
        .iter()
        .filter(|path| path.starts_with("mods/") && path.to_lowercase().ends_with(".jar"))
        .map(|path| path.rsplit('/').next().unwrap_or(path).to_owned())
        .filter(|name| seen.insert(name.clone()))
        .collect()
}

fn is_zip(data: &[u8]) -> bool {
    data.len() >= 4 && data.starts_with(b"PK")
}

/// Whether `data` looks like `modrinth.index.json`.
pub(super) fn is_index_json(data: &[u8]) -> bool {
    let text = String::from_utf8_lossy(data);
    let text = text.trim();
    if !text.starts_with('{') {
        return false;
    }
    #[derive(Deserialize)]
    #[serde(default, rename_all = "camelCase")]
    #[derive(Default)]
    struct Probe {
        format_version: u32,
        version_id: String,
        game: String,
    }
    serde_json::from_str::<Probe>(text).is_ok_and(|probe| {
        probe.format_version > 0
            || !probe.version_id.is_empty()
            || probe.game.eq_ignore_ascii_case("minecraft")
    })
}

/// Parses and lightly validates `modrinth.index.json`.
fn parse_index(data: &[u8]) -> Result<MrpackIndex> {
    let mut index: MrpackIndex = serde_json::from_slice(data).context("mrpack index")?;
    match index.format_version {
        0 => index.format_version = 1,
        1 => {}
        other => {
            return Err(format!("unsupported mrpack formatVersion {other} (want 1)").into());
        }
    }
    if index.name.is_empty() {
        return Err("mrpack: name is required".into());
    }
    if index.version_id.is_empty() {
        return Err("mrpack: versionId is required".into());
    }
    for (number, file) in index.files.iter().enumerate() {
        validate_path(&file.path).map_err(|error| error.context(format!("files[{number}]")))?;
        if file.hashes.is_empty() {
            return Err(format!("files[{number}]: at least one hash is required").into());
        }
        if file.downloads.is_empty() {
            return Err(format!("files[{number}]: downloads is required").into());
        }
    }
    Ok(index)
}

fn index_from_zip(data: &[u8]) -> Result<MrpackIndex> {
    let mut archive = ZipArchive::new(Cursor::new(data)).context("mrpack zip")?;
    for number in 0..archive.len() {
        let mut entry = archive.by_index(number)?;
        let name = entry.name();
        if name == INDEX || name.ends_with(&format!("/{INDEX}")) {
            let mut index = Vec::new();
            entry.read_to_end(&mut index)?;
            return parse_index(&index);
        }
    }
    Err(format!("mrpack zip missing {INDEX}").into())
}

/// Checks that a pack path is a clean, relative, slash-separated path that
/// leaves the world and Pastel's own files alone.
pub fn validate_path(path: &str) -> Result<()> {
    let path = path.trim();
    if path.is_empty() {
        return Err("path is required".into());
    }
    if path.contains('\0') {
        return Err("path must not contain NUL".into());
    }
    // Rejecting backslashes keeps a pack meaning the same thing on Windows.
    if path.contains('\\') {
        return Err("path must use forward slashes".into());
    }
    if path.starts_with('/') {
        return Err("path must be relative".into());
    }
    if path.as_bytes().get(1) == Some(&b':') {
        return Err("path must not be absolute".into());
    }
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("path contains an invalid component".into());
    }
    // Folding case also covers case-insensitive filesystems, where World/ is world/.
    let top = path.split('/').next().unwrap_or(path).to_lowercase();
    if matches!(top.as_str(), "world" | ".pastel" | "server.pastel") {
        return Err(format!("path {path:?} is reserved for the server").into());
    }
    Ok(())
}

fn extract_layer<R: io::Read + io::Seek>(
    archive: &mut ZipArchive<R>,
    layer: &str,
    root: &confined::Root,
    written: &mut Vec<String>,
) -> Result<()> {
    let prefix = format!("{layer}/");
    for number in 0..archive.len() {
        let mut entry = archive.by_index(number)?;
        let name = entry.name().to_owned();
        let Some(rel) = name.strip_prefix(&prefix) else {
            continue;
        };
        // Zip tools often add directory entries with a trailing slash, such as
        // server-overrides/config/bonded/.
        let is_dir = entry.is_dir();
        let rel = rel.trim_end_matches('/');
        if rel.is_empty() {
            continue;
        }
        validate_path(rel).map_err(|error| error.context(format!("override {name}")))?;
        let rel_path = crate::paths::join_slash(Path::new(""), rel);
        if is_dir {
            root.create_dir_all(&rel_path)?;
            continue;
        }
        let mut out = root.create_file(&rel_path, Some(0o644))?;
        io::copy(&mut entry, &mut out)?;
        written.push(rel.to_owned());
    }
    Ok(())
}

fn copy_tree(
    base: &Path,
    dir: &Path,
    root: &confined::Root,
    written: &mut Vec<String>,
) -> Result<()> {
    for path in sorted_entries(dir)? {
        let rel = slash_relative(base, &path);
        validate_path(&rel).map_err(|error| error.context(format!("override {rel}")))?;
        let file_type = fs::symlink_metadata(&path)?.file_type();
        let rel_path = crate::paths::join_slash(Path::new(""), &rel);
        if file_type.is_symlink() {
            return Err(format!("override {rel}: symbolic links are not supported").into());
        }
        if file_type.is_dir() {
            root.create_dir_all(&rel_path)?;
            copy_tree(base, &path, root, written)?;
            continue;
        }
        let mut input = fs::File::open(&path)?;
        let mut out = root.create_file(&rel_path, Some(0o644))?;
        io::copy(&mut input, &mut out)?;
        written.push(rel);
    }
    Ok(())
}

fn list_tree(base: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for path in sorted_entries(dir)? {
        let file_type = fs::symlink_metadata(&path)?.file_type();
        if file_type.is_dir() {
            list_tree(base, &path, out)?;
        } else {
            out.push(slash_relative(base, &path));
        }
    }
    Ok(())
}

fn sorted_entries(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut entries = fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    entries.sort();
    Ok(entries)
}

fn slash_relative(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn server_view_drops_client_only_files() {
        let index = parse_index(
            br#"{
  "formatVersion": 1,
  "game": "minecraft",
  "versionId": "1.0.0",
  "name": "Test Pack",
  "dependencies": {"minecraft": "26.2", "fabric-loader": "0.19.3"},
  "files": [
    {"path": "mods/server-mod.jar", "hashes": {"sha512": "aa", "sha1": "bb"},
     "downloads": ["https://cdn.modrinth.com/data/x/versions/y/server-mod.jar"],
     "fileSize": 1, "env": {"client": "unsupported", "server": "required"}},
    {"path": "mods/client-only.jar", "hashes": {"sha512": "cc"},
     "downloads": ["https://cdn.modrinth.com/data/x/versions/y/client-only.jar"],
     "env": {"client": "required", "server": "unsupported"}},
    {"path": "mods/both.jar", "hashes": {"SHA512": "DD"},
     "downloads": ["https://cdn.modrinth.com/data/x/versions/y/both.jar"]}
  ]
}"#,
        )
        .unwrap();
        let manifest = LoadedMrpack {
            index,
            overrides: Overrides::None,
        }
        .manifest();
        assert_eq!(
            (manifest.name.as_str(), manifest.version.as_str()),
            ("Test Pack", "1.0.0")
        );
        assert_eq!(manifest.minecraft(), "26.2");
        assert_eq!(manifest.loader_name(), "Fabric");
        let paths: Vec<&str> = manifest
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(paths, ["mods/server-mod.jar", "mods/both.jar"]);
        assert_eq!(manifest.files[1].hashes["sha512"], "dd");
        assert_eq!(manifest.mod_count(), 2);
    }

    #[test]
    fn rejects_escaping_and_unportable_paths() {
        let index = br#"{"versionId": "1", "name": "x", "files": [{"path": "mods/../evil.jar",
            "hashes": {"sha512": "aa"}, "downloads": ["https://example.com/x"]}]}"#;
        assert!(parse_index(index).is_err());
        for bad in [
            "/absolute",
            "../escape",
            "mods/../escape",
            "mods\\escape.jar",
            "mods//escape.jar",
            "C:/escape",
        ] {
            assert!(validate_path(bad).is_err(), "{bad}");
        }
        validate_path("mods/a..b.jar").unwrap();
    }

    fn write_pack(path: &Path, files: &[(&str, &str)], dirs: &[&str]) {
        let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let options = SimpleFileOptions::default();
        zip.start_file(INDEX, options).unwrap();
        zip.write_all(
            br#"{"formatVersion": 1, "game": "minecraft", "versionId": "9.9.9", "name": "Zip Pack",
  "dependencies": {"minecraft": "26.2", "fabric-loader": "0.19.3"},
  "files": [{"path": "mods/demo.jar", "hashes": {"sha512": "00", "sha1": "11"},
    "downloads": ["https://cdn.modrinth.com/data/x/versions/y/demo.jar"], "fileSize": 1,
    "env": {"client": "required", "server": "required"}}]}"#,
        )
        .unwrap();
        // Directory entries first, like common zip tools.
        for dir in dirs {
            zip.add_directory(*dir, options).unwrap();
        }
        for (name, body) in files {
            zip.start_file(*name, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn applies_zip_overrides_with_server_overrides_last() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("test.mrpack");
        write_pack(
            &zip_path,
            &[
                ("overrides/eula.txt", "eula=true\n"),
                ("overrides/config/demo.toml", "client=1\n"),
                ("overrides/mods/from-override.jar", "fake-jar"),
                ("server-overrides/config/demo.toml", "server=1\n"),
            ],
            &[],
        );
        let pack = LoadedMrpack::load(&zip_path).unwrap();
        let manifest = pack.manifest();
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].path, "mods/demo.jar");

        let root = dir.path().join("server");
        fs::create_dir(&root).unwrap();
        let written = pack.apply_overrides(&root).unwrap();
        assert_eq!(written.len(), 4);
        assert_eq!(override_mod_jars(&written), ["from-override.jar"]);
        assert_eq!(
            pack.list_override_mod_jars().unwrap(),
            ["from-override.jar"]
        );
        assert!(root.join("mods/from-override.jar").is_file());
        assert!(root.join("eula.txt").is_file());
        assert_eq!(
            fs::read_to_string(root.join("config/demo.toml")).unwrap(),
            "server=1\n"
        );
    }

    #[test]
    fn zip_directory_entries_are_created() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("dirs.mrpack");
        write_pack(
            &zip_path,
            &[("server-overrides/config/bonded/bonded-common.toml", "x=1\n")],
            &[
                "server-overrides/config/",
                "server-overrides/config/bonded/",
                "server-overrides/config/empty-only/",
            ],
        );
        let root = dir.path().join("server");
        fs::create_dir(&root).unwrap();
        LoadedMrpack::load(&zip_path)
            .unwrap()
            .apply_overrides(&root)
            .unwrap();
        assert!(root.join("config/bonded/bonded-common.toml").is_file());
        assert!(root.join("config/empty-only").is_dir());
    }

    #[test]
    fn folder_overrides_are_copied_and_listed() {
        let dir = tempfile::tempdir().unwrap();
        let pack_dir = dir.path().join("pack");
        fs::create_dir_all(pack_dir.join("server-overrides/mods")).unwrap();
        fs::write(
            pack_dir.join(INDEX),
            r#"{"formatVersion": 1, "versionId": "1", "name": "Folder Pack"}"#,
        )
        .unwrap();
        fs::write(pack_dir.join("server-overrides/mods/local.jar"), "jar").unwrap();
        let pack = LoadedMrpack::load(&pack_dir).unwrap();
        assert_eq!(pack.list_override_mod_jars().unwrap(), ["local.jar"]);
        let root = dir.path().join("server");
        fs::create_dir(&root).unwrap();
        assert_eq!(pack.apply_overrides(&root).unwrap(), ["mods/local.jar"]);
        assert!(root.join("mods/local.jar").is_file());
    }
}
