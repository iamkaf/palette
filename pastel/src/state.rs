//! Last-apply metadata and process tracking files under `.pastel/`.

use crate::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// Written after a successful refresh.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pack_coordinate: String,
    #[serde(default)]
    pub pack_name: String,
    #[serde(default)]
    pub pack_version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub minecraft: String,
    /// Display name such as `Fabric`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub loader: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mod_count: usize,
    #[serde(default)]
    pub applied_at: DateTime<Utc>,
    #[serde(default)]
    pub file_count: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

pub fn dir(root: &Path) -> PathBuf {
    root.join(".pastel")
}

pub fn path(root: &Path) -> PathBuf {
    dir(root).join("state.json")
}

/// The Minecraft server's process ID.
pub fn pid_path(root: &Path) -> PathBuf {
    dir(root).join("server.pid")
}

/// The process that keeps the console FIFO open.
pub fn hold_pid_path(root: &Path) -> PathBuf {
    dir(root).join("hold.pid")
}

/// The Pastel process that owns and restarts the server.
pub fn supervisor_pid_path(root: &Path) -> PathBuf {
    dir(root).join("supervisor.pid")
}

/// The console command transport for a background server. On macOS and Linux
/// this is a FIFO; on Windows it is a small file holding the address of an
/// owner-restricted named pipe.
pub fn console_in_path(root: &Path) -> PathBuf {
    dir(root).join("console.in")
}

/// Where the server's stdout and stderr are captured.
pub fn console_log_path(root: &Path) -> PathBuf {
    dir(root).join("console.log")
}

/// Reads state, or `None` when nothing has been applied yet.
pub fn load(root: &Path) -> Result<Option<State>> {
    match std::fs::read(path(root)) {
        Ok(data) => Ok(Some(serde_json::from_slice(&data)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn save(root: &Path, state: &State) -> Result<()> {
    std::fs::create_dir_all(dir(root))?;
    let mut data = serde_json::to_string_pretty(state)?;
    data.push('\n');
    std::fs::write(path(root), data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_state_written_by_the_go_release() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir(root.path())).unwrap();
        std::fs::write(
            path(root.path()),
            r#"{
  "packCoordinate": "modrinth:forever-world:1.1.0",
  "packName": "FOREVER WORLD",
  "packVersion": "1.1.0",
  "minecraft": "26.2",
  "loader": "Fabric",
  "modCount": 48,
  "appliedAt": "2026-09-30T19:12:41.639123456Z",
  "fileCount": 61
}
"#,
        )
        .unwrap();
        let state = load(root.path()).unwrap().unwrap();
        assert_eq!(state.pack_version, "1.1.0");
        assert_eq!(state.mod_count, 48);
        assert_eq!(state.applied_at.timestamp(), 1_790_795_561);

        save(root.path(), &state).unwrap();
        let written = std::fs::read_to_string(path(root.path())).unwrap();
        assert!(written.contains("\"appliedAt\": \"2026-09-30T19:12:41.639123456Z\""));
        assert!(written.ends_with("}\n"));
    }
}
