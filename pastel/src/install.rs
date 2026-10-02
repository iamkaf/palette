//! `pastel install`: turns what someone typed into a pack pin, writes
//! `server.pastel`, and applies the pack.
//!
//! ```text
//! ./pastel install https://…/pack.mrpack
//! ./pastel install https://modrinth.com/modpack/aristea
//! ./pastel install aristea
//! ./pastel install modrinth:aristea
//! ./pastel install com.example.modpacks:example-pack:1.2.0 -repo https://maven.example.com
//! ```

use crate::cli::{self, Common};
use crate::flags::{self, bool_flag, text_flag};
use crate::{Error, Result, config, modrinth, pack, paths, ui};
use std::path::Path;

pub fn install(args: &[String]) -> Result<()> {
    // Flags may come before or after the pack.
    let (flag_args, positional) = split_flags(args);
    let parsed = flags::parse(
        "install",
        &[
            text_flag("memory", "4G", "server memory (-Xmx)"),
            text_flag(
                "repo",
                "",
                "Maven repository base (comma-separated for several)",
            ),
            text_flag("dir", "", "server folder (default: current directory)"),
            bool_flag("yes", "overwrite existing server.pastel without asking"),
            bool_flag(
                "no-refresh",
                "only write server.pastel; do not download yet",
            ),
            text_flag(
                "version",
                "",
                "Modrinth version id or version number (optional)",
            ),
        ],
        &flag_args,
    )?;
    let Some(target) = positional
        .first()
        .map(|target| target.trim())
        .filter(|target| !target.is_empty())
    else {
        print_install_help();
        return Err(Error::explained("install needs a modpack"));
    };

    ui::banner();
    ui::blank();
    ui::title("Install pack");
    let dir = parsed.text("dir");
    let root = if dir.is_empty() {
        paths::current_dir()?
    } else {
        paths::absolute(Path::new(dir))?
    };
    std::fs::create_dir_all(&root)?;

    let config_path = root.join(config::FILE_NAME);
    if config_path.is_file() && !parsed.bool("yes") {
        ui::warn(&format!(
            "This folder already has a {}.",
            ui::blue("server.pastel")
        ));
        ui::detail(&config_path.display().to_string());
        if !cli::confirm(&format!(
            "Replace the pack pin and reinstall? {} ",
            ui::pink("[y/N]")
        ))? {
            ui::info("Cancelled — no changes made.");
            return Ok(());
        }
    }
    // Never reinstall under a running server.
    cli::require_server_stopped(&root)?;

    ui::step(&format!("Figuring out {}…", ui::pink(target)));
    let acquired = acquire(
        target,
        parsed.text("version"),
        &split_repos(parsed.text("repo")),
    )?;
    ui::ok(&acquired.title);
    if !acquired.version.is_empty() {
        ui::detail(&format!("version {}", acquired.version));
    }
    ui::detail(&format!("pin {}", acquired.pin));
    if !acquired.repositories.is_empty() {
        ui::detail(&format!("repos {}", acquired.repositories.join(", ")));
    }

    config::write(
        &config_path,
        &acquired.pin,
        parsed.text("memory"),
        &acquired.repositories,
    )
    .map_err(|error| error.context("write server.pastel"))?;
    ui::ok(&format!("Wrote {}", ui::blue("server.pastel")));

    if parsed.bool("no-refresh") {
        ui::blank();
        ui::step(&format!(
            "Next: {} then {}",
            ui::blue("./pastel refresh"),
            ui::blue("./pastel run")
        ));
        return Ok(());
    }

    let config = config::load(&config_path)?;
    let mut resolved =
        cli::load_pack(&config).map_err(|error| error.context("couldn't load the modpack"))?;
    ui::blank();
    ui::step(&format!(
        "Downloading {}…",
        ui::pink(&resolved.manifest.name)
    ));
    let common = Common::with_config(&config_path.display().to_string());
    let outcome = cli::apply(&common, &config, &mut resolved)
        .map_err(|error| error.context("install failed"))?;
    cli::print_summary(
        &outcome,
        false,
        &resolved.manifest.name,
        &resolved.manifest.version,
        &cli::next_step_hint(),
    );
    ui::big_ok("Pack installed");
    Ok(())
}

/// A pin worked out from what someone typed.
struct Acquired {
    pin: String,
    title: String,
    version: String,
    repositories: Vec<String>,
}

impl Acquired {
    fn path(path: &Path) -> Result<Self> {
        let path = paths::absolute(path)?;
        Ok(Self {
            title: path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
            pin: path.display().to_string(),
            version: String::new(),
            repositories: Vec::new(),
        })
    }
}

fn acquire(target: &str, version_flag: &str, repositories: &[String]) -> Result<Acquired> {
    let target = target.trim();
    let with_flag = |version: String| {
        if version_flag.is_empty() {
            version
        } else {
            version_flag.to_owned()
        }
    };
    // modrinth:slug[:version], or a modrinth.com page.
    if let Some((slug, version)) =
        modrinth::parse_ref(target).or_else(|| modrinth::parse_page_url(target))
    {
        return acquire_modrinth(&slug, &with_flag(version));
    }
    // A direct .mrpack URL.
    let lower = target.to_lowercase();
    if lower.starts_with("http://") {
        return Err("pack URLs must use https:// so the download can't be tampered with".into());
    }
    if lower.starts_with("https://") {
        if !lower.contains(".mrpack") {
            return Err("that URL doesn't look like a .mrpack or a Modrinth modpack page".into());
        }
        let title = reqwest::Url::parse(target)
            .ok()
            .and_then(|url| {
                url.path_segments()
                    .and_then(|mut segments| segments.next_back().map(str::to_owned))
            })
            .unwrap_or_else(|| target.rsplit('/').next().unwrap_or(target).to_owned());
        return Ok(Acquired {
            pin: target.to_owned(),
            title,
            version: String::new(),
            repositories: Vec::new(),
        });
    }
    // A local pack file, or an extracted pack folder.
    let path = Path::new(target);
    if path.is_file() || (path.is_dir() && path.join("modrinth.index.json").exists()) {
        return Acquired::path(path);
    }
    // A Maven coordinate, checked before the slug:version shorthand.
    if pack::is_maven_coordinate(target) {
        if repositories.is_empty() {
            return Err(
                format!("Maven pack {target:?} needs -repo https://… (no default host)").into(),
            );
        }
        // Probe now, so nothing is written for a pack that can't load.
        pack::resolve(target, repositories, None)?;
        return Ok(Acquired {
            pin: target.to_owned(),
            title: target.to_owned(),
            version: String::new(),
            repositories: repositories.to_vec(),
        });
    }
    // aristea@0.1.4 or aristea:0.1.4
    if let Some((slug, version)) = modrinth::parse_slug_version(target) {
        return acquire_modrinth(&slug, &with_flag(version));
    }
    // A bare slug means the latest release on Modrinth.
    if modrinth::looks_like_slug(target) {
        return acquire_modrinth(target, version_flag);
    }
    Err(format!(
        "don't know how to install {target:?} — try a Modrinth slug, modpack page URL, or .mrpack link"
    )
    .into())
}

fn acquire_modrinth(slug: &str, version: &str) -> Result<Acquired> {
    let pack = modrinth::Client::new()?.resolve_modpack(slug, version)?;
    let title = if pack.project.title.is_empty() {
        pack.project.slug
    } else {
        pack.project.title
    };
    Ok(Acquired {
        pin: pack.pin,
        title,
        version: pack.version.version_number,
        repositories: Vec::new(),
    })
}

fn split_repos(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|repo| !repo.is_empty())
        .map(String::from)
        .collect()
}

fn print_install_help() {
    ui::banner();
    ui::blank();
    ui::title("Install a modpack");
    ui::info(&format!(
        "{} will write {} and download the pack into this folder.",
        ui::brand(),
        ui::blue("server.pastel")
    ));
    ui::blank();
    ui::step("Pick one of these:");
    ui::blank();
    for (example, detail) in [
        (
            ui::blue("./pastel install aristea"),
            "Modrinth modpack (pins the latest version)",
        ),
        (
            format!(
                "{}   or   {}",
                ui::blue("./pastel install aristea:0.1.4"),
                ui::blue("aristea@0.1.4")
            ),
            "Specific version",
        ),
        (
            ui::blue("./pastel install https://modrinth.com/modpack/aristea"),
            "Modrinth page link",
        ),
        (
            ui::blue("./pastel install https://…/pack.mrpack"),
            "Direct pack file",
        ),
        (
            ui::blue(
                "./pastel install com.example.modpacks:example-pack:1.2.0 -repo https://maven.example.com",
            ),
            "Maven coordinate (needs -repo)",
        ),
    ] {
        ui::out(&format!("  {example}"));
        ui::detail(detail);
    }
    ui::blank();
    ui::info(&format!(
        "Then:  {}  →  {}",
        ui::blue("./pastel run"),
        ui::blue("./pastel console")
    ));
    ui::blank();
    ui::detail("Optional: -memory 4G  ·  -version 1.2.3  ·  -yes  ·  -no-refresh");
}

/// Pulls `-flag`, `--flag`, and `-flag value` out of `args`, so flags may follow
/// the pack target.
fn split_flags(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut flags = Vec::new();
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        index += 1;
        if arg == "--" {
            positional.extend(args[index..].iter().cloned());
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            positional.push(arg.clone());
            continue;
        }
        flags.push(arg.clone());
        let name = arg.trim_start_matches('-');
        let takes_value = matches!(name, "memory" | "repo" | "dir" | "version" | "config");
        if takes_value
            && let Some(value) = args.get(index)
            && !value.starts_with('-')
        {
            flags.push(value.clone());
            index += 1;
        }
    }
    (flags, positional)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|text| (*text).to_owned()).collect()
    }

    #[test]
    fn flags_may_follow_the_pack() {
        let (flags, positional) = split_flags(&strings(&[
            "aristea",
            "-memory",
            "6G",
            "--yes",
            "-version=1.0",
        ]));
        assert_eq!(flags, strings(&["-memory", "6G", "--yes", "-version=1.0"]));
        assert_eq!(positional, strings(&["aristea"]));
    }

    #[test]
    fn local_packs_pin_their_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("pack.mrpack");
        std::fs::write(&pack, "PK").unwrap();
        let acquired = acquire(pack.to_str().unwrap(), "", &[]).unwrap();
        assert_eq!(
            acquired.pin,
            paths::absolute(&pack).unwrap().display().to_string()
        );
        assert_eq!(acquired.title, "pack.mrpack");
    }

    #[test]
    fn rejects_plain_http_and_unknown_urls() {
        assert!(acquire("http://example.com/pack.mrpack", "", &[]).is_err());
        assert!(acquire("https://example.com/pack.zip", "", &[]).is_err());
        assert!(acquire("com.example:pack:1.0.0", "", &[]).is_err());
    }
}
