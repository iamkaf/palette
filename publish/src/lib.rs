//! Publishes a Minecraft pack release. A tool builds the release files once; this crate
//! records their hashes in `release.json`, verifies them later, and hands the exact same
//! bytes to GitHub Releases, a Maven repository, Modrinth, and CurseForge. Resolution and
//! archive creation stay in the tool, so a dry run shows what will ship and no platform
//! adapter can quietly produce a different pack.

mod curseforge;
mod github;
mod hash;
mod maven;
mod modrinth;

use serde::{Deserialize, Deserializer, Serialize, de};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub fn from_display(value: impl fmt::Display) -> Self {
        Self(value.to_string())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Error {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self(value.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self(value.to_string())
    }
}

impl From<reqwest::Error> for Error {
    fn from(value: reqwest::Error) -> Self {
        Self(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The command-line tool running a release, named in messages and HTTP requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool {
    pub command: &'static str,
    pub user_agent: &'static str,
}

/// The kind of pack a release publishes. Each kind has its own files and platform metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    Modpack(Loader),
    Datapack,
    ResourcePack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loader {
    Fabric,
    Forge,
    NeoForge,
}

impl Loader {
    fn id(self) -> &'static str {
        match self {
            Self::Fabric => "fabric",
            Self::Forge => "forge",
            Self::NeoForge => "neoforge",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Fabric => "Fabric",
            Self::Forge => "Forge",
            Self::NeoForge => "NeoForge",
        }
    }
}

/// What is being released, captured once so every platform sees the same identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    /// The Maven artifact ID and the start of every release file name.
    pub slug: String,
    pub group: String,
    pub version: String,
    /// Every Minecraft release the files support.
    pub game_versions: Vec<String>,
    pub content: Content,
}

impl Project {
    /// The file name a release file of `kind` must use.
    pub fn artifact_name(&self, kind: ArtifactKind) -> String {
        let base = format!("{}-{}", self.slug, self.version);
        match kind {
            ArtifactKind::Client => format!("{base}-client.mrpack"),
            ArtifactKind::Server => format!("{base}-server.mrpack"),
            ArtifactKind::CurseForge => format!("{base}-curseforge.zip"),
            ArtifactKind::Pack => format!("{base}.zip"),
            ArtifactKind::Maven => format!("{base}.pom"),
            ArtifactKind::MavenMetadata => "maven-metadata.xml".into(),
            ArtifactKind::ReleaseNotes => "release-notes.md".into(),
        }
    }

    /// The file players download from Modrinth and Maven.
    fn primary(&self) -> ArtifactKind {
        match self.content {
            Content::Modpack(_) => ArtifactKind::Client,
            Content::Datapack | Content::ResourcePack => ArtifactKind::Pack,
        }
    }

    /// The file CurseForge receives.
    fn curseforge_file(&self) -> ArtifactKind {
        match self.content {
            Content::Modpack(_) => ArtifactKind::CurseForge,
            Content::Datapack | Content::ResourcePack => ArtifactKind::Pack,
        }
    }

    /// The files the tool builds for this project and configuration.
    fn built_artifacts(&self, config: &PublishConfig) -> Vec<ArtifactKind> {
        match self.content {
            Content::Modpack(_) if config.curseforge.is_some() => {
                vec![
                    ArtifactKind::Client,
                    ArtifactKind::Server,
                    ArtifactKind::CurseForge,
                ]
            }
            Content::Modpack(_) => vec![ArtifactKind::Client, ArtifactKind::Server],
            Content::Datapack | Content::ResourcePack => vec![ArtifactKind::Pack],
        }
    }

    fn description(&self) -> &'static str {
        match self.content {
            Content::Modpack(_) => "Minecraft modpack (.mrpack)",
            Content::Datapack => "Minecraft datapack",
            Content::ResourcePack => "Minecraft resource pack",
        }
    }
}

/// A repository whose pack is being released.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub tool: Tool,
    /// The repository root. Git checks and the changelog path are relative to it.
    pub root: PathBuf,
    /// Where release files and `release.json` live, relative to `root`, with `/` separators.
    pub dist: String,
    /// The manifest holding the `[publish]` table.
    pub manifest: PathBuf,
}

impl Workspace {
    fn dist_dir(&self) -> PathBuf {
        self.root.join(&self.dist)
    }
}

/// The tool's side of a release.
pub trait Pack {
    /// Checks the tool's own inputs, such as a lockfile matching its manifest.
    fn check(&self) -> Result<()>;
    fn project(&self) -> Result<Project>;
    /// Builds the files players download into `dir`, named with [`Project::artifact_name`].
    fn build(
        &self,
        project: &Project,
        config: &PublishConfig,
        dir: &Path,
    ) -> Result<Vec<(PathBuf, ArtifactKind)>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishMode {
    DryRun,
    Publish,
}

/// The manifest's `[publish]` table.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PublishConfig {
    #[serde(default)]
    changelog: Option<String>,
    #[serde(default)]
    modrinth: Option<ModrinthConfig>,
    #[serde(default, deserialize_with = "deserialize_curseforge")]
    curseforge: Option<CurseForgeConfig>,
    #[serde(default)]
    github: Option<GitHubConfig>,
    #[serde(default)]
    maven: Option<MavenConfig>,
}

impl PublishConfig {
    pub fn curseforge(&self) -> Option<&CurseForgeConfig> {
        self.curseforge.as_ref()
    }

    /// Every platform target publishes a changelog, so any of them requires release notes.
    fn needs_release_notes(&self) -> bool {
        self.changelog.is_some()
            || self.modrinth.is_some()
            || self.curseforge.is_some()
            || self.github.is_some()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModrinthConfig {
    project: String,
}

/// `[publish.curseforge]`. Keys other than `project` and `author` belong to the tool, such
/// as Swatch's files to add to or exclude from its CurseForge export.
#[derive(Debug, Deserialize)]
pub struct CurseForgeConfig {
    pub project: u64,
    pub author: String,
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitHubConfig {
    #[serde(default)]
    repository: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MavenConfig {
    repository: String,
}

fn deserialize_curseforge<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<CurseForgeConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = toml::Value::deserialize(deserializer)?;
    match value {
        toml::Value::Boolean(false) => Ok(None),
        toml::Value::Table(_) => value.try_into().map(Some).map_err(de::Error::custom),
        _ => Err(de::Error::custom(
            "publish.curseforge must be false or a table with project and author",
        )),
    }
}

/// The role of one prepared file. The serialized name is the `role` in `release.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactKind {
    Client,
    Server,
    #[serde(rename = "curseforge")]
    CurseForge,
    /// A datapack or resource pack zip.
    Pack,
    #[serde(rename = "maven-pom")]
    Maven,
    MavenMetadata,
    ReleaseNotes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Destination {
    CurseForge,
    GitHub,
    Maven,
    Modrinth,
}

#[derive(Debug, PartialEq, Eq)]
struct Artifact {
    name: String,
    kind: ArtifactKind,
    sha256: String,
    sha512: String,
    bytes: Vec<u8>,
}

/// The files of one release, read once so every platform receives the same bytes.
#[derive(Debug)]
pub struct PreparedRelease {
    tool: Tool,
    project: Project,
    config: PublishConfig,
    artifacts: Vec<Artifact>,
    changelog: Option<String>,
}

impl PreparedRelease {
    pub fn project(&self) -> &Project {
        &self.project
    }

    fn artifact(&self, kind: ArtifactKind) -> Result<&Artifact> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.kind == kind)
            .ok_or_else(|| format!("prepared release is missing a {kind:?} artifact").into())
    }

    fn changelog(&self) -> Result<&str> {
        self.changelog
            .as_deref()
            .ok_or_else(|| "prepared release has no changelog".into())
    }

    fn http_client(&self) -> Result<reqwest::blocking::Client> {
        http_client(self.tool)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema_version: u32,
    pub pack_version: String,
    pub preparation_mode: ReleasePreparation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub targets: ReleaseTargets,
    pub artifacts: Vec<ReleaseArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseTargets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modrinth: Option<ReleaseModrinthTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curseforge: Option<ReleaseCurseForgeTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maven: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseCurseForgeTarget {
    pub project: u64,
    pub author: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseModrinthTarget {
    pub project: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseArtifact {
    pub role: ArtifactKind,
    pub path: String,
    pub media_type: String,
    pub sha256: String,
    pub sha512: String,
    pub destinations: Vec<Destination>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReleasePreparation {
    Strict,
    Preview,
}

/// What GitHub Actions says about the checkout, when a release runs there.
#[derive(Debug, Clone, Copy, Default)]
struct Ci<'a> {
    repository: Option<&'a str>,
    revision: Option<&'a str>,
}

fn with_ci<T>(run: impl FnOnce(Ci<'_>) -> T) -> T {
    let repository = std::env::var("GITHUB_REPOSITORY").ok();
    let revision = std::env::var("GITHUB_SHA").ok();
    run(Ci {
        repository: repository.as_deref(),
        revision: revision.as_deref(),
    })
}

/// Prepares the release once and writes `release.json` next to its files.
pub fn prepare_release(workspace: &Workspace, pack: &impl Pack) -> Result<PathBuf> {
    let release = with_ci(|ci| prepare(workspace, pack, ReleasePreparation::Strict, ci))?;
    let manifest = manifest_from_release(workspace, &release, ReleasePreparation::Strict)?;
    let path = workspace.dist_dir().join("release.json");
    write_json(&path, &manifest)?;
    Ok(path)
}

/// Checks that `release.json` still describes the prepared files and the current sources.
pub fn verify_release(workspace: &Workspace, pack: &impl Pack) -> Result<ReleaseManifest> {
    let (manifest, _) = with_ci(|ci| load_prepared(workspace, pack, ci.repository))?;
    Ok(manifest)
}

/// Publishes the prepared files to every configured target. A dry run prepares a preview
/// instead and reports what would be uploaded.
pub fn publish(workspace: &Workspace, pack: &impl Pack, mode: PublishMode) -> Result<Vec<String>> {
    let (manifest, release) = if mode == PublishMode::DryRun {
        let release = with_ci(|ci| prepare(workspace, pack, ReleasePreparation::Preview, ci))?;
        let manifest = manifest_from_release(workspace, &release, ReleasePreparation::Preview)?;
        write_json(
            &workspace.dist_dir().join("release.preview.json"),
            &manifest,
        )?;
        (manifest, release)
    } else {
        with_ci(|ci| load_prepared(workspace, pack, ci.repository))?
    };
    let mut output = dispatch_publish_targets(
        &release.config,
        mode,
        |name| {
            std::env::var(name)
                .ok()
                .is_some_and(|value| !value.is_empty())
        },
        |target| match (mode, target) {
            (PublishMode::DryRun, PublishTarget::GitHub) => github::dry_run(&release),
            (PublishMode::DryRun, PublishTarget::Maven) => maven::dry_run(&release),
            (PublishMode::DryRun, PublishTarget::Modrinth) => modrinth::dry_run(&release),
            (PublishMode::DryRun, PublishTarget::CurseForge) => curseforge::dry_run(&release),
            (PublishMode::Publish, PublishTarget::GitHub) => {
                let input = github::preflight(workspace, manifest.source_revision.as_deref())?;
                github::publish(&release, &input)
            }
            (PublishMode::Publish, PublishTarget::Maven) => maven::publish(&release),
            (PublishMode::Publish, PublishTarget::Modrinth) => modrinth::publish(&release),
            (PublishMode::Publish, PublishTarget::CurseForge) => curseforge::publish(&release),
        },
    )?;
    if output.is_empty() {
        output.push("prepared release locally; no publish targets are configured".into());
    }
    Ok(output)
}

/// Reads the `[publish]` table from the workspace manifest.
pub fn load_config(workspace: &Workspace) -> Result<PublishConfig> {
    let text = fs::read_to_string(&workspace.manifest).map_err(|error| {
        Error::from(format!(
            "cannot read {}: {error}",
            workspace.manifest.display()
        ))
    })?;
    parse_config(&text, &manifest_name(workspace))
}

fn parse_config(text: &str, manifest: &str) -> Result<PublishConfig> {
    let value: toml::Value =
        toml::from_str(text).map_err(|error| Error::from(format!("{manifest}: {error}")))?;
    let Some(table) = value.get("publish") else {
        return Ok(PublishConfig::default());
    };
    table
        .clone()
        .try_into()
        .map_err(|error| Error::from(format!("{manifest} [publish]: {error}")))
}

fn manifest_name(workspace: &Workspace) -> String {
    workspace
        .manifest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| workspace.manifest.display().to_string())
}

/// Build every local release file once.
fn prepare(
    workspace: &Workspace,
    pack: &impl Pack,
    mode: ReleasePreparation,
    ci: Ci<'_>,
) -> Result<PreparedRelease> {
    pack.check()?;
    let project = pack.project()?;
    let mut config = load_config(workspace)?;
    resolve_publish_targets(&mut config, mode, ci.repository)?;
    if mode == ReleasePreparation::Strict {
        require_clean_repository(workspace)?;
        require_matching_github_revision(workspace, ci.revision)?;
    }
    let changelog = if config.needs_release_notes() {
        match load_changelog(workspace, &config) {
            Ok(changelog) => Some(changelog),
            Err(_) if mode == ReleasePreparation::Preview => None,
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    let output_dir = match mode {
        ReleasePreparation::Strict => workspace.dist_dir(),
        ReleasePreparation::Preview => workspace.dist_dir().join("preview"),
    };
    fs::create_dir_all(&output_dir)?;

    let mut artifacts = Vec::new();
    for (path, kind) in pack.build(&project, &config, &output_dir)? {
        let expected = output_dir.join(project.artifact_name(kind));
        if path != expected {
            return Err(format!(
                "{} built {} as {}; it must be {}",
                workspace.tool.command,
                format!("{kind:?}").to_lowercase(),
                path.display(),
                expected.display()
            )
            .into());
        }
        artifacts.push(artifact(&path, kind)?);
    }
    if let Some(maven) = &config.maven {
        let pom = output_dir.join(project.artifact_name(ArtifactKind::Maven));
        write_atomic(&pom, minimal_pom(&project).as_bytes())?;
        artifacts.push(artifact(&pom, ArtifactKind::Maven)?);

        let metadata = output_dir.join(project.artifact_name(ArtifactKind::MavenMetadata));
        write_atomic(
            &metadata,
            maven::prepare_metadata(workspace.tool, &project, &maven.repository, mode)?.as_bytes(),
        )?;
        artifacts.push(artifact(&metadata, ArtifactKind::MavenMetadata)?);
    }
    if let Some(changelog) = &changelog {
        let notes = output_dir.join(project.artifact_name(ArtifactKind::ReleaseNotes));
        write_atomic(&notes, changelog.as_bytes())?;
        artifacts.push(artifact(&notes, ArtifactKind::ReleaseNotes)?);
    }
    artifacts.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(PreparedRelease {
        tool: workspace.tool,
        project,
        config,
        artifacts,
        changelog,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishTarget {
    GitHub,
    Maven,
    Modrinth,
    CurseForge,
}

const PUBLISH_ORDER: [PublishTarget; 4] = [
    PublishTarget::GitHub,
    PublishTarget::Maven,
    PublishTarget::Modrinth,
    PublishTarget::CurseForge,
];

fn dispatch_publish_targets(
    config: &PublishConfig,
    mode: PublishMode,
    credential_is_set: impl Fn(&str) -> bool,
    mut publish_target: impl FnMut(PublishTarget) -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    if mode == PublishMode::Publish {
        validate_publish_credentials(config, credential_is_set)?;
    }

    let mut output = Vec::new();
    for target in configured_targets(config) {
        output.extend(publish_target(target)?);
    }
    Ok(output)
}

fn configured_targets(config: &PublishConfig) -> impl Iterator<Item = PublishTarget> + '_ {
    PUBLISH_ORDER.into_iter().filter(|target| match target {
        PublishTarget::GitHub => config.github.is_some(),
        PublishTarget::Maven => config.maven.is_some(),
        PublishTarget::Modrinth => config.modrinth.is_some(),
        PublishTarget::CurseForge => config.curseforge.is_some(),
    })
}

fn validate_publish_credentials(
    config: &PublishConfig,
    credential_is_set: impl Fn(&str) -> bool,
) -> Result<()> {
    let mut missing = Vec::new();
    if config.github.is_some()
        && !credential_is_set("GITHUB_TOKEN")
        && !credential_is_set("GH_TOKEN")
    {
        missing.push("GitHub: set GITHUB_TOKEN (or GH_TOKEN)");
    }
    if config.maven.is_some() {
        if !credential_is_set("MAVEN_PUBLISH_USERNAME") {
            missing.push("Maven: set MAVEN_PUBLISH_USERNAME");
        }
        if !credential_is_set("MAVEN_PUBLISH_PASSWORD") {
            missing.push("Maven: set MAVEN_PUBLISH_PASSWORD");
        }
    }
    if config.modrinth.is_some() && !credential_is_set("MODRINTH_TOKEN") {
        missing.push("Modrinth: set MODRINTH_TOKEN");
    }
    if config.curseforge.is_some() && !credential_is_set("CURSEFORGE_TOKEN") {
        missing.push("CurseForge: set CURSEFORGE_TOKEN");
    }
    if missing.is_empty() {
        return Ok(());
    }

    Err(format!(
        "cannot publish because configured target credentials are missing:\n  - {}\nno files were uploaded",
        missing.join("\n  - ")
    )
    .into())
}

fn load_changelog(workspace: &Workspace, config: &PublishConfig) -> Result<String> {
    let relative = config.changelog.as_deref().unwrap_or("CHANGELOG.md");
    check_relative_path(relative)?;
    let path = workspace.root.join(relative);
    fs::read_to_string(&path).map_err(|error| {
        format!("cannot read publish changelog {}: {error}", path.display()).into()
    })
}

fn manifest_from_release(
    workspace: &Workspace,
    release: &PreparedRelease,
    preparation: ReleasePreparation,
) -> Result<ReleaseManifest> {
    let artifact_root = match preparation {
        ReleasePreparation::Strict => workspace.dist.clone(),
        ReleasePreparation::Preview => format!("{}/preview", workspace.dist),
    };
    let mut artifacts = Vec::with_capacity(release.artifacts.len());
    for artifact in &release.artifacts {
        artifacts.push(ReleaseArtifact {
            role: artifact.kind,
            path: format!("{artifact_root}/{}", artifact.name),
            media_type: artifact_media_type(artifact.kind).into(),
            sha256: artifact.sha256.clone(),
            sha512: artifact.sha512.clone(),
            destinations: destinations_for(artifact.kind, &release.config),
        });
    }
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(ReleaseManifest {
        schema_version: 1,
        pack_version: release.project.version.clone(),
        preparation_mode: preparation,
        source_revision: git_revision(workspace),
        targets: release_targets(&release.config, preparation),
        artifacts,
    })
}

fn load_prepared(
    workspace: &Workspace,
    pack: &impl Pack,
    github_repository: Option<&str>,
) -> Result<(ReleaseManifest, PreparedRelease)> {
    let tool = workspace.tool.command;
    pack.check()?;
    let project = pack.project()?;
    let mut config = load_config(workspace)?;
    resolve_publish_targets(&mut config, ReleasePreparation::Strict, github_repository)?;
    let path = workspace.dist_dir().join("release.json");
    let bytes = fs::read(&path).map_err(|error| {
        Error::from(format!(
            "cannot read {}: {error}; run `{tool} prepare` first",
            path.display()
        ))
    })?;
    let manifest: ReleaseManifest = serde_json::from_slice(&bytes)?;
    if manifest.schema_version != 1 {
        return Err(format!(
            "unsupported release.json schema version {}",
            manifest.schema_version
        )
        .into());
    }
    if manifest.preparation_mode != ReleasePreparation::Strict {
        return Err(format!(
            "release.json is a preview and cannot be verified or published; run `{tool} prepare`"
        )
        .into());
    }
    if manifest.pack_version != project.version {
        return Err(format!(
            "release.json pack version {} does not match the current version {}",
            manifest.pack_version, project.version
        )
        .into());
    }
    let current_targets = release_targets(&config, ReleasePreparation::Strict);
    if manifest.targets != current_targets {
        return Err(format!(
            "release.json publication targets no longer match {} or the environment; prepare again",
            manifest_name(workspace)
        )
        .into());
    }
    if manifest
        .source_revision
        .as_deref()
        .is_some_and(|revision| !valid_revision(revision))
    {
        return Err("release.json has an invalid source revision".into());
    }
    if let (Some(prepared), Some(current)) = (&manifest.source_revision, git_revision(workspace))
        && prepared != &current
    {
        return Err(format!(
            "release.json was prepared from source revision {prepared}, current revision is {current}"
        )
        .into());
    }
    // The prepared bytes came from a clean checkout of that revision. Uncommitted edits to
    // the pack's sources would not be in them.
    if manifest.source_revision.is_some() {
        require_clean_repository(workspace)?;
    }

    let dist_prefix = format!("{}/", workspace.dist);
    let mut artifacts = Vec::with_capacity(manifest.artifacts.len());
    let mut paths = BTreeSet::new();
    let mut roles = BTreeSet::new();
    let mut changelog = None;
    for record in &manifest.artifacts {
        check_relative_path(&record.path)?;
        if !record.path.starts_with(&dist_prefix) || record.path[dist_prefix.len()..].contains('/')
        {
            return Err(format!(
                "release artifact must be directly under {dist_prefix}: {}",
                record.path
            )
            .into());
        }
        let kind = record.role;
        if !paths.insert(record.path.as_str()) || !roles.insert(kind) {
            return Err(format!("duplicate release artifact {}", record.path).into());
        }
        let expected_name = project.artifact_name(kind);
        if record.path[dist_prefix.len()..] != expected_name {
            return Err(format!("{kind:?} artifact must use {dist_prefix}{expected_name}").into());
        }
        if record.media_type != artifact_media_type(kind) {
            return Err(format!("{} has an unexpected media type", record.path).into());
        }
        if record.destinations != destinations_for(kind, &config) {
            return Err(format!(
                "{} destinations no longer match {}",
                record.path,
                manifest_name(workspace)
            )
            .into());
        }
        let artifact = artifact(&workspace.root.join(&record.path), kind)?;
        if artifact.sha256 != record.sha256 || artifact.sha512 != record.sha512 {
            return Err(format!("{} does not match release.json", record.path).into());
        }
        if kind == ArtifactKind::ReleaseNotes {
            changelog = Some(
                String::from_utf8(artifact.bytes.clone())
                    .map_err(|_| Error::from(format!("{} is not UTF-8", record.path)))?,
            );
        }
        artifacts.push(artifact);
    }
    let mut required = project.built_artifacts(&config);
    if config.maven.is_some() {
        required.extend([ArtifactKind::Maven, ArtifactKind::MavenMetadata]);
    }
    if config.needs_release_notes() {
        required.push(ArtifactKind::ReleaseNotes);
    }
    for required in required {
        if !artifacts.iter().any(|artifact| artifact.kind == required) {
            return Err(format!("release.json is missing the {required:?} artifact").into());
        }
    }
    Ok((
        manifest,
        PreparedRelease {
            tool: workspace.tool,
            project,
            config,
            artifacts,
            changelog,
        },
    ))
}

fn artifact_media_type(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Client | ArtifactKind::Server => "application/x-modrinth-modpack+zip",
        ArtifactKind::CurseForge | ArtifactKind::Pack => "application/zip",
        ArtifactKind::Maven | ArtifactKind::MavenMetadata => "application/xml",
        ArtifactKind::ReleaseNotes => "text/markdown; charset=utf-8",
    }
}

fn resolve_publish_targets(
    config: &mut PublishConfig,
    preparation: ReleasePreparation,
    github_repository: Option<&str>,
) -> Result<()> {
    if let Some(github) = &mut config.github {
        match github.repository.as_deref().or(github_repository) {
            Some(repository) => {
                validate_github_repository(repository)?;
                github.repository = Some(repository.to_string());
            }
            None if preparation == ReleasePreparation::Preview => {}
            None => {
                return Err(
                    "publish.github.repository is required outside GitHub Actions; GITHUB_REPOSITORY was not set"
                        .into(),
                );
            }
        }
    }
    if let Some(modrinth) = &config.modrinth
        && modrinth.project.trim().is_empty()
    {
        return Err("publish.modrinth.project is required".into());
    }
    if let Some(curseforge) = &config.curseforge {
        if curseforge.project == 0 {
            return Err("publish.curseforge.project must be a positive project ID".into());
        }
        if curseforge.author.trim().is_empty() {
            return Err("publish.curseforge.author is required".into());
        }
    }
    if let Some(maven) = &mut config.maven {
        if !maven.repository.starts_with("https://") {
            return Err("publish.maven.repository must use HTTPS".into());
        }
        let repository = maven.repository.trim_end_matches('/');
        if repository.len() == "https:".len() {
            return Err("publish.maven.repository must name an HTTPS repository".into());
        }
        maven.repository = repository.to_string();
    }
    Ok(())
}

fn release_targets(config: &PublishConfig, preparation: ReleasePreparation) -> ReleaseTargets {
    ReleaseTargets {
        github: config.github.as_ref().map(|github| {
            github.repository.clone().unwrap_or_else(|| {
                debug_assert_eq!(preparation, ReleasePreparation::Preview);
                "<GITHUB_REPOSITORY>".into()
            })
        }),
        modrinth: config
            .modrinth
            .as_ref()
            .map(|modrinth| ReleaseModrinthTarget {
                project: modrinth.project.clone(),
            }),
        curseforge: config
            .curseforge
            .as_ref()
            .map(|curseforge| ReleaseCurseForgeTarget {
                project: curseforge.project,
                author: curseforge.author.clone(),
            }),
        maven: config.maven.as_ref().map(|maven| maven.repository.clone()),
    }
}

fn validate_github_repository(repository: &str) -> Result<()> {
    let mut parts = repository.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return Err(format!("publish.github.repository must be owner/name: {repository}").into());
    }
    Ok(())
}

fn destinations_for(kind: ArtifactKind, config: &PublishConfig) -> Vec<Destination> {
    let github = (Destination::GitHub, config.github.is_some());
    let maven = (Destination::Maven, config.maven.is_some());
    let modrinth = (Destination::Modrinth, config.modrinth.is_some());
    let curseforge = (Destination::CurseForge, config.curseforge.is_some());
    let candidates: &[(Destination, bool)] = match kind {
        ArtifactKind::Client => &[github, maven, modrinth],
        ArtifactKind::Server => &[github],
        ArtifactKind::CurseForge => &[curseforge, github],
        ArtifactKind::Pack => &[github, maven, modrinth, curseforge],
        ArtifactKind::Maven | ArtifactKind::MavenMetadata => &[maven],
        ArtifactKind::ReleaseNotes => &[],
    };
    let mut destinations: Vec<_> = candidates
        .iter()
        .filter(|(_, configured)| *configured)
        .map(|(destination, _)| *destination)
        .collect();
    destinations.sort();
    destinations
}

fn git_revision(workspace: &Workspace) -> Option<String> {
    let output = Command::new("git")
        .current_dir(&workspace.root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let revision = String::from_utf8(output.stdout).ok()?.trim().to_string();
    valid_revision(&revision).then_some(revision)
}

fn require_matching_github_revision(
    workspace: &Workspace,
    github_revision: Option<&str>,
) -> Result<()> {
    let (Some(github_revision), Some(head)) = (github_revision, git_revision(workspace)) else {
        return Ok(());
    };
    if !valid_revision(github_revision) || !github_revision.eq_ignore_ascii_case(&head) {
        return Err(format!(
            "GITHUB_SHA {github_revision} does not match the checked-out HEAD {head}"
        )
        .into());
    }
    Ok(())
}

fn require_clean_repository(workspace: &Workspace) -> Result<()> {
    let repository = Command::new("git")
        .current_dir(&workspace.root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output();
    let Ok(repository) = repository else {
        return Ok(());
    };
    if !repository.status.success() || String::from_utf8_lossy(&repository.stdout).trim() != "true"
    {
        return Ok(());
    }

    let status = Command::new("git")
        .current_dir(&workspace.root)
        .args(["status", "--porcelain=v1", "--untracked-files=normal"])
        .output()
        .map_err(|error| Error::from(format!("cannot inspect repository status: {error}")))?;
    if !status.status.success() {
        return Err("cannot inspect repository status for a strict release".into());
    }
    if !status.stdout.is_empty() {
        return Err(
            "strict releases require a clean repository, including no untracked non-ignored files"
                .into(),
        );
    }
    Ok(())
}

fn valid_revision(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64) && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn artifact(path: &Path, kind: ArtifactKind) -> Result<Artifact> {
    let bytes = fs::read(path)
        .map_err(|error| Error::from(format!("cannot read {}: {error}", path.display())))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::from(format!("invalid artifact filename: {}", path.display())))?;
    Ok(Artifact {
        name: name.into(),
        kind,
        sha256: hash::sha256_hex(&bytes),
        sha512: hash::sha512_hex(&bytes),
        bytes,
    })
}

fn minimal_pom(project: &Project) -> String {
    let packaging = match project.content {
        Content::Modpack(_) => "pom",
        Content::Datapack | Content::ResourcePack => "zip",
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0"
         xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
         xsi:schemaLocation="http://maven.apache.org/POM/4.0.0 https://maven.apache.org/xsd/maven-4.0.0.xsd">
  <modelVersion>4.0.0</modelVersion>
  <groupId>{}</groupId>
  <artifactId>{}</artifactId>
  <version>{}</version>
  <packaging>{packaging}</packaging>
  <name>{}</name>
  <description>{}</description>
</project>
"#,
        xml(&project.group),
        xml(&project.slug),
        xml(&project.version),
        xml(&project.name),
        project.description(),
    )
}

pub(crate) fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn http_client(tool: Tool) -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(tool.user_agent)
        .timeout(Duration::from_secs(300))
        .build()?)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    io::Write::write_all(&mut temporary, bytes)?;
    temporary
        .persist(path)
        .map_err(|error| Error::from_display(error.error))?;
    Ok(())
}

/// Rejects paths that could leave the repository or that some platforms cannot store.
fn check_relative_path(path: &str) -> Result<()> {
    let portable = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.ends_with(['.', ' '])
            && !part.chars().any(|character| {
                character <= '\u{1f}'
                    || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
            })
    };
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || !path.split('/').all(portable)
    {
        return Err(format!("invalid relative path `{path}`").into());
    }
    Ok(())
}

/// Shared fixtures for this crate's tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub(crate) const TOOL: Tool = Tool {
        command: "packtool",
        user_agent: "packtool/0.0.0 (test)",
    };

    pub(crate) fn workspace(root: &Path) -> Workspace {
        Workspace {
            tool: TOOL,
            root: root.to_path_buf(),
            dist: "build/dist".into(),
            manifest: root.join("pack.toml"),
        }
    }

    pub(crate) fn modpack() -> Project {
        Project {
            name: "Example Pack".into(),
            slug: "example-pack".into(),
            group: "org.example.packs".into(),
            version: "1.0.0".into(),
            game_versions: vec!["26.2".into()],
            content: Content::Modpack(Loader::Fabric),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{modpack, workspace};
    use super::*;

    /// A pack whose files are fixed bytes, so tests exercise only the release flow.
    struct FakePack {
        project: Project,
    }

    impl Pack for FakePack {
        fn check(&self) -> Result<()> {
            Ok(())
        }

        fn project(&self) -> Result<Project> {
            Ok(self.project.clone())
        }

        fn build(
            &self,
            project: &Project,
            config: &PublishConfig,
            dir: &Path,
        ) -> Result<Vec<(PathBuf, ArtifactKind)>> {
            let mut built = Vec::new();
            for kind in project.built_artifacts(config) {
                let path = dir.join(project.artifact_name(kind));
                fs::write(&path, format!("{kind:?} {}", project.version))?;
                built.push((path, kind));
            }
            Ok(built)
        }
    }

    const GITHUB: &str = "[publish.github]\nrepository = \"example/example-pack\"\n";

    fn repository(publish: &str) -> (tempfile::TempDir, Workspace) {
        let directory = tempfile::tempdir().expect("temporary pack");
        let workspace = workspace(directory.path());
        fs::write(
            &workspace.manifest,
            format!("name = \"Example\"\n\n{publish}"),
        )
        .expect("manifest");
        fs::write(directory.path().join("CHANGELOG.md"), "Original notes\n").expect("changelog");
        (directory, workspace)
    }

    fn modpack_pack() -> FakePack {
        FakePack { project: modpack() }
    }

    fn datapack_pack() -> FakePack {
        FakePack {
            project: Project {
                content: Content::Datapack,
                game_versions: vec!["1.21.1".into(), "26.3".into()],
                ..modpack()
            },
        }
    }

    fn no_ci() -> Ci<'static> {
        Ci::default()
    }

    fn commit(workspace: &Workspace) {
        fs::write(workspace.root.join(".gitignore"), "build/\n").expect("gitignore");
        for arguments in [
            &["init"][..],
            &["config", "user.name", "Test Author"],
            &["config", "user.email", "test@example.invalid"],
            &["add", "."],
            &["commit", "-m", "Create test pack"],
        ] {
            let status = Command::new("git")
                .current_dir(&workspace.root)
                .args(arguments)
                .status()
                .expect("run git");
            assert!(status.success(), "git {arguments:?}");
        }
    }

    #[test]
    fn pom_describes_the_content_without_dependencies() {
        let modpack_pom = minimal_pom(&modpack());
        assert!(modpack_pom.contains("Minecraft modpack"));
        assert!(modpack_pom.contains("<packaging>pom</packaging>"));
        assert!(!modpack_pom.contains("<dependencies>"));

        let datapack_pom = minimal_pom(&datapack_pack().project);
        assert!(datapack_pom.contains("<packaging>zip</packaging>"));
        assert!(datapack_pom.contains("Minecraft datapack"));
    }

    #[test]
    fn release_json_keeps_its_role_and_destination_names() {
        let roles = [
            ArtifactKind::Client,
            ArtifactKind::Server,
            ArtifactKind::CurseForge,
            ArtifactKind::Pack,
            ArtifactKind::Maven,
            ArtifactKind::MavenMetadata,
            ArtifactKind::ReleaseNotes,
        ]
        .map(|role| serde_json::to_value(role).expect("role JSON"));
        assert_eq!(
            roles,
            [
                "client",
                "server",
                "curseforge",
                "pack",
                "maven-pom",
                "maven-metadata",
                "release-notes"
            ]
        );
        let destinations = [
            Destination::CurseForge,
            Destination::GitHub,
            Destination::Maven,
            Destination::Modrinth,
        ]
        .map(|destination| serde_json::to_value(destination).expect("destination JSON"));
        assert_eq!(destinations, ["curseforge", "github", "maven", "modrinth"]);
    }

    #[test]
    fn curseforge_can_be_disabled_and_keeps_tool_keys() {
        let disabled: PublishConfig =
            toml::from_str("curseforge = false\n").expect("disabled CurseForge target");
        assert!(disabled.curseforge.is_none());

        let enabled: PublishConfig = toml::from_str(
            "[curseforge]\nproject = 123\nauthor = \"Example Author\"\n[[curseforge.add]]\nid = \"extra\"\n",
        )
        .expect("configured CurseForge target");
        let curseforge = enabled.curseforge().expect("CurseForge");
        assert_eq!(curseforge.project, 123);
        assert!(curseforge.extra.contains_key("add"));
    }

    #[test]
    fn release_targets_bind_each_configured_destination() {
        let mut config = parse_config(
            r#"[publish.modrinth]
project = "modrinth-project"

[publish.curseforge]
project = 123
author = "Example Author"

[publish.github]
repository = "example/example-pack"

[publish.maven]
repository = "https://maven.example.invalid/releases///"
"#,
            "pack.toml",
        )
        .expect("publish config");
        resolve_publish_targets(&mut config, ReleasePreparation::Strict, None)
            .expect("resolved targets");

        assert_eq!(
            release_targets(&config, ReleasePreparation::Strict),
            ReleaseTargets {
                github: Some("example/example-pack".into()),
                modrinth: Some(ReleaseModrinthTarget {
                    project: "modrinth-project".into(),
                }),
                curseforge: Some(ReleaseCurseForgeTarget {
                    project: 123,
                    author: "Example Author".into(),
                }),
                maven: Some("https://maven.example.invalid/releases".into()),
            }
        );
    }

    const EVERY_TARGET: &str = r#"[publish.github]
repository = "example/example-pack"

[publish.maven]
repository = "https://maven.example.invalid/releases"

[publish.modrinth]
project = "modrinth-project"

[publish.curseforge]
project = 123
author = "Example Author"
"#;

    #[test]
    fn live_publish_checks_every_credential_before_running_an_adapter() {
        let config = parse_config(EVERY_TARGET, "pack.toml").expect("publish config");
        let mut adapter_calls = Vec::new();
        let error = dispatch_publish_targets(
            &config,
            PublishMode::Publish,
            |name| name == "MAVEN_PUBLISH_USERNAME",
            |target| {
                adapter_calls.push(target);
                Ok(Vec::new())
            },
        )
        .expect_err("missing credentials")
        .to_string();

        assert!(adapter_calls.is_empty());
        assert!(error.contains("GitHub: set GITHUB_TOKEN (or GH_TOKEN)"));
        assert!(error.contains("Maven: set MAVEN_PUBLISH_PASSWORD"));
        assert!(error.contains("Modrinth: set MODRINTH_TOKEN"));
        assert!(error.contains("CurseForge: set CURSEFORGE_TOKEN"));
        assert!(error.contains("no files were uploaded"));
        assert!(!error.contains("MAVEN_PUBLISH_USERNAME"));
    }

    #[test]
    fn dry_run_skips_credentials_and_uses_safe_publish_order() {
        let config = parse_config(EVERY_TARGET, "pack.toml").expect("publish config");
        let mut adapter_calls = Vec::new();
        dispatch_publish_targets(
            &config,
            PublishMode::DryRun,
            |_| panic!("dry-run must not inspect credentials"),
            |target| {
                adapter_calls.push(target);
                Ok(Vec::new())
            },
        )
        .expect("dry-run dispatch");

        assert_eq!(
            adapter_calls,
            [
                PublishTarget::GitHub,
                PublishTarget::Maven,
                PublishTarget::Modrinth,
                PublishTarget::CurseForge,
            ]
        );
    }

    #[test]
    fn a_datapack_sends_one_zip_everywhere() {
        let (_directory, workspace) = repository(EVERY_TARGET);
        let pack = datapack_pack();
        let release = prepare(&workspace, &pack, ReleasePreparation::Preview, no_ci())
            .expect("preview release");
        let manifest = manifest_from_release(&workspace, &release, ReleasePreparation::Preview)
            .expect("manifest");

        let zip = manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.role == ArtifactKind::Pack)
            .expect("pack zip");
        assert_eq!(zip.path, "build/dist/preview/example-pack-1.0.0.zip");
        assert_eq!(zip.media_type, "application/zip");
        assert_eq!(
            zip.destinations,
            [
                Destination::CurseForge,
                Destination::GitHub,
                Destination::Maven,
                Destination::Modrinth
            ]
        );
        assert!(
            manifest
                .artifacts
                .iter()
                .all(|artifact| artifact.role != ArtifactKind::Client)
        );
    }

    #[test]
    fn a_misnamed_build_file_is_rejected() {
        struct Misnamed;
        impl Pack for Misnamed {
            fn check(&self) -> Result<()> {
                Ok(())
            }
            fn project(&self) -> Result<Project> {
                Ok(datapack_pack().project)
            }
            fn build(
                &self,
                _: &Project,
                _: &PublishConfig,
                dir: &Path,
            ) -> Result<Vec<(PathBuf, ArtifactKind)>> {
                let path = dir.join("pack.zip");
                fs::write(&path, "zip")?;
                Ok(vec![(path, ArtifactKind::Pack)])
            }
        }
        let (_directory, workspace) = repository("");
        let error = prepare(&workspace, &Misnamed, ReleasePreparation::Preview, no_ci())
            .expect_err("misnamed file")
            .to_string();
        assert!(error.contains("example-pack-1.0.0.zip"));
    }

    #[test]
    fn empty_github_target_uses_actions_repository_or_preview_placeholder() {
        let (_directory, workspace) = repository("[publish.github]\n");
        let pack = modpack_pack();

        let error = prepare(&workspace, &pack, ReleasePreparation::Strict, no_ci())
            .expect_err("unbound strict GitHub target")
            .to_string();
        assert!(error.contains("GITHUB_REPOSITORY was not set"));

        let actions = Ci {
            repository: Some("example/generated-pack"),
            revision: None,
        };
        let strict = prepare(&workspace, &pack, ReleasePreparation::Strict, actions)
            .expect("Actions release");
        let strict_manifest =
            manifest_from_release(&workspace, &strict, ReleasePreparation::Strict)
                .expect("strict manifest");
        assert_eq!(
            strict_manifest.targets.github.as_deref(),
            Some("example/generated-pack")
        );

        let preview = prepare(&workspace, &pack, ReleasePreparation::Preview, no_ci())
            .expect("local preview");
        let preview_manifest =
            manifest_from_release(&workspace, &preview, ReleasePreparation::Preview)
                .expect("preview manifest");
        assert_eq!(
            preview_manifest.targets.github.as_deref(),
            Some("<GITHUB_REPOSITORY>")
        );
    }

    #[test]
    fn preparation_keeps_one_changelog_snapshot() {
        let (_directory, workspace) = repository(GITHUB);
        let release = prepare(
            &workspace,
            &modpack_pack(),
            ReleasePreparation::Strict,
            no_ci(),
        )
        .expect("prepared release");
        fs::remove_file(workspace.root.join("CHANGELOG.md")).expect("remove changelog");
        assert_eq!(
            release.changelog().expect("captured changelog"),
            "Original notes\n"
        );
    }

    #[test]
    fn dry_run_does_not_require_release_notes() {
        let (_directory, workspace) = repository(GITHUB);
        fs::remove_file(workspace.root.join("CHANGELOG.md")).expect("remove changelog");
        let release = prepare(
            &workspace,
            &modpack_pack(),
            ReleasePreparation::Preview,
            no_ci(),
        )
        .expect("dry-run release");
        assert!(release.changelog.is_none());
    }

    #[test]
    fn verification_rejects_uncommitted_source_changes() {
        let (_directory, workspace) = repository(GITHUB);
        let pack = modpack_pack();
        commit(&workspace);
        prepare_release(&workspace, &pack).expect("strict preparation");
        load_prepared(&workspace, &pack, None).expect("clean verification");

        fs::write(workspace.root.join("CHANGELOG.md"), "Changed notes\n").expect("tracked change");
        let error = load_prepared(&workspace, &pack, None)
            .expect_err("uncommitted change")
            .to_string();
        assert!(error.contains("require a clean repository"));
    }

    #[test]
    fn strict_preparation_rejects_a_dirty_repository() {
        let (_directory, workspace) = repository(GITHUB);
        let pack = modpack_pack();
        commit(&workspace);
        let head = git_revision(&workspace).expect("Git HEAD");
        let mismatch = "a".repeat(40);
        let mismatched = Ci {
            repository: None,
            revision: Some(&mismatch),
        };
        let error = prepare(&workspace, &pack, ReleasePreparation::Strict, mismatched)
            .expect_err("mismatched Actions revision")
            .to_string();
        assert!(error.contains("does not match the checked-out HEAD"));

        let matching = Ci {
            repository: None,
            revision: Some(&head),
        };
        let clean = prepare(&workspace, &pack, ReleasePreparation::Strict, matching)
            .expect("clean strict preparation");
        let clean_manifest = manifest_from_release(&workspace, &clean, ReleasePreparation::Strict)
            .expect("clean manifest");
        assert!(clean_manifest.source_revision.is_some());

        fs::write(workspace.root.join("CHANGELOG.md"), "Changed notes\n").expect("tracked change");
        prepare(&workspace, &pack, ReleasePreparation::Preview, no_ci())
            .expect("tracked dirty preview");
        let error = prepare(&workspace, &pack, ReleasePreparation::Strict, no_ci())
            .expect_err("tracked dirty strict preparation")
            .to_string();
        assert!(error.contains("require a clean repository"));

        fs::write(workspace.root.join("CHANGELOG.md"), "Original notes\n").expect("restore");
        fs::write(workspace.root.join("untracked.txt"), "dirty\n").expect("untracked file");
        prepare(&workspace, &pack, ReleasePreparation::Preview, no_ci())
            .expect("untracked dirty preview");
        let error = prepare(&workspace, &pack, ReleasePreparation::Strict, no_ci())
            .expect_err("untracked dirty strict preparation")
            .to_string();
        assert!(error.contains("require a clean repository"));
    }

    #[test]
    fn non_git_preparation_does_not_claim_the_actions_revision() {
        let (_directory, workspace) = repository(GITHUB);
        let revision = "b".repeat(40);
        let actions = Ci {
            repository: None,
            revision: Some(&revision),
        };
        let release = prepare(
            &workspace,
            &modpack_pack(),
            ReleasePreparation::Strict,
            actions,
        )
        .expect("non-Git preparation");
        let manifest = manifest_from_release(&workspace, &release, ReleasePreparation::Strict)
            .expect("release manifest");
        assert_eq!(manifest.source_revision, None);
    }

    #[test]
    fn maven_preview_preserves_prior_strict_metadata() {
        let (_directory, workspace) =
            repository("[publish.maven]\nrepository = \"https://example.invalid/maven\"\n");
        let pack = modpack_pack();
        fs::create_dir_all(workspace.dist_dir()).expect("create dist directory");
        let metadata_path = workspace.dist_dir().join("maven-metadata.xml");
        fs::write(
            &metadata_path,
            maven::metadata_xml(
                "org.example.packs",
                "example-pack",
                "0.9.0",
                &["0.9.0".into()],
            ),
        )
        .expect("write prior Maven metadata");

        publish(&workspace, &pack, PublishMode::DryRun).expect("Maven preview");
        let preview: ReleaseManifest = serde_json::from_slice(
            &fs::read(workspace.dist_dir().join("release.preview.json")).expect("preview"),
        )
        .expect("parse preview manifest");
        assert_eq!(preview.preparation_mode, ReleasePreparation::Preview);
        assert!(
            fs::read_to_string(metadata_path)
                .expect("strict metadata")
                .contains("0.9.0")
        );
        assert!(
            preview
                .artifacts
                .iter()
                .all(|artifact| artifact.path.starts_with("build/dist/preview/"))
        );

        let verify_error = verify_release(&workspace, &pack)
            .expect_err("preview verification")
            .to_string();
        assert!(verify_error.contains("run `packtool prepare` first"));
        let publish_error = publish(&workspace, &pack, PublishMode::Publish)
            .expect_err("preview publication")
            .to_string();
        assert!(publish_error.contains("run `packtool prepare` first"));
    }

    #[test]
    fn dry_run_preserves_a_strict_release_snapshot() {
        let (_directory, workspace) = repository(GITHUB);
        let pack = modpack_pack();
        let strict_path = prepare_release(&workspace, &pack).expect("prepare strict release");
        let strict_bytes = fs::read(&strict_path).expect("strict release JSON");

        publish(&workspace, &pack, PublishMode::DryRun).expect("publication preview");

        assert_eq!(
            fs::read(&strict_path).expect("strict release JSON after preview"),
            strict_bytes
        );
        verify_release(&workspace, &pack).expect("strict snapshot still verifies");
    }

    #[test]
    fn release_manifest_verifies_exact_prepared_bytes() {
        let (_directory, workspace) = repository(GITHUB);
        let pack = modpack_pack();
        let path = prepare_release(&workspace, &pack).expect("prepare release");
        let manifest: ReleaseManifest =
            serde_json::from_slice(&fs::read(path).expect("release JSON"))
                .expect("parse release JSON");
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.pack_version, "1.0.0");
        assert!(manifest.artifacts.iter().any(|artifact| {
            artifact.role == ArtifactKind::Client
                && artifact.destinations == [Destination::GitHub]
                && artifact.media_type == "application/x-modrinth-modpack+zip"
                && artifact.sha256.len() == 64
                && artifact.sha512.len() == 128
        }));
        verify_release(&workspace, &pack).expect("verify release");

        let client = manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.role == ArtifactKind::Client)
            .expect("client artifact");
        fs::write(workspace.root.join(&client.path), b"changed").expect("change artifact");
        let error = verify_release(&workspace, &pack)
            .expect_err("changed artifact")
            .to_string();
        assert!(error.contains("does not match release.json"));
    }

    #[test]
    fn verification_rejects_a_changed_same_provider_destination() {
        let (_directory, workspace) = repository(GITHUB);
        let pack = modpack_pack();
        prepare_release(&workspace, &pack).expect("prepare release");
        let manifest = fs::read_to_string(&workspace.manifest)
            .expect("read manifest")
            .replace("example/example-pack", "example/other-pack");
        fs::write(&workspace.manifest, manifest).expect("change GitHub repository");

        let error = verify_release(&workspace, &pack)
            .expect_err("changed GitHub target")
            .to_string();
        assert!(error.contains("publication targets no longer match"));
    }
}
