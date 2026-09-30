use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

pub mod build;
pub mod check;
pub mod dev;
pub mod game;
pub mod init;
pub mod mcmeta;
pub mod modstage;
pub mod pack;
pub mod pair;
pub mod problems;
pub mod publish;
pub mod rcon;
pub mod source;
pub mod teakit;
pub mod versions;

pub const TOOL_NAME: &str = "chalk";
pub const USER_AGENT: &str = concat!(
    "chalk/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/iamkaf/palette)"
);

#[derive(Debug)]
pub struct Error(String);

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

impl From<toml::ser::Error> for Error {
    fn from(value: toml::ser::Error) -> Self {
        Self(value.to_string())
    }
}

impl From<zip::result::ZipError> for Error {
    fn from(value: zip::result::ZipError) -> Self {
        Self(value.to_string())
    }
}

impl From<palette_publish::Error> for Error {
    fn from(value: palette_publish::Error) -> Self {
        Self(value.to_string())
    }
}

impl From<Error> for palette_publish::Error {
    fn from(value: Error) -> Self {
        Self::from(value.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A datapack repository: `chalk.toml`, the pack's files in `datapack/`, and TeaKit tests
/// in `tests/`. Everything Chalk generates goes under `build/chalk/`.
pub struct PackRoot {
    dir: PathBuf,
    slug: String,
}

impl PackRoot {
    /// Finds the repository containing `start` by looking for `chalk.toml`.
    pub fn discover(start: &Path) -> Result<Self> {
        let mut current = Some(start);
        while let Some(dir) = current {
            if dir.join("chalk.toml").is_file() {
                return Self::at(dir);
            }
            current = dir.parent();
        }
        Err(format!(
            "no chalk.toml in {} or any parent directory",
            start.display()
        )
        .into())
    }

    pub fn at(dir: &Path) -> Result<Self> {
        let dir = dir.canonicalize()?;
        let slug = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| {
                format!(
                    "{} has no directory name to use as the pack slug",
                    dir.display()
                )
            })?;
        Ok(Self { dir, slug })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The repository directory name, used for world folders, instances, and archives.
    pub fn slug(&self) -> &str {
        &self.slug
    }

    pub fn manifest(&self) -> PathBuf {
        self.dir.join("chalk.toml")
    }

    pub fn pack_dir(&self) -> PathBuf {
        self.dir.join("datapack")
    }

    pub fn tests_dir(&self) -> PathBuf {
        self.dir.join("tests")
    }

    pub fn build_dir(&self) -> PathBuf {
        self.dir.join("build").join(TOOL_NAME)
    }
}
