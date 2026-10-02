//! The `server.pastel` instance file.

use crate::paths;
use crate::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "server.pastel";

const AUTO_RESTART_COMMENT: &str = "# Restart the server after an unexpected crash.\n";

/// The local instance file. It points at a pack and holds runtime settings.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// A Modrinth pin, `.mrpack` path or URL, `file:` URL, or Maven coordinate.
    pub pack: String,
    /// The `-Xmx` value, such as `4G`.
    pub memory: String,
    /// The Java executable. Empty means `java` on `PATH`.
    pub java: String,
    /// Ordered Maven repository bases for short coordinates; the first hit
    /// wins. There is no default host.
    pub repositories: Vec<String>,
    /// The Minecraft server root, relative to `server.pastel`.
    pub server_dir: String,
    /// JVM arguments added after `-Xmx`.
    pub extra_java_args: Vec<String>,
    pub nogui: Option<bool>,
    /// When false, `pastel run` starts without re-applying pack files.
    pub sync_on_run: Option<bool>,
    /// Restart a background server that crashed after it was ready.
    pub auto_restart: Option<bool>,
    #[serde(skip)]
    path: PathBuf,
}

impl Config {
    /// The absolute path of the loaded file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The absolute server root.
    pub fn root(&self) -> PathBuf {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        if self.server_dir.is_empty() {
            return dir.to_path_buf();
        }
        paths::clean(&dir.join(&self.server_dir))
    }

    pub fn java_bin(&self) -> &str {
        if self.java.is_empty() {
            "java"
        } else {
            &self.java
        }
    }

    /// The memory value without an `-Xmx` prefix.
    pub fn xmx(&self) -> String {
        if self.memory.is_empty() {
            "4G".to_owned()
        } else {
            self.memory.trim_start_matches("-Xmx").to_owned()
        }
    }

    pub fn nogui(&self) -> bool {
        self.nogui.unwrap_or(true)
    }

    pub fn sync_on_run(&self) -> bool {
        self.sync_on_run.unwrap_or(true)
    }

    pub fn auto_restart(&self) -> bool {
        self.auto_restart.unwrap_or(true)
    }

    /// Ordered Maven bases. Empty when unset: Pastel never invents a host.
    pub fn maven_repositories(&self) -> Vec<String> {
        crate::maven::normalize_repositories(&self.repositories)
    }

    /// Points the file at another pack, keeping everything else in it.
    pub fn set_pack(&mut self, pin: &str) -> Result<()> {
        let text = std::fs::read_to_string(&self.path)?;
        let mut document: toml_edit::DocumentMut = text.parse()?;
        document["pack"] = toml_edit::value(pin);
        std::fs::write(&self.path, document.to_string())?;
        self.pack = pin.to_owned();
        Ok(())
    }
}

/// Reads `server.pastel` from a file, or from a directory that contains one.
pub fn load(path: &Path) -> Result<Config> {
    let metadata = std::fs::metadata(path).context("server.pastel")?;
    let path = if metadata.is_dir() {
        path.join(FILE_NAME)
    } else {
        path.to_path_buf()
    };
    let path = paths::absolute(&path)?;
    let text = std::fs::read_to_string(&path).context(format_args!("read {}", path.display()))?;
    let mut config: Config = toml::from_str::<toml::Table>(&text)
        .and_then(|table| lowercase_keys(table).try_into())
        .context(format_args!("parse {}", path.display()))?;
    config.path = path;
    if config.pack.trim().is_empty() {
        return Err(format!("{}: pack is required", config.path.display()).into());
    }
    if config.auto_restart.is_none() {
        // Older files predate the setting. Write its default so it is discoverable.
        append_auto_restart_default(&config.path, &text).context(format_args!(
            "add auto_restart to {}",
            config.path.display()
        ))?;
        config.auto_restart = Some(true);
    }
    Ok(config)
}

/// Go Pastel matched keys ignoring case, so `Memory` still sets `memory`. An
/// exact lowercase key wins over other spellings of the same name.
fn lowercase_keys(table: toml::Table) -> toml::Table {
    let (exact, other): (Vec<_>, Vec<_>) = table
        .into_iter()
        .partition(|(key, _)| *key == key.to_lowercase());
    let mut keys = toml::Table::new();
    for (key, value) in exact.into_iter().chain(other) {
        keys.entry(key.to_lowercase()).or_insert(value);
    }
    keys
}

fn append_auto_restart_default(path: &Path, text: &str) -> std::io::Result<()> {
    let mut updated = text.to_owned();
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(AUTO_RESTART_COMMENT);
    updated.push_str("auto_restart = true\n");
    // Rewriting in place keeps the file's permissions.
    std::fs::write(path, updated)
}

/// Creates or overwrites a minimal `server.pastel` for a first install.
pub fn write(path: &Path, pack: &str, memory: &str, repositories: &[String]) -> Result<()> {
    if pack.trim().is_empty() {
        return Err("pack is required".into());
    }
    let memory = if memory.is_empty() { "4G" } else { memory };
    let mut text = String::from("# Pastel server pin — https://kaf.sh\n");
    text.push_str(&format!("pack = {}\n", toml_string(pack)));
    text.push_str(&format!("memory = {}\n", toml_string(memory)));
    // Explicit defaults let people discover they can flip them while debugging.
    text.push_str("# Set false so ./pastel run starts without re-downloading the pack.\n");
    text.push_str("sync_on_run = true\n");
    text.push_str(AUTO_RESTART_COMMENT);
    text.push_str("auto_restart = true\n");
    let repositories = crate::maven::normalize_repositories(repositories);
    if !repositories.is_empty() {
        text.push_str("repositories = [\n");
        for repository in repositories {
            text.push_str(&format!("  {},\n", toml_string(&repository)));
        }
        text.push_str("]\n");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

fn toml_string(value: &str) -> String {
    toml_edit::Value::from(value).to_string()
}

/// Walks up from `start` looking for `server.pastel`.
pub fn find(start: &Path) -> Result<PathBuf> {
    let start = paths::absolute(start)?;
    for dir in start.ancestors() {
        let candidate = dir.join(FILE_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!("no {FILE_NAME} found from {}", start.display()).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn loads_defaults_without_inventing_a_maven_host() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(
            dir.path(),
            "pack = \"com.example.modpacks:example-pack:1.1.0\"\nmemory = \"4G\"\n",
        );
        let config = load(&path).unwrap();
        assert_eq!(config.pack, "com.example.modpacks:example-pack:1.1.0");
        assert_eq!(config.xmx(), "4G");
        assert_eq!(config.root(), paths::absolute(dir.path()).unwrap());
        assert!(config.maven_repositories().is_empty());
        assert!(config.sync_on_run());
    }

    #[test]
    fn written_files_seed_discoverable_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        write(&path, "modrinth:example", "4G", &[]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("sync_on_run = true"));
        assert!(text.contains("auto_restart = true"));
        let config = load(&path).unwrap();
        assert!(config.sync_on_run() && config.auto_restart());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn load_adds_a_missing_auto_restart_once_and_keeps_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "pack = \"modrinth:example\"");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        assert!(load(&path).unwrap().auto_restart());
        load(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("auto_restart = true").count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o640);
        }
    }

    #[test]
    fn load_keeps_an_explicit_auto_restart() {
        let dir = tempfile::tempdir().unwrap();
        let text = "pack = \"modrinth:example\"\nauto_restart = false\n";
        let path = write_file(dir.path(), text);
        assert!(!load(&path).unwrap().auto_restart());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn keys_match_ignoring_case_like_go_pastel() {
        let dir = tempfile::tempdir().unwrap();
        let text = "Pack = \"modrinth:example\"\nMemory = \"8G\"\nAuto_Restart = false\n";
        let path = write_file(dir.path(), text);
        let config = load(&path).unwrap();
        assert_eq!(config.xmx(), "8G");
        assert!(!config.auto_restart());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn repositories_are_ordered_and_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(
            dir.path(),
            r#"
pack = "com.example.modpacks:example-pack:1.1.0"
repositories = [
  "https://maven.example.com/",
  "https://repo.example.org",
  "https://maven.example.com",
]
"#,
        );
        assert_eq!(
            load(&path).unwrap().maven_repositories(),
            ["https://maven.example.com", "https://repo.example.org"]
        );
    }

    #[test]
    fn written_pins_survive_quotes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let pin = r#"https://example.invalid/pack"name.mrpack"#;
        write(&path, pin, "4G", &[]).unwrap();
        assert_eq!(load(&path).unwrap().pack, pin);
    }

    #[test]
    fn set_pack_keeps_comments_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        write(&path, "modrinth:example:1.0.0", "6G", &[]).unwrap();
        let mut config = load(&path).unwrap();
        config.set_pack("modrinth:example:1.1.0").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Pastel server pin"));
        assert!(text.contains("pack = \"modrinth:example:1.1.0\"\n"));
        assert!(text.contains("memory = \"6G\""));
        assert_eq!(load(&path).unwrap().pack, "modrinth:example:1.1.0");
    }
}
