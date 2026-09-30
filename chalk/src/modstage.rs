//! Writes Modstage configs for Chalk's game runs and cleans up after them.

use crate::Result;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, value};

/// One Minecraft instance with the pack zip in its world.
pub struct Instance<'a> {
    pub name: String,
    pub minecraft: &'a str,
    pub loader: Loader<'a>,
    pub sides: &'a [&'a str],
    pub mods: Vec<String>,
    /// A server port other than the default 25565.
    pub server_port: Option<u16>,
    pub archive: &'a Path,
    /// The zip's file name in `world/datapacks/`.
    pub archive_name: String,
}

pub enum Loader<'a> {
    Vanilla,
    Fabric(&'a str),
}

/// A Modstage config for `instances`, with Maven repositories for TeaKit.
pub fn config(project: &str, instances: &[Instance<'_>]) -> String {
    let mut document = DocumentMut::new();

    let mut project_table = Table::new();
    project_table.insert("name", value(project));
    document.insert("project", Item::Table(project_table));

    let mut repositories = Table::new();
    repositories.insert("mavenLocal", value("mavenLocal"));
    repositories.insert("kaf", value("https://maven.kaf.sh"));
    document.insert("repositories", Item::Table(repositories));

    let mut tables = ArrayOfTables::new();
    for instance in instances {
        // Modstage only reads server_properties as an inline table.
        let mut server_properties = InlineTable::new();
        for (key, property) in [
            ("online-mode", "false"),
            ("enforce-secure-profile", "false"),
            // Minecraft 26.3 turns the whitelist on by default.
            ("white-list", "false"),
            ("gamemode", "creative"),
            ("spawn-protection", "0"),
        ] {
            server_properties.insert(key, property.into());
        }
        if let Some(port) = instance.server_port {
            server_properties.insert("server-port", port.to_string().into());
        }

        let mut fixture = Table::new();
        fixture.insert(
            "from",
            value(instance.archive.to_string_lossy().into_owned()),
        );
        fixture.insert(
            "to",
            value(format!("world/datapacks/{}", instance.archive_name)),
        );
        fixture.insert("side", value("server"));
        fixture.insert("replace", value(true));
        let mut fixtures = ArrayOfTables::new();
        fixtures.push(fixture);

        let mut table = Table::new();
        table.insert("name", value(instance.name.as_str()));
        table.insert("minecraft", value(instance.minecraft));
        match instance.loader {
            Loader::Vanilla => {
                table.insert("loader", value("vanilla"));
            }
            Loader::Fabric(version) => {
                table.insert("loader", value("fabric"));
                table.insert("loader_version", value(version));
            }
        }
        table.insert(
            "sides",
            value(instance.sides.iter().copied().collect::<Array>()),
        );
        table.insert("server_properties", value(server_properties));
        if !instance.mods.is_empty() {
            table.insert(
                "mods",
                value(instance.mods.iter().map(String::as_str).collect::<Array>()),
            );
        }
        table.insert("fixture", Item::ArrayOfTables(fixtures));
        tables.push(table);
    }
    document.insert("instance", Item::ArrayOfTables(tables));
    document.to_string()
}

/// A local TCP port nothing is listening on right now.
pub fn free_port() -> Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

/// Stops Minecraft processes still running in an instance. A dedicated server can
/// outlive its launcher when its shutdown hangs, as 1.21.1 sometimes does while saving
/// chunks, and it keeps the world locked so the next run can't start. Only Linux exposes
/// process working directories this way; elsewhere this does nothing.
pub fn stop_leftovers(config: &Path, instance: &str) -> Result<()> {
    let Some(dir) = instance_dir(config, instance)? else {
        return Ok(());
    };
    for pid in processes_in(&dir) {
        println!("  stopping Minecraft process {pid}, which outlived its run");
        signal(pid, "TERM");
        for _ in 0..20 {
            if !alive(pid) {
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
        if alive(pid) {
            signal(pid, "KILL");
        }
    }
    Ok(())
}

/// Asks Modstage where an instance lives. `None` before its first run.
pub fn instance_dir(config: &Path, instance: &str) -> Result<Option<PathBuf>> {
    let output = Command::new("modstage")
        .arg("--config")
        .arg(config)
        .args(["inspect", "instance", instance])
        .output()
        .map_err(|error| format!("running modstage: {error}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .find_map(|line| line.strip_prefix("instance_dir = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .map(PathBuf::from))
}

fn processes_in(dir: &Path) -> Vec<u32> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != std::process::id())
        .filter(|pid| {
            fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| cwd.starts_with(dir))
        })
        .collect()
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn signal(pid: u32, name: &str) {
    let _ = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(pid.to_string())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_properties_are_an_inline_table_so_modstage_reads_them() {
        let archive = Path::new("my-pack.zip");
        let config: DocumentMut = config(
            "my-pack",
            &[Instance {
                name: "load-26.3".into(),
                minecraft: "26.3",
                loader: Loader::Vanilla,
                sides: &["server"],
                mods: Vec::new(),
                server_port: Some(25700),
                archive,
                archive_name: "my-pack.zip".into(),
            }],
        )
        .parse()
        .expect("valid TOML");

        let properties = config["instance"][0]["server_properties"]
            .as_inline_table()
            .expect("inline server_properties");
        assert_eq!(
            properties
                .get("white-list")
                .and_then(|value| value.as_str()),
            Some("false")
        );
        assert_eq!(
            properties
                .get("server-port")
                .and_then(|value| value.as_str()),
            Some("25700")
        );
        assert!(config["instance"][0].get("mods").is_none());
    }
}
