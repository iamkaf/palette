//! Swatch's side of a release: its lockfile checks and the modpack files it builds. The
//! shared publisher prepares, verifies, and uploads them.

use crate::spec::{Loader as PackLoader, Lockfile};
use crate::{PackRoot, Result, TOOL_NAME, USER_AGENT};
use palette_publish::{
    ArtifactKind, Content, Loader, Pack, Project, PublishConfig, Tool, Workspace,
};
use std::path::{Path, PathBuf};

pub use palette_publish::{PublishMode, ReleaseManifest};

const TOOL: Tool = Tool {
    command: TOOL_NAME,
    user_agent: USER_AGENT,
};

/// Prepares the release once and writes `build/dist/release.json`.
pub fn prepare_release(root: &PackRoot) -> Result<PathBuf> {
    Ok(palette_publish::prepare_release(
        &workspace(root),
        &Modpack::load(root)?,
    )?)
}

/// Checks that `build/dist/release.json` still describes the prepared files.
pub fn verify_release(root: &PackRoot) -> Result<ReleaseManifest> {
    Ok(palette_publish::verify_release(
        &workspace(root),
        &Modpack::load(root)?,
    )?)
}

/// Publishes the prepared files, or previews them with a dry run.
pub fn publish(root: &PackRoot, mode: PublishMode) -> Result<Vec<String>> {
    Ok(palette_publish::publish(
        &workspace(root),
        &Modpack::load(root)?,
        mode,
    )?)
}

fn workspace(root: &PackRoot) -> Workspace {
    Workspace {
        tool: TOOL,
        root: root.path.clone(),
        dist: "build/dist".into(),
        manifest: root.pack_toml(),
    }
}

/// A pack with its lockfile read once, so every release step sees the same pins.
struct Modpack<'a> {
    root: &'a PackRoot,
    lock: Lockfile,
}

impl<'a> Modpack<'a> {
    fn load(root: &'a PackRoot) -> Result<Self> {
        Ok(Self {
            root,
            lock: crate::load_lock(root)?,
        })
    }
}

impl Pack for Modpack<'_> {
    fn check(&self) -> palette_publish::Result<()> {
        let manifest =
            std::fs::read_to_string(self.root.pack_toml()).map_err(crate::Error::from)?;
        let spec = crate::spec::PackSpec::parse(&manifest)?;
        if !crate::resolve::lock_matches_spec(&spec, &self.lock) {
            return Err(
                "pack.toml changed since the last install; run `swatch install` and prepare again"
                    .into(),
            );
        }
        crate::authored::verify(self.root, &self.lock.authored)?;
        Ok(())
    }

    fn project(&self) -> palette_publish::Result<Project> {
        let pack = &self.lock.pack;
        Ok(Project {
            name: pack.name.clone(),
            slug: pack.slug.clone(),
            group: pack.group.clone(),
            version: pack.version.clone(),
            game_versions: vec![pack.minecraft.clone()],
            content: Content::Modpack(match pack.loader {
                PackLoader::Fabric => Loader::Fabric,
                PackLoader::Forge => Loader::Forge,
                PackLoader::NeoForge => Loader::NeoForge,
            }),
        })
    }

    fn build(
        &self,
        _project: &Project,
        config: &PublishConfig,
        dir: &Path,
    ) -> palette_publish::Result<Vec<(PathBuf, ArtifactKind)>> {
        use crate::export::{BuildSide, export_from_lock_to};
        let mut built = vec![
            (
                export_from_lock_to(self.root, &self.lock, BuildSide::Client, dir)?,
                ArtifactKind::Client,
            ),
            (
                export_from_lock_to(self.root, &self.lock, BuildSide::Server, dir)?,
                ArtifactKind::Server,
            ),
        ];
        if config.curseforge().is_some() {
            let curseforge = crate::curseforge::load_config(self.root)?
                .ok_or("pack.toml [publish.curseforge] is not configured")?;
            built.push((
                crate::curseforge::export_from_lock_to(self.root, &self.lock, &curseforge, dir)?,
                ArtifactKind::CurseForge,
            ));
        }
        Ok(built)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{ContentPlacement, FileSpec, PackMeta};
    use std::fs;
    use std::io::Read;

    fn release_root() -> (tempfile::TempDir, PackRoot, Lockfile) {
        let directory = tempfile::tempdir().expect("temporary pack");
        let root = PackRoot {
            path: directory.path().to_path_buf(),
        };
        fs::write(
            root.pack_toml(),
            r#"format = 1

[pack]
name = "Example Pack"
slug = "example-pack"
version = "1.0.0"
group = "org.example.packs"
minecraft = "26.2"
loader = "fabric"
loader_version = "0.19.3"

[mods]
example = "1.0.0"

[publish.github]
repository = "example/example-pack"
"#,
        )
        .expect("manifest");
        fs::write(root.path.join("CHANGELOG.md"), "Original notes\n").expect("changelog");
        let lock = Lockfile::new(
            PackMeta {
                name: "Example Pack".into(),
                slug: "example-pack".into(),
                version: "1.0.0".into(),
                group: "org.example.packs".into(),
                minecraft: "26.2".into(),
                loader: PackLoader::Fabric,
                loader_version: "0.19.3".into(),
            },
            vec![FileSpec {
                id: "example".into(),
                requested_version: "1.0.0".into(),
                path: "mods/example.jar".into(),
                file_size: 0,
                sha1: "a".repeat(40),
                sha512: "b".repeat(128),
                env: ContentPlacement::SharedMod.env(),
                downloads: vec!["https://example.invalid/example.jar".into()],
            }],
        );
        fs::write(root.lock_toml(), lock.to_toml().expect("lock TOML")).expect("lockfile");
        (directory, root, lock)
    }

    #[test]
    fn a_prepared_modpack_keeps_the_lockfile_it_was_built_from() {
        let (_directory, root, lock) = release_root();
        let path = prepare_release(&root).expect("prepare release");
        let manifest: ReleaseManifest =
            serde_json::from_slice(&fs::read(path).expect("release JSON")).expect("parse");
        assert!(manifest.artifacts.iter().any(|artifact| {
            artifact.role == ArtifactKind::Server
                && artifact.media_type == "application/x-modrinth-modpack+zip"
        }));

        let mut replacement = lock;
        replacement.pack.version = "2.0.0".into();
        fs::write(
            root.lock_toml(),
            replacement.to_toml().expect("replacement lock"),
        )
        .expect("replace lock");
        assert!(verify_release(&root).is_err());

        let client = manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.role == ArtifactKind::Client)
            .expect("client artifact");
        let mut archive =
            zip::ZipArchive::new(fs::File::open(root.path.join(&client.path)).expect("mrpack"))
                .expect("mrpack zip");
        let mut index = String::new();
        archive
            .by_name("modrinth.index.json")
            .expect("index")
            .read_to_string(&mut index)
            .expect("index text");
        let index: serde_json::Value = serde_json::from_str(&index).expect("index JSON");
        assert_eq!(index["versionId"], "1.0.0");
    }
}
