//! Lexical path helpers that match the Go release's path handling, so server
//! folders and process command lines compare the same way.

use std::env;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Lexically normalizes `path`: drops `.`, folds `..` into the parent, and
/// never climbs above a root.
pub fn clean(path: &Path) -> PathBuf {
    let mut out: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(component),
            },
            _ => out.push(component),
        }
    }
    if out.is_empty() {
        PathBuf::from(".")
    } else {
        out.iter().collect()
    }
}

/// The working directory, preferring `$PWD` when it names the same folder.
///
/// Background servers are recognized by the server path in their command line,
/// so this keeps the spelling a shell (and earlier Pastel releases) used.
pub fn current_dir() -> io::Result<PathBuf> {
    #[cfg(unix)]
    if let Some(pwd) = env::var_os("PWD").map(PathBuf::from)
        && pwd.is_absolute()
        && same_file(&pwd, Path::new("."))
    {
        return Ok(pwd);
    }
    env::current_dir()
}

#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (a.metadata(), b.metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Makes `path` absolute against [`current_dir`] and cleans it.
pub fn absolute(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(clean(path));
    }
    #[cfg(windows)]
    {
        Ok(clean(&std::path::absolute(path)?))
    }
    #[cfg(not(windows))]
    {
        Ok(clean(&current_dir()?.join(path)))
    }
}

/// Joins a slash-separated relative path onto `root` with native separators.
pub fn join_slash(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .filter(|part| !part.is_empty())
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_folds_dots_without_leaving_the_root() {
        assert_eq!(
            clean(Path::new("/srv/./mc/../pack/")),
            Path::new("/srv/pack")
        );
        assert_eq!(clean(Path::new("/../srv")), Path::new("/srv"));
        assert_eq!(clean(Path::new("a/../../b")), Path::new("../b"));
        assert_eq!(clean(Path::new("")), Path::new("."));
    }
}
