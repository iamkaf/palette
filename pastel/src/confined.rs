//! Writes that stay inside one folder even when it contains symbolic links.
//!
//! Pack overrides and Java archives choose their own paths. Paths are
//! validated before they get here; this also refuses to follow a link that
//! points out of the folder.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};

pub struct Root {
    path: PathBuf,
}

impl Root {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            path: path.canonicalize()?,
        })
    }

    /// Creates `rel` and its parents, returning the real directory path.
    pub fn create_dir_all(&self, rel: &Path) -> io::Result<PathBuf> {
        let mut current = self.path.clone();
        for component in rel.components() {
            let Component::Normal(name) = component else {
                if component == Component::CurDir {
                    continue;
                }
                return Err(escape(rel));
            };
            let next = current.join(name);
            match fs::symlink_metadata(&next) {
                Ok(metadata) if metadata.is_dir() => current = next,
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    let target = self.inside(&next)?;
                    if !target.is_dir() {
                        return Err(not_a_directory(&next));
                    }
                    current = target;
                }
                Ok(_) => return Err(not_a_directory(&next)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match fs::create_dir(&next) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                    current = next;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(current)
    }

    /// Creates or truncates the file at `rel`. `mode` applies to new files on Unix.
    pub fn create_file(&self, rel: &Path, mode: Option<u32>) -> io::Result<File> {
        let target = self.file_target(rel)?;
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        options.open(target)
    }

    /// Creates a symbolic link at `rel` pointing at `target`, replacing any file there.
    #[cfg(unix)]
    pub fn symlink(&self, target: &Path, rel: &Path) -> io::Result<()> {
        let parent = self.create_dir_all(rel.parent().unwrap_or(Path::new("")))?;
        let name = rel.file_name().ok_or_else(|| escape(rel))?;
        let link = parent.join(name);
        match fs::remove_file(&link) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::os::unix::fs::symlink(target, link)
    }

    fn file_target(&self, rel: &Path) -> io::Result<PathBuf> {
        let parent = self.create_dir_all(rel.parent().unwrap_or(Path::new("")))?;
        let name = rel.file_name().ok_or_else(|| escape(rel))?;
        let target = parent.join(name);
        match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_symlink() => self.inside(&target),
            _ => Ok(target),
        }
    }

    /// Resolves a link and checks that it lands inside the root.
    fn inside(&self, link: &Path) -> io::Result<PathBuf> {
        let target = link.canonicalize()?;
        if target.starts_with(&self.path) {
            Ok(target)
        } else {
            Err(escape(link))
        }
    }
}

fn escape(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{} leads outside the folder", path.display()),
    )
}

fn not_a_directory(path: &Path) -> io::Error {
    io::Error::other(format!("{} is not a directory", path.display()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn refuses_links_that_leave_the_root() {
        let outside = tempfile::tempdir().unwrap();
        let root_dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root_dir.path().join("config")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("eula.txt"),
            root_dir.path().join("eula.txt"),
        )
        .unwrap();
        std::fs::create_dir(root_dir.path().join("real")).unwrap();
        std::os::unix::fs::symlink("real", root_dir.path().join("inner")).unwrap();

        let root = Root::open(root_dir.path()).unwrap();
        assert!(
            root.create_file(Path::new("config/demo.toml"), None)
                .is_err()
        );
        assert!(root.create_file(Path::new("eula.txt"), None).is_err());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());

        root.create_file(Path::new("inner/demo.toml"), None)
            .unwrap();
        assert!(root_dir.path().join("real/demo.toml").is_file());
    }
}
