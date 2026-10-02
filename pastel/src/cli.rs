//! Pastel's commands and their friendly output.

use crate::config::{self, Config};
use crate::flags::{self, Flag, Kind, bool_flag, int_flag, text_flag};
use crate::pack::{self, MavenRef, PinKind, Resolved};
use crate::runtime::{self, StopTarget};
use crate::state::{self, State};
use crate::sync::{self, Outcome};
use crate::ui::{self, SyncReport};
use crate::{Error, Result, install, jre, maven, mcprops, modrinth, paths, selfupdate, version};
use std::io::{self, BufRead, IsTerminal};
use std::path::{Path, PathBuf};

/// Dispatches `args` (without the program name). Bare `pastel` is the home
/// screen: status and suggestions. It never starts the server.
pub fn run(args: &[String]) -> Result<()> {
    let Some(command) = args.first() else {
        return friendly(home(&[]));
    };
    let rest = &args[1..];
    match command.as_str() {
        "install" | "get" | "add" => friendly(install::install(rest)),
        "run" => friendly(run_server(rest)),
        "console" | "attach" | "logs" | "terminal" => friendly(console(rest)),
        // `sync` is the old name.
        "refresh" | "sync" => friendly(refresh(rest)),
        "status" => friendly(status(rest)),
        "home" | "hi" | "hello" => friendly(home(rest)),
        "stop" => friendly(stop(rest)),
        "update" | "upgrade" => friendly(update(rest)),
        "self-update" | "selfupdate" => friendly(self_update(rest)),
        // Internal: keeps the console FIFO open for a background server.
        "__hold-fifo" => match rest.first() {
            Some(path) => runtime::hold_fifo(Path::new(path)),
            None => Err("usage: pastel __hold-fifo <path>".into()),
        },
        // Internal: owns the background Java process and restarts it after crashes.
        "__supervise" => supervise(rest),
        "version" | "-version" | "--version" => {
            ui::banner();
            ui::detail(&format!("version {}", env!("CARGO_PKG_VERSION")));
            Ok(())
        }
        "help" | "-h" | "--help" => {
            ui::help_block();
            Ok(())
        }
        // Flags alone still show home, so nothing starts by surprise.
        other if other.starts_with('-') => friendly(home(args)),
        other => friendly(Err(format!(
            "I don't know the command {other:?} — try: ./pastel  (or ./pastel help)"
        )
        .into())),
    }
}

fn supervise(args: &[String]) -> Result<()> {
    let usage = "usage: pastel __supervise <root> <auto-restart> -- <java> [args...]";
    let [root, auto_restart, separator, java, java_args @ ..] = args else {
        return Err(usage.into());
    };
    if separator != "--" {
        return Err(usage.into());
    }
    let auto_restart = match auto_restart.as_str() {
        "true" => true,
        "false" => false,
        other => return Err(format!("invalid auto-restart value: {other:?}").into()),
    };
    runtime::supervise(Path::new(root), java, java_args, auto_restart)
}

/// Prints an error once, in plain language, unless it was already explained.
fn friendly(result: Result<()>) -> Result<()> {
    let Err(error) = result else {
        return Ok(());
    };
    if error.is_explained() {
        return Err(error);
    }
    let message = error.to_string();
    ui::blank();
    ui::error_message("Something went wrong", &message, &tips_for(&message));
    Err(Error::explained(message))
}

fn tips_for(message: &str) -> Vec<&'static str> {
    let low = message.to_lowercase();
    let mut tips = Vec::new();
    if low.contains("server.pastel") {
        tips.push("First time?  ./pastel install <modpack-slug-or-url>");
    }
    if low.contains("no maven repositories") {
        tips.push(
            "Add repositories = [\"https://…\"] to server.pastel, or pin an https://…/.mrpack URL",
        );
    }
    if low.contains("not a modpack") || low.contains("not found") {
        tips.push("Check the Modrinth slug or page URL for the modpack");
    }
    if low.contains("java") {
        tips.push("Install Java and make sure the java command works in Terminal");
    }
    if low.contains("get ") || low.contains("fetch") || low.contains("download") {
        tips.push("Check your internet connection and try again");
    }
    if tips.is_empty() {
        tips.push("Run ./pastel for a friendly overview");
    }
    tips
}

const CONFIG: Flag = text_flag("config", "", "path to server.pastel");
const VERBOSE: Flag = bool_flag("v", "verbose logs");
const NO_PRUNE: Flag = bool_flag("no-prune", "do not prune extra mods");
const DRY_RUN: Flag = bool_flag("dry-run", "plan only");

/// The flags most commands share.
pub(crate) struct Common {
    pub config: String,
    root: String,
    dry_run: bool,
    pub prune: bool,
    verbose: bool,
}

fn parse_common(args: &[String], with_dry_run: bool) -> Result<Common> {
    let mut flags = vec![
        CONFIG,
        text_flag("root", "", "server root directory"),
        VERBOSE,
    ];
    if with_dry_run {
        flags.extend([
            DRY_RUN,
            Flag {
                name: "prune",
                kind: Kind::Bool,
                default: "true",
                usage: "prune unlisted jars under mods/",
            },
            NO_PRUNE,
        ]);
    }
    let parsed = flags::parse("pastel", &flags, args)?;
    Ok(Common {
        config: parsed.text("config").to_owned(),
        root: parsed.text("root").to_owned(),
        dry_run: parsed.bool("dry-run"),
        prune: !with_dry_run || (parsed.bool("prune") && !parsed.bool("no-prune")),
        verbose: parsed.bool("v"),
    })
}

impl Common {
    pub(crate) fn with_config(config: &str) -> Self {
        Self {
            config: config.to_owned(),
            root: String::new(),
            dry_run: false,
            prune: true,
            verbose: false,
        }
    }
}

fn load_instance(common: &Common) -> Result<Config> {
    let path = if common.config.is_empty() {
        config::find(&paths::current_dir()?)
            .map_err(|_| "couldn't find server.pastel in this folder")?
    } else {
        PathBuf::from(&common.config)
    };
    let mut config = config::load(&path)?;
    if !common.root.is_empty() {
        config.server_dir = common.root.clone();
    }
    Ok(config)
}

pub(crate) fn load_pack(config: &Config) -> Result<Resolved> {
    let mut raw = config.pack.trim().to_owned();
    // Only plain relative paths are relative to server.pastel; never join pins.
    if !raw.is_empty()
        && pack::classify_pin(&raw) == PinKind::Path
        && !Path::new(&raw).is_absolute()
    {
        let dir = config.path().parent().unwrap_or(Path::new("."));
        raw = paths::clean(&dir.join(&raw)).display().to_string();
    }
    let cache = config.root().join(".pastel").join("cache").join("packs");
    pack::resolve(&raw, &config.maven_repositories(), Some(&cache))
}

pub(crate) fn apply(common: &Common, config: &Config, resolved: &mut Resolved) -> Result<Outcome> {
    let mut report = SyncReport::new(common.verbose);
    sync::run(sync::Options {
        root: &config.root(),
        manifest: &mut resolved.manifest,
        pack_coordinate: &resolved.coordinate,
        mrpack: &resolved.mrpack,
        prune_mods: common.prune,
        dry_run: common.dry_run,
        report: &mut report,
    })
}

pub(crate) fn next_step_hint() -> String {
    format!(
        "Start the server with {}, then {} for the live log.",
        ui::blue("./pastel run"),
        ui::blue("./pastel console")
    )
}

/// The result box. `next_step` shows after a real apply, not a preview.
pub(crate) fn print_summary(
    outcome: &Outcome,
    dry: bool,
    name: &str,
    version: &str,
    next_step: &str,
) {
    ui::blank();
    ui::title(if dry { "Preview" } else { "All set" });
    let mut lines = vec![format!(
        "{}  {}",
        ui::pink(name),
        ui::blue(&format!("v{version}"))
    )];
    let downloaded = ui::bold(&outcome.downloaded.to_string());
    lines.push(match (outcome.downloaded, dry) {
        (0, _) => "Everything was already up to date".to_owned(),
        (_, true) => format!("{downloaded} file(s) would be updated"),
        (_, false) => format!("{downloaded} file(s) downloaded or updated"),
    });
    if outcome.overrides > 0 {
        lines.push(if dry {
            "overrides would apply".to_owned()
        } else {
            format!(
                "{} override file(s) applied",
                ui::bold(&outcome.overrides.to_string())
            )
        });
    }
    if outcome.loader {
        lines.push("loader installed".to_owned());
    }
    if outcome.unchanged > 0 {
        lines.push(format!("{} item(s) already good", outcome.unchanged));
    }
    if !outcome.pruned.is_empty() {
        let count = outcome.pruned.len();
        lines.push(if dry {
            format!("{count} extra mod(s) would be removed")
        } else {
            format!("{count} extra mod(s) removed")
        });
    }
    ui::summary_box(&lines);
    if !dry && !next_step.is_empty() {
        ui::blank();
        ui::title("Next step");
        ui::step(next_step);
    }
}

pub(crate) fn require_server_stopped(root: &Path) -> Result<()> {
    let Some(pid) = runtime::running(root) else {
        return Ok(());
    };
    ui::blank();
    ui::title("Hold on — the server is running");
    ui::detail(&format!("process {pid}"));
    ui::info("Pastel won't change mods or configs while the game is up.");
    ui::info("That can break the world for anyone who's playing.");
    ui::blank();
    ui::step(&format!(
        "Stop the server first:  {}",
        ui::blue("./pastel stop")
    ));
    ui::detail(&format!(
        "If that says it's not running:  {}",
        ui::blue(&format!("./pastel stop -pid {pid}"))
    ));
    ui::detail(&format!(
        "Deleted the folder while it was up?  {}",
        ui::blue("./pastel stop -orphans")
    ));
    ui::detail("Then run your command again.");
    Err(Error::explained("server is running"))
}

fn refresh(args: &[String]) -> Result<()> {
    let common = parse_common(args, true)?;
    ui::banner();
    ui::blank();
    let config = load_instance(&common)?;
    require_server_stopped(&config.root())?;
    let mut resolved =
        load_pack(&config).map_err(|error| error.context("couldn't load the modpack"))?;
    let label = format!(
        "{} {}",
        ui::pink(&resolved.manifest.name),
        ui::blue(&format!("v{}", resolved.manifest.version))
    );
    if common.dry_run {
        ui::step(&format!("Checking {label} (preview only)…"));
    } else {
        ui::step(&format!("Refreshing {label}…"));
    }
    ui::detail(&resolved.coordinate);
    let outcome =
        apply(&common, &config, &mut resolved).map_err(|error| error.context("refresh failed"))?;
    let next = if common.dry_run {
        String::new()
    } else {
        next_step_hint()
    };
    print_summary(
        &outcome,
        common.dry_run,
        &resolved.manifest.name,
        &resolved.manifest.version,
        &next,
    );
    Ok(())
}

fn run_server(args: &[String]) -> Result<()> {
    let parsed = flags::parse(
        "run",
        &[
            bool_flag("foreground", "keep the server attached to this terminal"),
            bool_flag("f", "short for -foreground"),
            CONFIG,
            bool_flag("v", "verbose"),
            NO_PRUNE,
        ],
        args,
    )?;
    let common = Common {
        prune: !parsed.bool("no-prune"),
        verbose: parsed.bool("v"),
        ..Common::with_config(parsed.text("config"))
    };

    ui::banner();
    ui::blank();
    let config = load_instance(&common)?;
    let root = config.root();
    // Refreshing rewrites mods and configs, so check before touching anything.
    require_server_stopped(&root)?;
    let mut resolved =
        load_pack(&config).map_err(|error| error.context("couldn't load the modpack"))?;
    ui::step(&format!(
        "Getting {} ready…",
        ui::pink(&resolved.manifest.name)
    ));
    ui::detail(&resolved.coordinate);

    if config.sync_on_run() {
        let outcome = apply(&common, &config, &mut resolved)
            .map_err(|error| error.context("refresh failed"))?;
        print_summary(
            &outcome,
            false,
            &resolved.manifest.name,
            &resolved.manifest.version,
            "",
        );
    } else {
        ui::warn("sync_on_run = false — not refreshing pack files (local mods/config kept as-is)");
        ui::detail(&format!(
            "Run {} when you want pack changes again.",
            ui::blue("./pastel refresh")
        ));
        // Still align the loader launcher with the pack, without downloading mods.
        pack::ensure_loader(&root, &mut resolved.manifest)
            .map_err(|error| error.context("loader"))?;
    }

    let manifest = &resolved.manifest;
    let required = jre::require_major(manifest.minecraft());
    ui::blank();
    ui::title("Java");
    ui::detail(&jre::format_requirement(manifest.minecraft()));
    let java = jre::ensure(&root, required, Some(&config.java)).map_err(|error| {
        error.context(format_args!("Java {required} is required for this pack"))
    })?;

    runtime::start(&runtime::Options {
        root: &root,
        java: &java,
        xmx: &config.xmx(),
        manifest,
        extra_args: &config.extra_java_args,
        nogui: config.nogui(),
        java_major: required,
        foreground: parsed.bool("foreground") || parsed.bool("f"),
        auto_restart: config.auto_restart(),
    })
}

fn console(args: &[String]) -> Result<()> {
    let common = parse_common(args, false)?;
    let config = load_instance(&common)?;
    let root = config.root();
    if runtime::running(&root).is_none() {
        ui::banner();
        ui::blank();
        ui::title("Console");
        ui::info("The server isn't running right now — nothing to attach to.");
        ui::blank();
        ui::step(&format!("Start it:  {}", ui::blue("./pastel run")));
        ui::detail(&format!("Then:      {}", ui::blue("./pastel console")));
        ui::blank();
        ui::detail(&format!(
            "If it just crashed, check {}",
            ui::blue("logs/latest.log")
        ));
        return Err(Error::explained("server is not running"));
    }
    runtime::attach(&root)
}

fn home(args: &[String]) -> Result<()> {
    let common = parse_common(args, false)?;
    ui::banner();
    ui::blank();
    ui::out(&format!(
        "{}Hi! {} keeps this Minecraft server's mods and configs in sync.",
        ui::dim("· "),
        ui::brand()
    ));
    ui::info("Nothing starts automatically — pick a command when you're ready.");
    ui::blank();

    let snapshot = match gather_snapshot(&common) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            print_getting_started(&error);
            return Ok(());
        }
    };
    print_snapshot(&snapshot, false);
    ui::blank();
    ui::title("Suggested next step");
    if snapshot.state.is_none() {
        ui::step(&format!(
            "Files not downloaded yet — {} then {}.",
            ui::blue("./pastel refresh"),
            ui::blue("./pastel run")
        ));
    } else if snapshot.update_available {
        ui::step(&format!(
            "A newer pack is out — run {} and pick the version you want.",
            ui::blue("./pastel update")
        ));
    } else if snapshot.running {
        ui::step(&format!(
            "Server is up — {} for logs/commands, or {} to shut down.",
            ui::blue("./pastel console"),
            ui::blue("./pastel stop")
        ));
    } else {
        ui::step(&format!(
            "Ready when you are: {} starts the server in the background.",
            ui::blue("./pastel run")
        ));
    }
    ui::blank();
    ui::info(&format!(
        "{} can update packs, keep mods in sync, and run the server for you.",
        ui::brand()
    ));
    ui::detail(&format!(
        "See every command:  {}",
        ui::blue("./pastel help")
    ));
    Ok(())
}

fn print_getting_started(error: &Error) {
    ui::warn(&error.to_string());
    ui::blank();
    ui::title("Getting started");
    ui::info(&format!(
        "Drop {} in an empty server folder, then install a modpack:",
        ui::brand()
    ));
    ui::blank();
    ui::step(&ui::blue("./pastel install aristea"));
    ui::detail(&format!(
        "or a Modrinth page:  {}",
        ui::blue("./pastel install https://modrinth.com/modpack/aristea")
    ));
    ui::detail(&format!(
        "or a direct pack:    {}",
        ui::blue("./pastel install https://…/pack.mrpack")
    ));
    ui::blank();
    ui::info(&format!(
        "That writes {} and downloads the pack for you.",
        ui::blue("server.pastel")
    ));
    ui::info(&format!(
        "Then {} starts the server.",
        ui::blue("./pastel run")
    ));
}

fn status(args: &[String]) -> Result<()> {
    let common = parse_common(args, false)?;
    ui::banner();
    ui::blank();
    print_snapshot(&gather_snapshot(&common)?, true);
    Ok(())
}

/// What's installed here, what the pin points at, and what's newer.
struct Snapshot {
    root: PathBuf,
    pack_pin: String,
    memory: String,
    java: String,
    state: Option<State>,
    running: bool,
    server: Option<mcprops::Info>,
    pin: std::result::Result<PinnedPack, String>,
    latest: std::result::Result<Option<Latest>, String>,
    update_available: bool,
}

struct PinnedPack {
    name: String,
    version: String,
    minecraft: String,
    loader: &'static str,
    mods: usize,
}

struct Latest {
    name: String,
    version: String,
    minecraft: String,
}

fn gather_snapshot(common: &Common) -> Result<Snapshot> {
    let config = load_instance(common)?;
    let root = config.root();
    let state = state::load(&root)?;
    let pin = load_pack(&config)
        .map(|resolved| {
            let manifest = resolved.manifest;
            PinnedPack {
                minecraft: manifest.minecraft().to_owned(),
                loader: manifest.loader_name(),
                mods: manifest.mod_count(),
                name: manifest.name,
                version: manifest.version,
            }
        })
        .map_err(|error| error.to_string());
    let baseline = state
        .as_ref()
        .map(|state| state.pack_version.clone())
        .filter(|version| !version.is_empty())
        .or_else(|| pin.as_ref().ok().map(|pin| pin.version.clone()))
        .unwrap_or_default();
    let (latest, update_available) = match check_pack_update(&config, &baseline) {
        Ok(Some((latest, available))) => (Ok(Some(latest)), available),
        Ok(None) => (Ok(None), false),
        Err(error) => (Err(error.to_string()), false),
    };
    Ok(Snapshot {
        running: runtime::running(&root).is_some(),
        server: mcprops::read(&root),
        pack_pin: config.pack.clone(),
        memory: config.xmx(),
        java: config.java_bin().to_owned(),
        root,
        state,
        pin,
        latest,
        update_available,
    })
}

/// The newest version for pins with an update channel: Maven coordinates and
/// Modrinth pins. Direct URLs and local files have none.
fn check_pack_update(config: &Config, baseline: &str) -> Result<Option<(Latest, bool)>> {
    if let Some(pin) = MavenRef::parse(&config.pack) {
        let check = pack::check_update(&config.maven_repositories(), &pin, baseline)?;
        return Ok(Some((
            Latest {
                name: check.latest_name,
                version: check.latest_version,
                minecraft: check.latest_minecraft,
            },
            check.available,
        )));
    }
    let Some((slug, _)) =
        modrinth::parse_ref(&config.pack).or_else(|| modrinth::parse_page_url(&config.pack))
    else {
        return Ok(None);
    };
    let check = modrinth::Client::new()?.check_update(&slug, baseline)?;
    Ok(Some((
        Latest {
            name: check.latest_name,
            version: check.latest_version,
            minecraft: check.latest_minecraft,
        },
        check.available,
    )))
}

fn minecraft_and_java(minecraft: &str) -> String {
    format!(
        "Minecraft {minecraft}{}Java {}",
        ui::dim(" · "),
        jre::require_major(minecraft)
    )
}

fn print_snapshot(snapshot: &Snapshot, detailed: bool) {
    ui::title("What you've got");
    if detailed {
        ui::kv("folder", &snapshot.root.display().to_string());
        ui::kv("pack pin", &snapshot.pack_pin);
        ui::kv("memory", &snapshot.memory);
        ui::kv("java", &snapshot.java);
    } else {
        ui::kv("pin", &snapshot.pack_pin);
    }

    ui::blank();
    ui::title("Installed");
    match &snapshot.state {
        None => ui::info(&format!(
            "No pack applied yet — run {} to install your pin.",
            ui::blue("./pastel refresh")
        )),
        Some(state) => {
            let mut minecraft = state.minecraft.clone();
            let mut loader = state.loader.clone();
            let mut mods = state.mod_count;
            // Older state files may lack these; use the pin when it's the same version.
            if let Ok(pin) = &snapshot.pin
                && pin.version == state.pack_version
            {
                if loader.is_empty() {
                    loader = pin.loader.to_owned();
                }
                if mods == 0 {
                    mods = pin.mods;
                }
                if minecraft.is_empty() {
                    minecraft.clone_from(&pin.minecraft);
                }
            }
            ui::kv(
                "pack",
                &format!(
                    "{} {}",
                    state.pack_name,
                    ui::blue(&format!("v{}", state.pack_version))
                ),
            );
            let mut bits = Vec::new();
            if !minecraft.is_empty() {
                bits.push(format!("Minecraft {minecraft}"));
            }
            if !loader.is_empty() {
                bits.push(ui::loader(&loader));
            }
            match mods {
                0 => {}
                1 => bits.push("1 mod".to_owned()),
                count => bits.push(format!("{count} mods")),
            }
            if !minecraft.is_empty() {
                bits.push(format!("Java {}", jre::require_major(&minecraft)));
            }
            if !bits.is_empty() {
                ui::out(&format!("{}{}", " ".repeat(16), bits.join(&ui::dim(" · "))));
            }
            if detailed {
                let applied = state.applied_at.with_timezone(&chrono::Local);
                ui::kv(
                    "updated",
                    &applied.format("%b %-d, %Y %-I:%M %p").to_string(),
                );
                ui::kv("files", &format!("{} tracked", state.file_count));
            }
        }
    }

    ui::blank();
    ui::title("Server");
    match &snapshot.server {
        Some(server) => {
            let whitelist = if server.whitelist {
                "whitelist on"
            } else {
                "whitelist off"
            };
            let dot = ui::dim(" · ");
            ui::out(&format!(
                "  {}{dot}port {}{dot}{whitelist}",
                server.motd, server.port
            ));
        }
        None => ui::info("No server.properties yet — it appears after the first run"),
    }
    if snapshot.running {
        ui::ok("Online");
    } else {
        ui::info("Offline");
    }

    ui::blank();
    ui::title("Updates");
    match (&snapshot.latest, &snapshot.pin) {
        (Err(error), _) => {
            ui::warn("Couldn't check for new versions");
            ui::detail(error);
        }
        (Ok(None), Err(error)) => {
            ui::warn("Couldn't load pin");
            ui::detail(error);
        }
        (Ok(None), Ok(_)) => ui::info("This pin has no update channel — no upgrade check"),
        (Ok(Some(latest)), _) if snapshot.update_available => {
            let mut line = format!(
                "{} {}",
                latest.name,
                ui::blue(&format!("v{}", latest.version))
            );
            if !latest.minecraft.is_empty() {
                line.push_str(&ui::dim(" · "));
                line.push_str(&minecraft_and_java(&latest.minecraft));
            }
            ui::warn(&format!("New version: {line}"));
            ui::detail(&format!("Run {} to upgrade", ui::blue("./pastel update")));
        }
        (Ok(Some(latest)), _) => {
            let mut line = format!(
                "On the latest pack ({})",
                ui::blue(&format!("v{}", latest.version))
            );
            if !latest.minecraft.is_empty() {
                line.push_str(&ui::dim(" · "));
                line.push_str(&minecraft_and_java(&latest.minecraft));
            }
            ui::ok(&line);
        }
    }

    if detailed {
        match &snapshot.pin {
            Ok(pin) if !pin.version.is_empty() => {
                ui::blank();
                ui::title("Your pin");
                ui::kv(
                    "pack",
                    &version::pack_line(&pin.name, &pin.version, &pin.minecraft),
                );
            }
            _ => {}
        }
    }
}

fn stop(args: &[String]) -> Result<()> {
    let parsed = flags::parse(
        "stop",
        &[
            bool_flag("force", "skip graceful stop; SIGTERM/SIGKILL immediately"),
            int_flag(
                "pid",
                "stop this process id (after a deleted server folder)",
            ),
            bool_flag(
                "orphans",
                "stop Minecraft servers whose folder was deleted while running",
            ),
            CONFIG,
        ],
        args,
    )?;
    ui::banner();
    ui::blank();

    // -orphans and -pid work without a server.pastel; the folder may be gone.
    if parsed.bool("orphans") {
        return runtime::stop(Path::new("."), StopTarget::Orphans);
    }
    let config_path = parsed.text("config");
    if let Ok(pid) = u32::try_from(parsed.int("pid"))
        && pid > 0
    {
        let root = if config_path.is_empty() {
            paths::current_dir()?
        } else {
            config::load(Path::new(config_path))?.root()
        };
        return runtime::stop(&root, StopTarget::Pid(pid));
    }

    let target = StopTarget::Server {
        force: parsed.bool("force"),
    };
    match load_instance(&Common::with_config(config_path)) {
        Ok(config) => runtime::stop(&config.root(), target),
        Err(_) => {
            // No server.pastel: still recover processes running from this folder.
            let cwd = paths::current_dir()?;
            ui::detail(&format!(
                "no server.pastel here — checking for processes in {}",
                cwd.display()
            ));
            runtime::stop(&cwd, target)
        }
    }
}

fn update(args: &[String]) -> Result<()> {
    let parsed = flags::parse(
        "update",
        &[
            text_flag(
                "to",
                "",
                "pack version to install (skips the picker if set)",
            ),
            bool_flag("yes", "skip the confirmation prompt (for scripts)"),
            bool_flag("dry-run", "show the plan without applying"),
            bool_flag("v", "verbose"),
            CONFIG,
            NO_PRUNE,
        ],
        args,
    )?;
    let common = Common {
        dry_run: parsed.bool("dry-run"),
        prune: !parsed.bool("no-prune"),
        verbose: parsed.bool("v"),
        ..Common::with_config(parsed.text("config"))
    };
    let config = load_instance(&common)?;
    require_server_stopped(&config.root())?;
    let upgrade = Upgrade {
        common: &common,
        config: &config,
        to: parsed.text("to").trim(),
        yes: parsed.bool("yes"),
    };
    if let Some((slug, _)) =
        modrinth::parse_ref(&config.pack).or_else(|| modrinth::parse_page_url(&config.pack))
    {
        return upgrade.modrinth(&slug);
    }
    if let Some(pin) = MavenRef::parse(&config.pack) {
        return upgrade.maven(&pin);
    }
    Err(format!(
        "pack pin {:?} isn't updatable that way — use ./pastel install for a new pack, or pin modrinth:… / Maven group:artifact:version",
        config.pack
    )
    .into())
}

struct Upgrade<'a> {
    common: &'a Common,
    config: &'a Config,
    /// A version chosen on the command line, or empty for the picker.
    to: &'a str,
    yes: bool,
}

/// One row in the version picker.
struct VersionChoice {
    version: String,
    minecraft: String,
    /// The pin written to server.pastel.
    pin: String,
    /// Such as `beta` on Modrinth.
    channel: String,
    resolved: Option<Resolved>,
}

impl Upgrade<'_> {
    fn installed(&self) -> Result<Option<State>> {
        state::load(&self.config.root())
    }

    fn cache(&self) -> PathBuf {
        self.config
            .root()
            .join(".pastel")
            .join("cache")
            .join("packs")
    }

    fn maven(&self, pin: &MavenRef) -> Result<()> {
        ui::banner();
        ui::blank();
        ui::out(&format!(
            "{}Looking up pack versions on Maven…",
            ui::dim("· ")
        ));
        ui::blank();
        let installed = self.installed()?;
        let repositories = self.config.maven_repositories();
        let (versions, release) =
            maven::Client::new(&repositories)?.list_versions(&pin.group, &pin.artifact)?;
        if versions.is_empty() {
            return Err(format!("no versions published for {}:{}", pin.group, pin.artifact).into());
        }
        let cache = self.cache();
        let choices: Vec<VersionChoice> = versions
            .iter()
            .map(|version| {
                let coordinate = pin.coordinate(version);
                match pack::resolve(&coordinate, &repositories, Some(&cache)) {
                    Ok(resolved) => VersionChoice {
                        version: resolved.manifest.version.clone(),
                        minecraft: resolved.manifest.minecraft().to_owned(),
                        pin: coordinate,
                        channel: String::new(),
                        resolved: Some(resolved),
                    },
                    Err(_) => VersionChoice {
                        version: version.clone(),
                        minecraft: String::new(),
                        pin: String::new(),
                        channel: String::new(),
                        resolved: None,
                    },
                }
            })
            .collect();
        let Some(target_version) = self.target_version(&choices, installed.as_ref(), &release)?
        else {
            return Ok(());
        };
        let chosen = choices
            .into_iter()
            .find(|choice| choice.version == target_version);
        let (target, target_pin) = match chosen {
            Some(VersionChoice {
                resolved: Some(resolved),
                pin,
                ..
            }) => (resolved, pin),
            _ => {
                let coordinate = pin.coordinate(&target_version);
                let resolved =
                    pack::resolve(&coordinate, &repositories, Some(&cache)).map_err(|error| {
                        error.context(format_args!("couldn't load pack {coordinate}"))
                    })?;
                (resolved, coordinate)
            }
        };
        self.apply(installed.as_ref(), target, &target_pin)
    }

    fn modrinth(&self, slug: &str) -> Result<()> {
        ui::banner();
        ui::blank();
        ui::out(&format!(
            "{}Looking up pack versions on Modrinth…",
            ui::dim("· ")
        ));
        ui::blank();
        let installed = self.installed()?;
        let (project, mut versions) = modrinth::Client::new()?.mrpack_versions(slug)?;
        // Keep the picker short; the API lists newest first.
        versions.truncate(40);
        let release = versions
            .iter()
            .find(|version| version.version_type.eq_ignore_ascii_case("release"))
            .or(versions.first())
            .map(|version| version.version_number.clone())
            .unwrap_or_default();
        // The picker uses API metadata only, without downloading every pack.
        let pin_slug = [&project.slug, &project.id]
            .into_iter()
            .find(|value| !value.trim().is_empty())
            .map_or(slug, |value| value.as_str());
        let choices: Vec<VersionChoice> = versions
            .iter()
            .map(|version| VersionChoice {
                version: version.version_number.clone(),
                minecraft: version.minecraft().to_owned(),
                pin: modrinth::pin(pin_slug, &version.version_number),
                channel: version.version_type.clone(),
                resolved: None,
            })
            .collect();
        let Some(target_version) = self.target_version(&choices, installed.as_ref(), &release)?
        else {
            return Ok(());
        };
        let target_pin = choices
            .iter()
            .find(|choice| choice.version == target_version)
            .map_or_else(
                || modrinth::pin(pin_slug, &target_version),
                |choice| choice.pin.clone(),
            );
        let target = pack::resolve(&target_pin, &[], Some(&self.cache()))
            .map_err(|error| error.context(format_args!("couldn't load pack {target_pin}")))?;
        self.apply(installed.as_ref(), target, &target_pin)
    }

    /// The version from `-to`, or one the user picks. `None` means cancelled.
    fn target_version(
        &self,
        choices: &[VersionChoice],
        installed: Option<&State>,
        release: &str,
    ) -> Result<Option<String>> {
        if !self.to.is_empty() {
            return Ok(Some(self.to.to_owned()));
        }
        let installed = installed.map_or("", |state| state.pack_version.as_str());
        match pick_version(choices, installed, release)? {
            Some(version) => Ok(Some(version)),
            None => {
                ui::info("Cancelled — no changes made.");
                Ok(None)
            }
        }
    }

    fn apply(
        &self,
        installed: Option<&State>,
        mut target: Resolved,
        target_pin: &str,
    ) -> Result<()> {
        let baseline = installed.map_or("", |state| state.pack_version.as_str());
        let baseline_minecraft = installed.map_or("", |state| state.minecraft.as_str());
        let target_version = target.manifest.version.clone();
        if !baseline.is_empty() && baseline == target_version {
            ui::ok(&format!("Already on v{baseline}."));
            ui::info(&format!(
                "To re-download files for this version, run {}.",
                ui::blue("./pastel refresh")
            ));
            return Ok(());
        }

        ui::blank();
        ui::title("Confirm upgrade");
        let from = match installed {
            Some(state) if !baseline.is_empty() => {
                let name = if state.pack_name.is_empty() {
                    "installed"
                } else {
                    &state.pack_name
                };
                version::pack_line(name, baseline, baseline_minecraft)
            }
            _ => "nothing installed".to_owned(),
        };
        ui::kv("from", &from);
        ui::kv(
            "to",
            &version::pack_line(
                &target.manifest.name,
                &target_version,
                target.manifest.minecraft(),
            ),
        );
        ui::detail(target_pin);
        if !baseline.is_empty() && version::compare(baseline, &target_version).is_gt() {
            ui::warn("This is a downgrade.");
        }
        if self.common.dry_run {
            ui::blank();
            ui::warn("Dry run — nothing was changed.");
            return Ok(());
        }
        if !self.yes
            && !confirm(&format!(
                "Proceed with this upgrade? {} ",
                ui::pink("[y/N]")
            ))?
        {
            ui::info("Cancelled — no changes made.");
            return Ok(());
        }

        let mut config = config::load(self.config.path())?;
        config
            .set_pack(target_pin)
            .map_err(|error| error.context("couldn't update server.pastel pin"))?;
        ui::ok(&format!("Updated pin → {}", ui::blue(target_pin)));
        ui::blank();
        ui::step(&format!(
            "Downloading {} {}…",
            ui::pink(&target.manifest.name),
            ui::blue(&format!("v{target_version}"))
        ));
        target.coordinate = target_pin.to_owned();
        let outcome = apply(self.common, &config, &mut target)
            .map_err(|error| error.context("upgrade failed"))?;
        print_summary(
            &outcome,
            false,
            &target.manifest.name,
            &target_version,
            &next_step_hint(),
        );
        Ok(())
    }
}

/// Lists versions and reads a choice. `None` means the user cancelled.
fn pick_version(
    choices: &[VersionChoice],
    installed: &str,
    release: &str,
) -> Result<Option<String>> {
    ui::title("Pick a pack version");
    if !installed.is_empty() {
        ui::detail(&format!("Currently installed: v{installed}"));
    }
    ui::blank();
    for (index, choice) in choices.iter().enumerate() {
        let mut label = format!(
            "{})  v{}",
            ui::blue(&(index + 1).to_string()),
            choice.version
        );
        if !choice.minecraft.is_empty() {
            label.push_str(&format!(
                "  ·  Minecraft {}  ·  Java {}",
                choice.minecraft,
                jre::require_major(&choice.minecraft)
            ));
        }
        let mut tags = Vec::new();
        if choice.version == release {
            tags.push("latest");
        }
        if !choice.channel.is_empty() && !choice.channel.eq_ignore_ascii_case("release") {
            tags.push(&choice.channel);
        }
        if !installed.is_empty() && choice.version == installed {
            tags.push("installed");
        }
        if !tags.is_empty() {
            label.push_str(&format!("  {}", ui::dim(&format!("({})", tags.join(", ")))));
        }
        ui::out(&format!("  {label}"));
    }
    ui::blank();
    if !io::stdin().is_terminal() {
        return Err(
            "no TTY for version picker — pass -to VERSION (and -yes to skip confirm)".into(),
        );
    }
    ui::out_inline(&format!(
        "{}{}",
        ui::blue("Version number or id"),
        ui::dim(" (q cancels): ")
    ));
    let line = read_line()?;
    let line = line.trim();
    if line.is_empty()
        || ["q", "quit", "cancel"]
            .iter()
            .any(|word| line.eq_ignore_ascii_case(word))
    {
        return Ok(None);
    }
    if let Ok(number) = line.parse::<usize>() {
        return match number.checked_sub(1).and_then(|index| choices.get(index)) {
            Some(choice) => Ok(Some(choice.version.clone())),
            None => Err(format!("pick a number between 1 and {}", choices.len()).into()),
        };
    }
    let wanted = line.strip_prefix('v').unwrap_or(line);
    choices
        .iter()
        .find(|choice| choice.version == wanted)
        .map(|choice| Some(choice.version.clone()))
        .ok_or_else(|| format!("unknown version {wanted:?}").into())
}

pub(crate) fn confirm(prompt: &str) -> Result<bool> {
    if !io::stdin().is_terminal() {
        return Err("no TTY for confirmation — pass -yes to confirm non-interactively".into());
    }
    ui::out_inline(&ui::blue(prompt));
    let answer = read_line()?.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

fn read_line() -> Result<String> {
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line)? == 0 {
        return Err("no input".into());
    }
    Ok(line)
}

fn self_update(args: &[String]) -> Result<()> {
    if !args.is_empty() {
        return Err("self-update does not accept arguments".into());
    }
    ui::banner();
    ui::blank();
    ui::title("Updating Pastel");
    ui::step("Downloading the latest release…");
    let executable =
        std::env::current_exe().map_err(|error| format!("find current executable: {error}"))?;
    let outcome = selfupdate::run(&executable, selfupdate::RELEASES_URL)
        .map_err(|error| error.context("self-update failed"))?;
    ui::big_ok(&format!("Pastel is now {}", outcome.version));
    ui::detail("Restart any running Pastel command to use the new version.");
    Ok(())
}
