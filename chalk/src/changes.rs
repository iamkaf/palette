//! Leads for porting: what the pack's JSON files use that vanilla data uses in only some
//! of the releases reading those files. Vanilla data shows every rename, reshaped value,
//! and moved folder its own files went through, so a pack using the same thing likely
//! needs a look. The table comes from server jars and ships with Chalk, so listing the
//! leads is a quick scan with no game.

use crate::check::Support;
use crate::pack::PackFormat;
use crate::versions::Versions;
use crate::{PackRoot, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use zip::ZipArchive;

const TABLE: &str = include_str!("vanilla.json");

/// Which releases' vanilla data uses what. Bit `i` of every mask stands for `releases[i]`.
#[derive(Serialize, Deserialize)]
pub struct Table {
    /// One release per data format, oldest first.
    pub releases: Vec<TableRelease>,
    /// For each folder under `data/minecraft/`, such as `loot_table` or `worldgen/biome`,
    /// the releases with files in it.
    pub folders: BTreeMap<String, u64>,
    /// For each folder, what its files use in some releases but not all: `key:<name>`,
    /// `shape:<key>:<shape>` for the kind of value a key holds, and `id:<identifier>`.
    pub features: BTreeMap<String, BTreeMap<String, u64>>,
}

#[derive(Serialize, Deserialize)]
pub struct TableRelease {
    pub version: String,
    pub format: [u32; 2],
}

impl TableRelease {
    fn format(&self) -> PackFormat {
        PackFormat {
            major: self.format[0],
            minor: self.format[1],
        }
    }
}

/// The table built into Chalk.
pub fn table() -> Result<Table> {
    Ok(serde_json::from_str(TABLE)?)
}

/// One source file and what to look into in it.
pub struct FileLeads {
    pub source: PathBuf,
    /// The releases that read the file, such as `1.21.1 to 26.2`.
    pub read_by: String,
    pub leads: Vec<String>,
}

/// Every source file with leads, in path order.
pub fn find(support: &Support, table: &Table) -> Result<Vec<FileLeads>> {
    let names = Names {
        table,
        versions: &support.versions,
    };
    let mut read: BTreeMap<&Path, (&str, u64)> = BTreeMap::new();
    for (index, release) in table.releases.iter().enumerate() {
        if !support.pack.formats.contains(release.format()) {
            continue;
        }
        for file in support.pack.files_for(release.format()) {
            read.entry(&file.source).or_insert((&file.path, 0)).1 |= 1 << index;
        }
    }

    let mut found = Vec::new();
    for (source, (path, read)) in read {
        let Some(kind) = path
            .strip_prefix("data/")
            .and_then(|rest| rest.split_once('/'))
            .filter(|_| path.ends_with(".json"))
            .and_then(|(_, rest)| kind(rest))
        else {
            continue;
        };
        // Vanilla never has files there, so there is nothing to compare with.
        let Some(&folder) = table.folders.get(&kind) else {
            continue;
        };
        let mut leads = Vec::new();
        if read & !folder != 0 {
            leads.push(format!(
                "`{kind}` files: vanilla has them {}",
                names.when(folder, read)
            ));
        } else if let Some(known) = table.features.get(&kind) {
            let mut used = BTreeSet::new();
            features(
                &serde_json::from_str(&fs::read_to_string(source)?)?,
                &mut used,
            );
            let mut keys_with_leads = BTreeSet::new();
            for feature in &used {
                let Some(&mask) = known.get(feature) else {
                    continue;
                };
                if read & !mask == 0 {
                    continue;
                }
                let what = match feature.split_once(':') {
                    Some(("key", key)) => {
                        keys_with_leads.insert(key);
                        format!("`{}`", named(key))
                    }
                    Some(("shape", rest)) => {
                        let Some((key, shape)) = rest.rsplit_once(':') else {
                            continue;
                        };
                        // Whether the key is used at all says more than its shape.
                        if keys_with_leads.contains(key) {
                            continue;
                        }
                        format!("`{}` as {}", named(key), describe(shape))
                    }
                    Some((_, id)) => format!("`{id}`"),
                    None => continue,
                };
                leads.push(format!(
                    "{what}: in vanilla `{kind}` files {}",
                    names.when(mask, read)
                ));
            }
        }
        if !leads.is_empty() {
            found.push(FileLeads {
                source: source.to_path_buf(),
                read_by: names.span(read),
                leads,
            });
        }
    }
    Ok(found)
}

/// Prints the leads with sources relative to the repository.
pub fn print(root: &PackRoot, found: &[FileLeads]) {
    for file in found {
        let source = file.source.strip_prefix(root.dir()).unwrap_or(&file.source);
        println!("  {} ({})", source.display(), file.read_by);
        for lead in &file.leads {
            println!("    {lead}");
        }
    }
}

/// Names releases the way Chalk's release list does, falling back to the table's names
/// for formats Chalk doesn't know, such as snapshots.
struct Names<'a> {
    table: &'a Table,
    versions: &'a Versions,
}

impl Names<'_> {
    fn first(&self, index: usize) -> &str {
        let release = &self.table.releases[index];
        self.versions
            .releases
            .iter()
            .find(|known| known.data_format == release.format())
            .map_or(&release.version, |known| &known.version)
    }

    fn last(&self, index: usize) -> &str {
        let release = &self.table.releases[index];
        self.versions
            .releases
            .iter()
            .rev()
            .find(|known| known.data_format == release.format())
            .map_or(&release.version, |known| &known.version)
    }

    fn span(&self, mask: u64) -> String {
        let (low, high) = (lowest(mask), highest(mask));
        if low == high && self.first(low) == self.last(high) {
            self.first(low).to_owned()
        } else {
            format!("{} to {}", self.first(low), self.last(high))
        }
    }

    /// When vanilla uses something, phrased for the releases in `read` that don't.
    fn when(&self, used: u64, read: u64) -> String {
        let missing = read & !used;
        if lowest(used) > highest(missing) {
            format!("from {}", self.first(lowest(used)))
        } else if highest(used) < lowest(missing) {
            format!("until {}", self.last(highest(used)))
        } else {
            let not: Vec<String> = (0..self.table.releases.len())
                .filter(|index| missing & (1 << index) != 0)
                .map(|index| self.span(1 << index))
                .collect();
            format!("except in {}", not.join(", "))
        }
    }
}

fn lowest(mask: u64) -> usize {
    mask.trailing_zeros() as usize
}

fn highest(mask: u64) -> usize {
    63 - mask.leading_zeros() as usize
}

/// `conditions/location` for a key inside `conditions`, or just the key at the top.
fn named(key: &str) -> &str {
    key.strip_prefix('/').unwrap_or(key)
}

fn describe(shape: &str) -> &str {
    match shape {
        "object" => "an object",
        "list" => "a list",
        "number" => "a number",
        "boolean" => "true or false",
        other => other,
    }
}

/// The folder a file under `data/<namespace>/` belongs to, such as `loot_table` or
/// `worldgen/biome`. `None` for tags and vanilla's built-in packs, which aren't compared.
fn kind(path: &str) -> Option<String> {
    let mut parts = path.split('/');
    let first = parts.next()?;
    let kind = match first {
        "tags" | "datapacks" => return None,
        "worldgen" => format!("worldgen/{}", parts.next()?),
        _ => first.to_owned(),
    };
    // Only files inside the folder count.
    parts.next().map(|_| kind)
}

/// Everything a JSON file uses that the table can record. Keys carry the key of the
/// object holding them, so a condition's `location` isn't mistaken for a predicate's.
fn features(value: &Value, out: &mut BTreeSet<String>) {
    features_in(value, "", out);
}

fn features_in(value: &Value, parent: &str, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                out.insert(format!("key:{parent}/{key}"));
                out.insert(format!("shape:{parent}/{key}:{}", shape(child)));
                features_in(child, key, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                features_in(item, parent, out);
            }
        }
        Value::String(text) if is_id(text) => {
            out.insert(format!("id:{text}"));
        }
        _ => {}
    }
}

fn shape(value: &Value) -> &'static str {
    match value {
        Value::Object(_) => "object",
        Value::Array(_) => "list",
        Value::String(_) => "text",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Null => "null",
    }
}

fn is_id(text: &str) -> bool {
    text.contains(':')
        && text.len() < 80
        && text.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-./:".contains(&byte)
        })
}

/// Builds the table from dedicated server jars, one per data format.
pub fn generate(jars: &[PathBuf]) -> Result<Table> {
    let mut read = Vec::new();
    for jar in jars {
        read.push(read_jar(jar).map_err(|error| format!("{}: {error}", jar.display()))?);
    }
    read.sort_by_key(|(release, _, _)| release.format);
    if let Some(pair) = read
        .windows(2)
        .find(|pair| pair[0].0.format == pair[1].0.format)
    {
        return Err(format!(
            "{} and {} share a data format; pass one jar per format",
            pair[0].0.version, pair[1].0.version
        )
        .into());
    }
    if read.len() > 64 {
        return Err("the table holds at most 64 data formats".into());
    }

    let all = if read.len() == 64 {
        u64::MAX
    } else {
        (1u64 << read.len()) - 1
    };
    let mut folders = BTreeMap::new();
    let mut features: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for (index, (_, release_folders, release_features)) in read.iter().enumerate() {
        for folder in release_folders {
            *folders.entry(folder.clone()).or_insert(0) |= 1 << index;
        }
        for (folder, feature) in release_features {
            *features
                .entry(folder.clone())
                .or_default()
                .entry(feature.clone())
                .or_insert(0) |= 1 << index;
        }
    }
    // Something every release uses can't differ between them.
    for known in features.values_mut() {
        known.retain(|_, mask| *mask != all);
    }
    features.retain(|_, known| !known.is_empty());
    Ok(Table {
        releases: read.into_iter().map(|(release, _, _)| release).collect(),
        folders,
        features,
    })
}

type JarData = (TableRelease, BTreeSet<String>, BTreeSet<(String, String)>);

fn read_jar(path: &Path) -> Result<JarData> {
    let mut outer = ZipArchive::new(File::open(path)?)?;
    let version: Value = serde_json::from_reader(outer.by_name("version.json")?)?;
    let id = version["id"].as_str().ok_or("version.json has no id")?;
    let pack = &version["pack_version"];
    // 1.21.9 split the data format into a major and a minor number.
    let format = match (pack["data_major"].as_u64(), pack["data"].as_u64()) {
        (Some(major), _) => [major, pack["data_minor"].as_u64().unwrap_or(0)],
        (None, Some(major)) => [major, 0],
        _ => return Err("version.json has no data pack format".into()),
    };
    let format = [
        u32::try_from(format[0]).map_err(|_| "data format out of range")?,
        u32::try_from(format[1]).map_err(|_| "data format out of range")?,
    ];

    // Server jars bundle the game itself under META-INF/versions/.
    let bundled = outer
        .file_names()
        .find(|name| name.starts_with("META-INF/versions/") && name.ends_with(".jar"))
        .map(str::to_owned)
        .ok_or("not a bundled server jar")?;
    let mut bytes = Vec::new();
    outer.by_name(&bundled)?.read_to_end(&mut bytes)?;
    let mut game = ZipArchive::new(Cursor::new(bytes))?;

    let mut folders = BTreeSet::new();
    let mut found = BTreeSet::new();
    for index in 0..game.len() {
        let mut entry = game.by_index(index)?;
        let name = entry.name().to_owned();
        let Some(kind) = name
            .strip_prefix("data/minecraft/")
            .filter(|_| name.ends_with(".json"))
            .and_then(kind)
        else {
            continue;
        };
        let mut text = String::new();
        entry.read_to_string(&mut text)?;
        let mut used = BTreeSet::new();
        features(&serde_json::from_str(&text)?, &mut used);
        found.extend(used.into_iter().map(|feature| (kind.clone(), feature)));
        folders.insert(kind);
    }
    Ok((
        TableRelease {
            version: id.to_owned(),
            format,
        },
        folders,
        found,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn server_jar(
        dir: &Path,
        version: &str,
        pack_version: &str,
        files: &[(&str, &str)],
    ) -> PathBuf {
        let mut game = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, text) in files {
            game.start_file(*name, SimpleFileOptions::default())
                .expect("file");
            game.write_all(text.as_bytes()).expect("write");
        }
        let game = game.finish().expect("game jar").into_inner();
        let path = dir.join(format!("{version}.jar"));
        let mut outer = zip::ZipWriter::new(File::create(&path).expect("jar"));
        outer
            .start_file("version.json", SimpleFileOptions::default())
            .expect("version");
        write!(
            outer,
            r#"{{"id": "{version}", "pack_version": {pack_version}}}"#
        )
        .expect("write");
        outer
            .start_file(
                format!("META-INF/versions/{version}/server-{version}.jar"),
                SimpleFileOptions::default(),
            )
            .expect("bundled");
        outer.write_all(&game).expect("write");
        outer.finish().expect("finish");
        path
    }

    #[test]
    fn a_renamed_key_is_a_lead_for_the_releases_without_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let old = server_jar(
            dir.path(),
            "1.21.11",
            r#"{"data_major": 94, "data_minor": 1}"#,
            &[
                (
                    "data/minecraft/loot_table/a.json",
                    r#"{"functions": [], "rolls": 1}"#,
                ),
                ("data/minecraft/worldgen/configured_feature/b.json", "{}"),
            ],
        );
        let new = server_jar(
            dir.path(),
            "26.1.2",
            r#"{"data_major": 101, "data_minor": 1}"#,
            &[
                (
                    "data/minecraft/loot_table/a.json",
                    r#"{"modifier": {}, "rolls": 1}"#,
                ),
                ("data/minecraft/worldgen/feature/b.json", "{}"),
            ],
        );
        let table = generate(&[new, old]).expect("table");
        assert_eq!(table.releases[0].version, "1.21.11");
        assert!(!table.features["loot_table"].contains_key("key:/rolls"));

        let repo = dir.path().join("my-pack");
        fs::create_dir_all(repo.join("datapack/data/demo/loot_table")).expect("pack");
        fs::create_dir_all(repo.join("datapack/data/demo/worldgen/configured_feature"))
            .expect("pack");
        fs::write(
            repo.join("chalk.toml"),
            "description = \"Demo\"\nminecraft = \"1.21.11-26.1.2\"\n",
        )
        .expect("manifest");
        fs::write(
            repo.join("datapack/data/demo/loot_table/drops.json"),
            r#"{"functions": [], "rolls": 1}"#,
        )
        .expect("loot table");
        fs::write(
            repo.join("datapack/data/demo/worldgen/configured_feature/rock.json"),
            "{}",
        )
        .expect("feature");
        let support = check::support(&PackRoot::at(&repo).expect("root")).expect("support");

        let found = find(&support, &table).expect("leads");

        let leads: Vec<(&str, &Vec<String>)> = found
            .iter()
            .map(|file| (file.read_by.as_str(), &file.leads))
            .collect();
        assert_eq!(
            leads,
            [
                (
                    "1.21.11 to 26.1.2",
                    &vec!["`functions`: in vanilla `loot_table` files until 1.21.11".to_owned()]
                ),
                (
                    "1.21.11 to 26.1.2",
                    &vec![
                        "`worldgen/configured_feature` files: vanilla has them until 1.21.11"
                            .to_owned()
                    ]
                ),
            ]
        );
    }

    #[test]
    fn the_built_in_table_reads() {
        let table = table().expect("table");
        assert!(table.releases.len() >= 2);
    }
}
