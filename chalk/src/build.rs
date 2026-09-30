use crate::check::files;
use crate::{PackRoot, Result};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// Zips `datapack/` into the archive players drop into their world's `datapacks` folder.
pub fn build(root: &PackRoot) -> Result<PathBuf> {
    let pack_dir = root.pack_dir();
    let out = root.build_dir().join(format!("{}.zip", root.slug()));
    fs::create_dir_all(root.build_dir())?;
    let mut zip = ZipWriter::new(File::create(&out)?);
    // Fixed timestamps keep the archive identical for identical sources.
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for path in files(&pack_dir)? {
        let relative = path
            .strip_prefix(&pack_dir)
            .map_err(|_| format!("{} is outside the pack", path.display()))?;
        let name: Vec<String> = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect();
        zip.start_file(name.join("/"), options)?;
        zip.write_all(&fs::read(&path)?)?;
    }
    zip.finish()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_archive_has_the_pack_at_its_root() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("my-pack");
        fs::create_dir_all(repo.join("datapack/data/my_pack/function")).expect("pack dirs");
        fs::write(repo.join("datapack/pack.mcmeta"), "{}").expect("pack.mcmeta");
        fs::write(
            repo.join("datapack/data/my_pack/function/load.mcfunction"),
            "say hi",
        )
        .expect("function");

        let archive = build(&PackRoot::at(&repo).expect("root")).expect("build");

        let names: Vec<String> = zip::ZipArchive::new(File::open(archive).expect("archive"))
            .expect("zip")
            .file_names()
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(
            names,
            vec!["data/my_pack/function/load.mcfunction", "pack.mcmeta"]
        );
    }
}
