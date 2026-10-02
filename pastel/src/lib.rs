use std::fmt;
use std::io;

pub mod cli;
pub mod config;
pub mod confined;
pub mod fetch;
pub mod flags;
pub mod http;
pub mod install;
pub mod jre;
pub mod maven;
pub mod mcprops;
pub mod modrinth;
pub mod pack;
pub mod paths;
pub mod runtime;
pub mod selfupdate;
pub mod state;
pub mod sync;
#[cfg(test)]
mod testing;
pub mod ui;
pub mod version;

pub const USER_AGENT: &str = concat!(
    "Pastel/",
    env!("CARGO_PKG_VERSION"),
    " (+https://kaf.sh/pastel)"
);

/// A failure with a message for the server owner.
///
/// `explained` marks errors whose plain-language explanation was already
/// printed, so the CLI exits without showing a second error box.
#[derive(Debug)]
pub struct Error {
    message: String,
    explained: bool,
}

impl Error {
    pub fn explained(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            explained: true,
        }
    }

    pub fn is_explained(&self) -> bool {
        self.explained
    }

    /// Prefixes the message, keeping whether it was already explained.
    pub fn context(self, prefix: impl fmt::Display) -> Self {
        Self {
            message: format!("{prefix}: {}", self.message),
            explained: self.explained,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self {
            message,
            explained: false,
        }
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

macro_rules! impl_from_error {
    ($($source:ty),* $(,)?) => {
        $(impl From<$source> for Error {
            fn from(value: $source) -> Self {
                value.to_string().into()
            }
        })*
    };
}

impl_from_error!(
    io::Error,
    reqwest::Error,
    serde_json::Error,
    toml::de::Error,
    toml_edit::TomlError,
    quick_xml::DeError,
    zip::result::ZipError,
);

#[cfg(unix)]
impl_from_error!(nix::errno::Errno);

pub type Result<T> = std::result::Result<T, Error>;

/// Adds a message prefix to any error that converts into [`Error`].
pub trait Context<T> {
    fn context(self, prefix: impl fmt::Display) -> Result<T>;
}

impl<T, E: Into<Error>> Context<T> for std::result::Result<T, E> {
    fn context(self, prefix: impl fmt::Display) -> Result<T> {
        self.map_err(|error| error.into().context(prefix))
    }
}
