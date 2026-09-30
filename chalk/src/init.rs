//! Creates a new pack repository that builds, loads, and passes its test out of the box.

use crate::Result;
use crate::versions::versions;
use std::fs;
use std::path::Path;

/// Writes a starter pack into `dir`, which must be missing or empty.
pub fn init(dir: &Path) -> Result<()> {
    if dir.exists() && fs::read_dir(dir)?.next().is_some() {
        return Err(format!("{} isn't empty", dir.display()).into());
    }
    let slug = dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "{} has no directory name to use as the pack slug",
                dir.display()
            )
        })?;
    let namespace = namespace(slug)?;
    let versions = versions()?;
    let (Some(newest), Some(oldest)) = (versions.minecraft.first(), versions.minecraft.last())
    else {
        return Err("Chalk supports no Minecraft versions".into());
    };

    let files = [
        (
            "chalk.toml".to_owned(),
            format!(
                "description = \"{slug}\"\nminecraft = \"{}-{}\"\n",
                oldest.version, newest.version
            ),
        ),
        (".gitignore".to_owned(), "/build\n".to_owned()),
        (
            format!("datapack/data/{namespace}/function/hello.mcfunction"),
            "setblock ~ ~ ~ minecraft:gold_block\n".to_owned(),
        ),
        (
            "tests/hello.mcfunction".to_owned(),
            format!(
                "# hello places a gold block where it runs.\n\
                 execute positioned 0 100 0 run function {namespace}:hello\n\
                 execute unless block 0 100 0 minecraft:gold_block run say no gold block at 0 100 0\n"
            ),
        ),
    ];
    for (path, contents) in files {
        let path = dir.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)?;
    }
    Ok(())
}

/// The pack's namespace: its slug with dashes turned into underscores.
fn namespace(slug: &str) -> Result<String> {
    let namespace = slug.to_ascii_lowercase().replace('-', "_");
    let valid = !namespace.is_empty()
        && namespace.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.')
        });
    if valid {
        Ok(namespace)
    } else {
        Err(format!(
            "{slug} can't become a namespace; use letters, digits, dashes, underscores, and dots"
        )
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PackRoot;
    use crate::check;

    #[test]
    fn a_new_pack_supports_every_version_chalk_tests() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("My-Pack");
        init(&repo).expect("init");

        let support = check::support(&PackRoot::at(&repo).expect("root")).expect("support");
        assert_eq!(
            support.minecraft.len(),
            versions().expect("versions").minecraft.len()
        );
        assert!(
            repo.join("datapack/data/my_pack/function/hello.mcfunction")
                .is_file()
        );
        assert!(init(&repo).is_err(), "init must not overwrite a pack");
    }

    #[test]
    fn slugs_that_cannot_be_namespaces_are_rejected() {
        assert!(namespace("portals!").is_err());
    }
}
