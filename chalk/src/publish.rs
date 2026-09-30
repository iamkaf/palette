//! Chalk's side of a release: the checked pack and the zip players download. The shared
//! publisher prepares, verifies, and uploads it.

use crate::check::{self, Support};
use crate::{PackRoot, Result, TOOL_NAME, USER_AGENT, build, source};
use palette_publish::{ArtifactKind, Content, Pack, Project, PublishConfig, Tool, Workspace};
use std::path::{Path, PathBuf};

pub use palette_publish::{PublishMode, ReleaseManifest};

const TOOL: Tool = Tool {
    command: TOOL_NAME,
    user_agent: USER_AGENT,
};

/// Prepares the release once and writes `build/chalk/dist/release.json`.
pub fn prepare_release(root: &PackRoot) -> Result<PathBuf> {
    Ok(palette_publish::prepare_release(
        &workspace(root),
        &Datapack::load(root)?,
    )?)
}

/// Checks that `build/chalk/dist/release.json` still describes the prepared files.
pub fn verify_release(root: &PackRoot) -> Result<ReleaseManifest> {
    Ok(palette_publish::verify_release(
        &workspace(root),
        &Datapack::load(root)?,
    )?)
}

/// Publishes the prepared files, or previews them with a dry run.
pub fn publish(root: &PackRoot, mode: PublishMode) -> Result<Vec<String>> {
    Ok(palette_publish::publish(
        &workspace(root),
        &Datapack::load(root)?,
        mode,
    )?)
}

fn workspace(root: &PackRoot) -> Workspace {
    Workspace {
        tool: TOOL,
        root: root.dir().to_path_buf(),
        dist: "build/chalk/dist".into(),
        manifest: root.manifest(),
    }
}

/// A pack read and checked once, so every release step sees the same sources.
struct Datapack<'a> {
    root: &'a PackRoot,
    support: Support,
    manifest: source::Manifest,
}

impl<'a> Datapack<'a> {
    fn load(root: &'a PackRoot) -> Result<Self> {
        Ok(Self {
            root,
            support: check::support(root)?,
            manifest: source::manifest(root)?,
        })
    }
}

impl Pack for Datapack<'_> {
    fn check(&self) -> palette_publish::Result<()> {
        Ok(())
    }

    fn project(&self) -> palette_publish::Result<Project> {
        let required = |value: &Option<String>, field: &str| {
            value
                .clone()
                .ok_or_else(|| format!("chalk.toml needs a {field} to publish"))
        };
        Ok(Project {
            name: required(&self.manifest.name, "name")?,
            slug: self.root.slug().to_owned(),
            group: self.manifest.group.clone().unwrap_or_default(),
            version: required(&self.manifest.version, "version")?,
            game_versions: game_versions(&self.support),
            content: Content::Datapack,
        })
    }

    fn build(
        &self,
        project: &Project,
        _config: &PublishConfig,
        dir: &Path,
    ) -> palette_publish::Result<Vec<(PathBuf, ArtifactKind)>> {
        let out = dir.join(project.artifact_name(ArtifactKind::Pack));
        build::build_to(&self.support.pack, &out)?;
        Ok(vec![(out, ArtifactKind::Pack)])
    }
}

/// Every release the built pack loads on, which is every release whose data format is in
/// its range, not only the versions Chalk tests.
fn game_versions(support: &Support) -> Vec<String> {
    support
        .versions
        .releases
        .iter()
        .filter(|release| support.pack.formats.contains(release.data_format))
        .map(|release| release.version.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn pack(manifest: &str) -> (tempfile::TempDir, PackRoot) {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("my-pack");
        fs::create_dir_all(repo.join("datapack/data/demo/function")).expect("pack dirs");
        fs::write(repo.join("chalk.toml"), manifest).expect("manifest");
        fs::write(
            repo.join("datapack/data/demo/function/hello.mcfunction"),
            "say hello\n",
        )
        .expect("function");
        let root = PackRoot::at(&repo).expect("root");
        (dir, root)
    }

    #[test]
    fn a_dry_run_releases_one_zip_for_every_release_in_range() {
        let (_dir, root) = pack(
            r#"description = "Demo"
minecraft = "1.21.11-26.2"
name = "My Pack"
version = "1.0.0"
group = "com.example.packs"

[publish.maven]
repository = "https://maven.example.invalid/releases"
"#,
        );
        publish(&root, PublishMode::DryRun).expect("dry run");

        let preview: ReleaseManifest = serde_json::from_slice(
            &fs::read(root.dir().join("build/chalk/dist/release.preview.json")).expect("preview"),
        )
        .expect("preview JSON");
        let pack = preview
            .artifacts
            .iter()
            .find(|artifact| artifact.role == ArtifactKind::Pack)
            .expect("pack artifact");
        assert_eq!(pack.path, "build/chalk/dist/preview/my-pack-1.0.0.zip");
        assert_eq!(
            game_versions(&check::support(&root).expect("support")),
            ["1.21.11", "26.1", "26.1.1", "26.1.2", "26.2"]
        );
    }

    #[test]
    fn maven_needs_a_group() {
        let (_dir, root) = pack(
            r#"description = "Demo"
minecraft = "26.2"
name = "My Pack"
version = "1.0.0"

[publish.maven]
repository = "https://maven.example.invalid/releases"
"#,
        );
        let error = publish(&root, PublishMode::DryRun).expect_err("no group");
        assert!(error.to_string().contains("needs a group"), "{error}");
    }
}
