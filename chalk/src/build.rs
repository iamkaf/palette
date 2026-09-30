use crate::source::Pack;
use crate::{PackRoot, Result, mcmeta};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// Builds the zip players drop into their world's `datapacks` folder: the generated
/// `pack.mcmeta`, every file for all versions, and one overlay per version range.
pub fn build(root: &PackRoot, pack: &Pack) -> Result<PathBuf> {
    let out = root.build_dir().join(format!("{}.zip", root.slug()));
    fs::create_dir_all(root.build_dir())?;
    let mut zip = ZipWriter::new(File::create(&out)?);
    // Fixed timestamps keep the archive identical for identical sources.
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    zip.start_file("pack.mcmeta", options)?;
    zip.write_all(serde_json::to_string_pretty(&mcmeta::generate(pack)?)?.as_bytes())?;
    for file in &pack.files {
        zip.start_file(file.path.as_str(), options)?;
        zip.write_all(&fs::read(&file.source)?)?;
    }
    for overlay in &pack.overlays {
        for file in &overlay.files {
            zip.start_file(format!("{}/{}", overlay.directory, file.path), options)?;
            zip.write_all(&fs::read(&file.source)?)?;
        }
    }
    zip.finish()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source;
    use crate::versions::versions;
    use std::io::Read;

    #[test]
    fn variants_land_in_a_generated_overlay() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("my-pack");
        fs::create_dir_all(repo.join("datapack/data/demo/tags/block")).expect("pack dirs");
        fs::write(
            repo.join("chalk.toml"),
            "description = \"Demo\"\nminecraft = \"1.21.1-26.3\"\n",
        )
        .expect("manifest");
        fs::write(
            repo.join("datapack/data/demo/tags/block/frame.json"),
            "{\"values\": []}",
        )
        .expect("base");
        fs::write(
            repo.join("datapack/data/demo/tags/block/frame@-26.2.json"),
            "{\"values\": [\"minecraft:obsidian\"]}",
        )
        .expect("variant");
        let root = PackRoot::at(&repo).expect("root");
        let pack = source::load(&root, &versions().expect("versions")).expect("pack");

        let mut archive =
            zip::ZipArchive::new(File::open(build(&root, &pack).expect("build")).expect("zip"))
                .expect("archive");

        let mut names: Vec<String> = archive.file_names().map(str::to_owned).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "1.21-26.2/data/demo/tags/block/frame.json",
                "data/demo/tags/block/frame.json",
                "pack.mcmeta"
            ]
        );
        let mut mcmeta = String::new();
        archive
            .by_name("pack.mcmeta")
            .expect("mcmeta")
            .read_to_string(&mut mcmeta)
            .expect("read");
        let mcmeta: serde_json::Value = serde_json::from_str(&mcmeta).expect("json");
        assert_eq!(mcmeta["overlays"]["entries"][0]["directory"], "1.21-26.2");
    }
}
