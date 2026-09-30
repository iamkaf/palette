use super::{Content, PreparedRelease, Result};
use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://minecraft.curseforge.com/api/projects";

fn upload_url(project: u64) -> String {
    format!("{API_BASE}/{project}/upload-file")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadMetadata {
    changelog: String,
    changelog_type: String,
    display_name: String,
    game_version_names: Vec<String>,
    release_type: String,
}

#[derive(Debug, Deserialize)]
struct UploadResponse {
    id: u64,
}

pub fn dry_run(release: &PreparedRelease) -> Result<Vec<String>> {
    let config = release
        .config
        .curseforge
        .as_ref()
        .ok_or_else(|| crate::Error::from("CurseForge is not configured"))?;
    let artifact = release.artifact(release.project.curseforge_file())?;
    Ok(vec![format!(
        "DRY CurseForge {} <- {} ({})",
        upload_url(config.project),
        artifact.name,
        artifact.sha512
    )])
}

pub fn publish(release: &PreparedRelease) -> Result<Vec<String>> {
    let config = release
        .config
        .curseforge
        .as_ref()
        .ok_or_else(|| crate::Error::from("CurseForge is not configured"))?;
    let token = std::env::var("CURSEFORGE_TOKEN")
        .map_err(|_| crate::Error::from("set CURSEFORGE_TOKEN"))?;
    let artifact = release.artifact(release.project.curseforge_file())?;
    let metadata = serde_json::to_string(&UploadMetadata {
        changelog: release.changelog()?.to_string(),
        changelog_type: "markdown".into(),
        display_name: format!("{} {}", release.project.name, release.project.version),
        game_version_names: game_version_names(
            &release.project.content,
            &release.project.game_versions,
        ),
        release_type: "release".into(),
    })?;
    let url = upload_url(config.project);
    let form = reqwest::blocking::multipart::Form::new()
        .text("metadata", metadata)
        .part(
            "file",
            reqwest::blocking::multipart::Part::bytes(artifact.bytes.clone())
                .file_name(artifact.name.clone()),
        );
    let response = release
        .http_client()?
        .post(url)
        .header("X-Api-Token", token)
        .multipart(form)
        .send()?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "CurseForge upload failed: {status}: {}",
            response.text().unwrap_or_default().trim()
        )
        .into());
    }
    let uploaded: UploadResponse = response.json()?;
    Ok(vec![format!(
        "uploaded CurseForge {} as file {}",
        artifact.name, uploaded.id
    )])
}

/// CurseForge takes a modpack's loader alongside its Minecraft versions.
fn game_version_names(content: &Content, game_versions: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    if let Content::Modpack(loader) = content {
        names.push(loader.display_name().to_string());
    }
    names.extend(game_versions.iter().cloned());
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_name_every_minecraft_version_and_modpacks_their_loader() {
        let versions = vec!["1.21.1".to_string(), "26.3".to_string()];
        assert_eq!(game_version_names(&Content::Datapack, &versions), versions);
        assert_eq!(
            game_version_names(
                &Content::Modpack(super::super::Loader::Fabric),
                &versions[1..]
            ),
            vec!["Fabric".to_string(), "26.3".to_string()]
        );
    }

    #[test]
    fn sends_game_version_names_to_curseforge() {
        let metadata = UploadMetadata {
            changelog: String::new(),
            changelog_type: "markdown".into(),
            display_name: "Pack 1.0.0".into(),
            game_version_names: vec!["Fabric".into(), "26.2".into()],
            release_type: "release".into(),
        };
        let json = serde_json::to_value(metadata).expect("metadata JSON");
        assert_eq!(json["gameVersionNames"][1], "26.2");
        assert!(json.get("gameVersions").is_none());
    }
}
