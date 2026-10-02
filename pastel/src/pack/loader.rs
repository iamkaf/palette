//! Installs the server launcher a pack's loader needs.

use super::{Launch, LaunchTarget, Loader, Manifest, preferred_args_file_name};
use crate::{Context, Result, http, jre, paths};
use regex::Regex;
use serde::Deserialize;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;
use std::time::Duration;

/// `fabric-server-mc.{mc}-loader.{loader}-launcher.{installer}.jar`
static FABRIC_LAUNCHER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^fabric-server-mc\.(.+)-loader\.(.+)-launcher\.(.+)\.jar$")
        .expect("valid Fabric launcher pattern")
});

/// Installs the loader the pack declares when it's missing, and sets
/// `manifest.launch`. Returns whether anything was installed.
///
/// An installed launcher is reused only when it matches the pack's exact
/// Minecraft and loader versions, so upgrades and loader switches never start
/// a stale one.
pub fn ensure_loader(root: &Path, manifest: &mut Manifest) -> Result<bool> {
    let kind = manifest.resolved_kind();
    if let Some(launch) = installed_launch(root, manifest)? {
        if let (Loader::Fabric, LaunchTarget::Jar(jar)) = (kind, &launch.target) {
            remove_other_fabric_launchers(root, Some(jar));
        }
        manifest.launch = Some(launch);
        return Ok(false);
    }
    let minecraft = manifest.minecraft();
    let launch = match kind {
        Loader::Fabric => {
            // Drop launchers from older pack versions before installing.
            remove_other_fabric_launchers(root, None);
            jar_launch(
                kind,
                install_fabric_server_jar(root, minecraft, &fabric_loader(manifest)?)?,
            )
        }
        Loader::NeoForge | Loader::Forge => {
            let java = jre::ensure(root, jre::require_major(minecraft), None)?;
            install_forge_family_server(
                root,
                kind,
                &forge_artifact_version(manifest, kind)?,
                &java,
            )?
        }
        Loader::Quilt => {
            return Err("quilt loader install is not automatic yet; include quilt-server-launch.jar in the pack or overrides".into());
        }
        Loader::Vanilla => {
            return Err("vanilla pack needs server.jar in the pack or overrides".into());
        }
    };
    manifest.launch = Some(launch);
    Ok(true)
}

/// The root launcher jar a refresh keeps for this pack, found without
/// installing or removing anything. Refresh previews use it.
pub fn installed_launch_jar(root: &Path, manifest: &Manifest) -> Option<String> {
    match installed_launch(root, manifest).ok()??.target {
        LaunchTarget::Jar(jar) => Some(jar),
        LaunchTarget::ArgsFile(_) => None,
    }
}

/// The launch an already-installed loader provides for exactly this pack.
fn installed_launch(root: &Path, manifest: &Manifest) -> Result<Option<Launch>> {
    let kind = manifest.resolved_kind();
    Ok(match kind {
        Loader::Fabric => {
            let (minecraft, loader) = (manifest.minecraft(), fabric_loader(manifest)?);
            root_file_names(root)
                .into_iter()
                .find(|name| fabric_jar_matches(name, minecraft, &loader))
                .map(|jar| jar_launch(kind, jar))
        }
        Loader::NeoForge | Loader::Forge => {
            let artifact_version = forge_artifact_version(manifest, kind)?;
            args_file_launch(root, kind, &artifact_version).or_else(|| {
                // Forge before 1.17 has no args file; a pack may ship its server jar instead.
                generic_server_jar(root)
                    .filter(|(_, found)| *found == kind)
                    .map(|(jar, _)| jar_launch(kind, jar))
            })
        }
        Loader::Quilt => existing_jar(root, kind, "quilt-server-launch.jar"),
        Loader::Vanilla => existing_jar(root, kind, "server.jar"),
    })
}

fn jar_launch(kind: Loader, jar: String) -> Launch {
    Launch {
        kind,
        target: LaunchTarget::Jar(jar),
        jvm_args_file: None,
    }
}

fn existing_jar(root: &Path, kind: Loader, name: &str) -> Option<Launch> {
    root.join(name)
        .is_file()
        .then(|| jar_launch(kind, name.to_owned()))
}

fn fabric_loader(manifest: &Manifest) -> Result<String> {
    let loader = manifest
        .dependency("fabric-loader")
        .or_else(|| manifest.dependency("fabric_loader"));
    match loader {
        Some(loader) if !manifest.minecraft().is_empty() => Ok(loader.to_owned()),
        _ => Err("fabric pack needs dependencies.minecraft and fabric-loader".into()),
    }
}

fn forge_artifact_version(manifest: &Manifest, kind: Loader) -> Result<String> {
    let version = manifest
        .dependency(kind.id())
        .ok_or_else(|| format!("{0} pack needs dependencies.{0}", kind.id()))?;
    Ok(installer_artifact_version(
        kind,
        manifest.minecraft(),
        version,
    ))
}

/// Downloads the official installer and runs `--installServer`.
fn install_forge_family_server(
    root: &Path,
    kind: Loader,
    artifact_version: &str,
    java: &Path,
) -> Result<Launch> {
    let cache = root.join(".pastel").join("cache").join("installers");
    fs::create_dir_all(&cache)?;
    let installer = cache.join(format!("{}-{artifact_version}-installer.jar", kind.id()));
    if fs::metadata(&installer).map_or(true, |metadata| metadata.len() == 0) {
        download_file(&installer_url(kind, artifact_version), &installer)
            .context(format_args!("download {} installer", kind.id()))?;
    }
    // Official installers write libraries/ and the args files under the server root.
    let output = Command::new(java)
        .arg("-jar")
        .arg(&installer)
        .args(["--installServer", "."])
        .current_dir(root)
        .output()
        .context(format_args!("{} installer failed", kind.id()))?;
    if !output.status.success() {
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        let text = text.trim();
        let start = text
            .char_indices()
            .rev()
            .nth(1999)
            .map_or(0, |(index, _)| index);
        return Err(format!(
            "{} installer failed: {}\n{}",
            kind.id(),
            output.status,
            &text[start..]
        )
        .into());
    }
    // Memory still comes from server.pastel's -Xmx.
    let user_jvm_args = root.join("user_jvm_args.txt");
    if !user_jvm_args.is_file() {
        fs::write(
            &user_jvm_args,
            "# Managed by Pastel — memory is set via server.pastel\n",
        )?;
    }
    args_file_launch(root, kind, artifact_version).ok_or_else(|| {
        format!(
            "{} installer did not create an args file for {artifact_version}",
            kind.id()
        )
        .into()
    })
}

fn installer_artifact_version(kind: Loader, minecraft: &str, version: &str) -> String {
    if kind == Loader::Forge && !minecraft.is_empty() && !version.contains('-') {
        format!("{minecraft}-{version}")
    } else {
        version.to_owned()
    }
}

fn installer_url(kind: Loader, version: &str) -> String {
    if kind == Loader::NeoForge {
        format!(
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/{version}/neoforge-{version}-installer.jar"
        )
    } else {
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{version}/forge-{version}-installer.jar"
        )
    }
}

/// The installed args file for exactly this loader version.
fn args_file_launch(root: &Path, kind: Loader, artifact_version: &str) -> Option<Launch> {
    let dir = if kind == Loader::NeoForge {
        format!("libraries/net/neoforged/neoforge/{artifact_version}")
    } else {
        format!("libraries/net/minecraftforge/forge/{artifact_version}")
    };
    [preferred_args_file_name(), "unix_args.txt", "win_args.txt"]
        .iter()
        .map(|base| format!("{dir}/{base}"))
        .find(|rel| paths::join_slash(root, rel).is_file())
        .map(|rel| Launch {
            kind,
            target: LaunchTarget::ArgsFile(rel),
            jvm_args_file: root
                .join("user_jvm_args.txt")
                .is_file()
                .then(|| "user_jvm_args.txt".to_owned()),
        })
}

fn root_file_names(root: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| !kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// A pack-provided Forge or NeoForge server jar, and which loader it belongs to.
fn generic_server_jar(root: &Path) -> Option<(String, Loader)> {
    for name in root_file_names(root) {
        let lower = name.to_lowercase();
        if !lower.ends_with(".jar") {
            continue;
        }
        if lower.starts_with("neoforge-") && lower.contains("universal") {
            return Some((name, Loader::NeoForge));
        }
        if lower.starts_with("forge-") && (lower.contains("universal") || lower.contains("shim")) {
            return Some((name, Loader::Forge));
        }
    }
    [
        ("forge-server.jar", Loader::Forge),
        ("server.jar", Loader::Vanilla),
    ]
    .into_iter()
    .find(|(name, _)| root.join(name).is_file())
    .map(|(name, kind)| (name.to_owned(), kind))
}

/// Whether `name` is a Fabric launcher for these Minecraft and loader versions.
/// The installer build may differ.
fn fabric_jar_matches(name: &str, minecraft: &str, loader: &str) -> bool {
    FABRIC_LAUNCHER
        .captures(name)
        .is_some_and(|captures| &captures[1] == minecraft && &captures[2] == loader)
}

/// Deletes root `fabric-server-*.jar` files other than `keep`.
fn remove_other_fabric_launchers(root: &Path, keep: Option<&str>) {
    for name in root_file_names(root) {
        if name.starts_with("fabric-server-")
            && name.ends_with(".jar")
            && Some(name.as_str()) != keep
        {
            let _ = fs::remove_file(root.join(name));
        }
    }
}

fn install_fabric_server_jar(root: &Path, minecraft: &str, loader: &str) -> Result<String> {
    let installer = latest_fabric_installer()?;
    let name = format!("fabric-server-mc.{minecraft}-loader.{loader}-launcher.{installer}.jar");
    let url = format!(
        "https://meta.fabricmc.net/v2/versions/loader/{minecraft}/{loader}/{installer}/server/jar"
    );
    download_file(&url, &root.join(&name)).context("download Fabric server jar")?;
    Ok(name)
}

fn latest_fabric_installer() -> Result<String> {
    #[derive(Deserialize)]
    struct Installer {
        #[serde(default)]
        version: String,
        #[serde(default)]
        stable: bool,
    }
    let client = http::client(Duration::from_secs(60))?;
    let response = http::get(&client, "https://meta.fabricmc.net/v2/versions/installer")
        .context("fabric installer meta")?;
    let installers: Vec<Installer> = serde_json::from_reader(response)?;
    installers
        .iter()
        .find(|installer| installer.stable && !installer.version.is_empty())
        .or(installers.first())
        .map(|installer| installer.version.clone())
        .ok_or_else(|| "fabric installer meta empty".into())
}

fn download_file(url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let client = http::client(Duration::from_secs(600))?;
    let mut response = http::get(&client, url)?;
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".pastel-tmp");
    let result = (|| -> io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        io::copy(&mut response, &mut file)?;
        file.sync_all()?;
        fs::rename(&tmp, dest)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(result?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn installer_coordinates() {
        assert_eq!(
            installer_artifact_version(Loader::Forge, "1.20.1", "47.2.0"),
            "1.20.1-47.2.0"
        );
        assert_eq!(
            installer_artifact_version(Loader::Forge, "1.20.1", "1.20.1-47.2.0"),
            "1.20.1-47.2.0"
        );
        assert_eq!(
            installer_artifact_version(Loader::NeoForge, "26.1.2", "21.1.0"),
            "21.1.0"
        );
        assert_eq!(
            installer_url(Loader::NeoForge, "21.1.172"),
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/21.1.172/neoforge-21.1.172-installer.jar"
        );
        assert_eq!(
            installer_url(Loader::Forge, "1.20.1-47.2.0"),
            "https://maven.minecraftforge.net/net/minecraftforge/forge/1.20.1-47.2.0/forge-1.20.1-47.2.0-installer.jar"
        );
    }

    #[test]
    fn fabric_launchers_match_minecraft_and_loader() {
        let name = "fabric-server-mc.26.2-loader.0.19.3-launcher.1.1.1.jar";
        assert!(fabric_jar_matches(name, "26.2", "0.19.3"));
        assert!(!fabric_jar_matches(name, "26.1.2", "0.19.3"));
        assert!(!fabric_jar_matches(name, "26.2", "0.19.2"));
    }

    #[test]
    fn a_matching_fabric_launcher_is_reused_and_stale_ones_removed() {
        let root = tempfile::tempdir().unwrap();
        // A leftover 26.1.2 launcher from an older pack version.
        let stale = "fabric-server-mc.26.1.2-loader.0.19.2-launcher.1.1.1.jar";
        let good = "fabric-server-mc.26.2-loader.0.19.3-launcher.9.9.9.jar";
        fs::write(root.path().join(stale), "old").unwrap();
        fs::write(root.path().join(good), "new").unwrap();
        let mut manifest = Manifest {
            name: "t".to_owned(),
            version: "1".to_owned(),
            dependencies: HashMap::from([
                ("minecraft".to_owned(), "26.2".to_owned()),
                ("fabric-loader".to_owned(), "0.19.3".to_owned()),
            ]),
            ..Manifest::default()
        };
        assert!(!ensure_loader(root.path(), &mut manifest).unwrap());
        assert_eq!(
            manifest.launch.map(|launch| launch.target),
            Some(LaunchTarget::Jar(good.to_owned()))
        );
        assert!(!root.path().join(stale).exists());
        assert!(root.path().join(good).exists());
    }

    #[test]
    fn only_the_exact_neoforge_version_is_reused() {
        let root = tempfile::tempdir().unwrap();
        // 21.1.99 sorts after 21.1.200 as text; neither may stand in for the other.
        for version in ["21.1.99", "21.1.200"] {
            let dir = root
                .path()
                .join(format!("libraries/net/neoforged/neoforge/{version}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(preferred_args_file_name()), "-p libraries\n").unwrap();
        }
        let mut manifest = Manifest {
            dependencies: HashMap::from([
                ("minecraft".to_owned(), "1.21.1".to_owned()),
                ("neoforge".to_owned(), "21.1.200".to_owned()),
            ]),
            ..Manifest::default()
        };
        assert!(!ensure_loader(root.path(), &mut manifest).unwrap());
        let want = format!(
            "libraries/net/neoforged/neoforge/21.1.200/{}",
            preferred_args_file_name()
        );
        assert_eq!(
            manifest.launch.map(|launch| launch.target),
            Some(LaunchTarget::ArgsFile(want))
        );
        assert!(args_file_launch(root.path(), Loader::NeoForge, "21.1.300").is_none());
    }
}
