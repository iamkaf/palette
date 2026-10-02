//! The server-side pack model derived from a Modrinth `.mrpack`.

mod loader;
mod mrpack;
mod pin;
mod resolve;

pub use loader::{ensure_loader, installed_launch_jar};
pub use mrpack::{LoadedMrpack, MrpackIndex, override_mod_jars, validate_path};
pub use pin::{MavenRef, PinKind, UpdateCheck, check_update, classify_pin, is_maven_coordinate};
pub use resolve::{Resolved, resolve};

use crate::Result;
use crate::fetch::Algorithm;
use crate::paths;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loader {
    Fabric,
    NeoForge,
    Forge,
    Quilt,
    Vanilla,
}

impl Loader {
    /// The display name, such as `NeoForge`.
    pub fn name(self) -> &'static str {
        match self {
            Loader::Fabric => "Fabric",
            Loader::NeoForge => "NeoForge",
            Loader::Forge => "Forge",
            Loader::Quilt => "Quilt",
            Loader::Vanilla => "Vanilla",
        }
    }

    /// The `.mrpack` dependency key, such as `neoforge`.
    pub fn id(self) -> &'static str {
        match self {
            Loader::Fabric => "fabric",
            Loader::NeoForge => "neoforge",
            Loader::Forge => "forge",
            Loader::Quilt => "quilt",
            Loader::Vanilla => "vanilla",
        }
    }
}

/// The server-side desired state of a pack.
#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub dependencies: HashMap<String, String>,
    pub files: Vec<PackFile>,
    /// How to start the server. [`ensure_loader`] sets it.
    pub launch: Option<Launch>,
}

/// One managed path in the server tree.
#[derive(Debug, Clone, Default)]
pub struct PackFile {
    /// Slash-separated and relative to the server root.
    pub path: String,
    /// Lowercase algorithm names to lowercase hex digests.
    pub hashes: BTreeMap<String, String>,
    pub downloads: Vec<String>,
    pub file_size: u64,
}

impl PackFile {
    /// The strongest published digest.
    pub fn preferred_hash(&self) -> Result<(Algorithm, &str)> {
        for algorithm in Algorithm::ALL {
            if let Some(hex) = self.hashes.get(algorithm.key())
                && !hex.is_empty()
            {
                return Ok((algorithm, hex));
            }
        }
        match self.hashes.iter().find(|(_, hex)| !hex.is_empty()) {
            Some((name, _)) => Err(format!("unsupported hash algorithm {name:?}").into()),
            None => Err(format!("{}: no hash", self.path).into()),
        }
    }
}

/// How to start the dedicated server.
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub kind: Loader,
    pub target: LaunchTarget,
    /// Forge and NeoForge's `user_jvm_args.txt`, relative to the server root.
    pub jvm_args_file: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LaunchTarget {
    /// `-jar <jar>`, relative to the server root.
    Jar(String),
    /// `@<args file>`, relative to the server root.
    ArgsFile(String),
}

impl Manifest {
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err("manifest name is required".into());
        }
        if self.version.is_empty() {
            return Err("manifest version is required".into());
        }
        for (index, file) in self.files.iter().enumerate() {
            validate_path(&file.path).map_err(|error| error.context(format!("files[{index}]")))?;
            if file.hashes.is_empty() {
                return Err(format!("files[{index}]: at least one hash is required").into());
            }
            if file.downloads.is_empty() {
                return Err(format!("files[{index}]: downloads is required").into());
            }
        }
        Ok(())
    }

    pub fn minecraft(&self) -> &str {
        self.dependency("minecraft")
            .or_else(|| self.dependency("Minecraft"))
            .unwrap_or("")
    }

    /// A non-empty dependency version.
    pub fn dependency(&self, key: &str) -> Option<&str> {
        self.dependencies
            .get(key)
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }

    /// The loader the pack declares, if any.
    pub fn loader(&self) -> Option<Loader> {
        if self.dependency("fabric-loader").is_some() || self.dependency("fabric_loader").is_some()
        {
            Some(Loader::Fabric)
        } else if self.dependency("neoforge").is_some() {
            Some(Loader::NeoForge)
        } else if self.dependency("forge").is_some() {
            Some(Loader::Forge)
        } else if self.dependency("quilt-loader").is_some() {
            Some(Loader::Quilt)
        } else {
            None
        }
    }

    /// The loader display name, or empty for vanilla packs.
    pub fn loader_name(&self) -> &'static str {
        self.loader().map_or("", Loader::name)
    }

    /// The launch kind: the installed launcher's, else the declared loader's.
    pub fn resolved_kind(&self) -> Loader {
        match &self.launch {
            Some(launch) => launch.kind,
            None => self.loader().unwrap_or(Loader::Vanilla),
        }
    }

    /// Counts the pack's jars under `mods/`.
    pub fn mod_count(&self) -> usize {
        self.files
            .iter()
            .filter(|file| {
                file.path.starts_with("mods/") && file.path.to_lowercase().ends_with(".jar")
            })
            .count()
    }

    /// The arguments after the Java executable.
    pub fn java_args(
        &self,
        root: &Path,
        xmx: &str,
        jvm_args: &[String],
        nogui: bool,
    ) -> Result<Vec<String>> {
        let launch = self
            .launch
            .as_ref()
            .ok_or("the pack's loader is not installed yet; run ./pastel refresh")?;
        let xmx = if xmx.is_empty() { "4G" } else { xmx };
        let mut args = vec![format!("-Xmx{xmx}")];
        args.extend(jvm_args.iter().cloned());
        // -Xmx comes first so the server.pastel memory wins where Forge and
        // NeoForge's user_jvm_args.txt allows an override.
        if let Some(rel) = &launch.jvm_args_file {
            let path = paths::join_slash(root, rel);
            if path.is_file() {
                args.push(format!("@{}", path.display()));
            }
        }
        match &launch.target {
            LaunchTarget::ArgsFile(rel) => {
                let path = existing_args_file(root, rel)?;
                args.push(format!("@{}", path.display()));
            }
            LaunchTarget::Jar(rel) => {
                let path = paths::join_slash(root, rel);
                if !path.is_file() {
                    return Err(format!("launch jar not found: {rel}").into());
                }
                args.push("-jar".to_owned());
                args.push(path.display().to_string());
            }
        }
        if nogui {
            args.push("nogui".to_owned());
        }
        Ok(args)
    }
}

/// `rel`, or its Unix or Windows counterpart, under `root`.
fn existing_args_file(root: &Path, rel: &str) -> Result<std::path::PathBuf> {
    let alternate = if let Some(base) = rel.strip_suffix("unix_args.txt") {
        Some(format!("{base}win_args.txt"))
    } else {
        rel.strip_suffix("win_args.txt")
            .map(|base| format!("{base}unix_args.txt"))
    };
    std::iter::once(rel.to_owned())
        .chain(alternate)
        .map(|candidate| paths::join_slash(root, &candidate))
        .find(|path| path.is_file())
        .ok_or_else(|| format!("launch args file not found: {rel}").into())
}

/// The args file Forge and NeoForge installers write for this platform.
pub fn preferred_args_file_name() -> &'static str {
    if cfg!(windows) {
        "win_args.txt"
    } else {
        "unix_args.txt"
    }
}

/// Whether `name` is a root-level server launcher jar that Pastel may prune
/// when the pack no longer uses it.
pub fn is_managed_root_jar(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".jar")
        && (lower == "server.jar"
            || lower == "forge-server.jar"
            || [
                "fabric-server-",
                "quilt-server-",
                "forge-",
                "neoforge-",
                "minecraft_server.",
            ]
            .iter()
            .any(|prefix| lower.starts_with(prefix)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> PackFile {
        PackFile {
            path: path.to_owned(),
            hashes: BTreeMap::from([("sha256".to_owned(), "aa".to_owned())]),
            downloads: vec!["https://example.com/foo".to_owned()],
            file_size: 0,
        }
    }

    fn manifest(files: Vec<PackFile>) -> Manifest {
        Manifest {
            name: "x".to_owned(),
            version: "1".to_owned(),
            files,
            ..Manifest::default()
        }
    }

    #[test]
    fn validate_prefers_the_strongest_hash() {
        let mut example = file("mods/example.jar");
        example.hashes.insert("sha512".to_owned(), "abc".to_owned());
        let manifest = manifest(vec![example]);
        manifest.validate().unwrap();
        let (algorithm, hex) = manifest.files[0].preferred_hash().unwrap();
        assert_eq!((algorithm, hex), (Algorithm::Sha512, "abc"));
    }

    #[test]
    fn validate_rejects_reserved_server_paths() {
        for path in [
            "world/level.dat",
            "World/region/r.0.0.mca",
            ".pastel/state.json",
            "server.pastel",
        ] {
            assert!(manifest(vec![file(path)]).validate().is_err(), "{path}");
        }
    }

    #[test]
    fn validate_rejects_escapes_and_missing_downloads() {
        assert!(manifest(vec![file("mods/../secret")]).validate().is_err());
        let mut missing = file("mods/example.jar");
        missing.downloads.clear();
        assert!(manifest(vec![missing]).validate().is_err());
    }

    fn launch(target: LaunchTarget, jvm_args_file: Option<&str>) -> Manifest {
        Manifest {
            launch: Some(Launch {
                kind: Loader::Fabric,
                target,
                jvm_args_file: jvm_args_file.map(String::from),
            }),
            ..Manifest::default()
        }
    }

    #[test]
    fn java_args_for_a_launcher_jar() {
        let root = tempfile::tempdir().unwrap();
        let jar = "fabric-server-mc.26.1.2-loader.0.19.2-launcher.1.1.1.jar";
        std::fs::write(root.path().join(jar), "x").unwrap();
        let args = launch(LaunchTarget::Jar(jar.to_owned()), None)
            .java_args(root.path(), "4G", &["-XX:+UseG1GC".to_owned()], true)
            .unwrap();
        let jar_path = root.path().join(jar).display().to_string();
        assert_eq!(args, ["-Xmx4G", "-XX:+UseG1GC", "-jar", &jar_path, "nogui"]);

        let args = launch(LaunchTarget::Jar(jar.to_owned()), None)
            .java_args(root.path(), "4G", &[], false)
            .unwrap();
        assert_ne!(args.last().map(String::as_str), Some("nogui"));
    }

    #[test]
    fn java_args_for_an_args_file_fall_back_to_the_other_platform() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("libraries/net/neoforged/neoforge/21.0.0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("win_args.txt"), "-p libraries\n").unwrap();
        std::fs::write(root.path().join("user_jvm_args.txt"), "# jvm\n").unwrap();
        let args = launch(
            LaunchTarget::ArgsFile(
                "libraries/net/neoforged/neoforge/21.0.0/unix_args.txt".to_owned(),
            ),
            Some("user_jvm_args.txt"),
        )
        .java_args(root.path(), "6G", &[], true)
        .unwrap();
        assert_eq!(args[0], "-Xmx6G");
        assert_eq!(
            args[1],
            format!("@{}", root.path().join("user_jvm_args.txt").display())
        );
        assert_eq!(args[2], format!("@{}", dir.join("win_args.txt").display()));
        assert_eq!(args[3], "nogui");
    }

    #[test]
    fn recognizes_managed_root_jars() {
        for name in [
            "fabric-server-mc.26.1.2-loader.0.19.2-launcher.1.1.1.jar",
            "neoforge-21.0.0-universal.jar",
            "server.jar",
        ] {
            assert!(is_managed_root_jar(name), "{name}");
        }
        assert!(!is_managed_root_jar("lithium.jar"));
    }

    #[test]
    fn reads_the_loader_from_dependencies() {
        let manifest = Manifest {
            dependencies: HashMap::from([
                ("neoforge".to_owned(), "21.0.0".to_owned()),
                ("minecraft".to_owned(), "1.21.1".to_owned()),
            ]),
            ..Manifest::default()
        };
        assert_eq!(manifest.loader(), Some(Loader::NeoForge));
        assert_eq!(manifest.resolved_kind(), Loader::NeoForge);
        assert_eq!(manifest.minecraft(), "1.21.1");
    }
}
