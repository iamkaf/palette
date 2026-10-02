//! Friendly, pastel-colored terminal output.

use std::io::{self, IsTerminal, Write};
use std::sync::OnceLock;

static COLOR: OnceLock<bool> = OnceLock::new();

fn enabled() -> bool {
    *COLOR.get_or_init(|| {
        let color = detect_color();
        if color {
            enable_windows_ansi();
        }
        color
    })
}

fn detect_color() -> bool {
    if std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()) {
        return false;
    }
    if std::env::var_os("PASTEL_FORCE_COLOR").is_some_and(|value| !value.is_empty()) {
        return true;
    }
    // Only color interactive terminals by default.
    io::stdout().is_terminal() || io::stderr().is_terminal()
}

/// Turns on VT processing so RGB colors work in conhost and Windows Terminal.
#[cfg(windows)]
fn enable_windows_ansi() {
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE,
        STD_OUTPUT_HANDLE, SetConsoleMode,
    };
    for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: the standard handles belong to this process, and the mode is
        // read into and written from a local integer.
        unsafe {
            let handle = GetStdHandle(which);
            let mut mode = 0;
            if GetConsoleMode(handle, &mut mode) != 0 {
                SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
            }
        }
    }
}

#[cfg(not(windows))]
fn enable_windows_ansi() {}

fn rgb(r: u8, g: u8, b: u8, text: &str) -> String {
    if !enabled() || text.is_empty() {
        return text.to_owned();
    }
    format!("\x1b[38;2;{r};{g};{b}m{text}\x1b[0m")
}

pub fn pink(text: &str) -> String {
    rgb(248, 165, 194, text)
}

pub fn blue(text: &str) -> String {
    rgb(162, 210, 255, text)
}

fn lavender(text: &str) -> String {
    rgb(198, 180, 232, text)
}

/// Success accent.
pub fn mint(text: &str) -> String {
    rgb(167, 227, 194, text)
}

/// Warning accent.
fn peach(text: &str) -> String {
    rgb(255, 179, 148, text)
}

/// Error accent.
fn coral(text: &str) -> String {
    rgb(255, 138, 148, text)
}

/// Muted secondary text.
pub fn dim(text: &str) -> String {
    rgb(160, 160, 175, text)
}

pub fn bold(text: &str) -> String {
    if !enabled() {
        return text.to_owned();
    }
    format!("\x1b[1m{text}\x1b[0m")
}

/// Colors a loader name in its accent.
pub fn loader(name: &str) -> String {
    match name.trim().to_lowercase().as_str() {
        "fabric" => rgb(219, 176, 255, name),
        "neoforge" => rgb(241, 120, 80, name),
        "forge" => rgb(180, 140, 100, name),
        "quilt" => lavender(name),
        "vanilla" => mint(name),
        _ => blue(name),
    }
}

/// The product name, always capitalized, in alternating pink and blue letters.
pub fn brand() -> String {
    if !enabled() {
        return "Pastel".to_owned();
    }
    let mut out = String::new();
    for (index, letter) in "Pastel".chars().enumerate() {
        // Bold and color per letter, so a reset never wipes the bold.
        let (r, g, b) = if index % 2 == 0 {
            (248, 165, 194)
        } else {
            (162, 210, 255)
        };
        out.push_str(&format!("\x1b[1;38;2;{r};{g};{b}m{letter}"));
    }
    out.push_str("\x1b[0m");
    out
}

/// Prints a line to stdout, ignoring a closed pipe.
pub fn out(line: &str) {
    let _ = writeln!(io::stdout(), "{line}");
}

/// Prints without a newline and flushes, for prompts and status lines.
pub fn out_inline(text: &str) {
    let mut stdout = io::stdout();
    let _ = write!(stdout, "{text}");
    let _ = stdout.flush();
}

fn err(line: &str) {
    let _ = writeln!(io::stderr(), "{line}");
}

const LOGO: [&str; 4] = [
    "▗▄▄▖  ▗▄▖  ▗▄▄▖▗▄▄▄▖▗▄▄▄▖▗▖   ",
    "▐▌ ▐▌▐▌ ▐▌▐▌     █  ▐▌   ▐▌   ",
    "▐▛▀▘ ▐▛▀▜▌ ▝▀▚▖  █  ▐▛▀▀▘▐▌   ",
    "▐▌   ▐▌ ▐▌▗▄▄▞▘  █  ▐▙▄▄▖▐▙▄▄▖",
];

/// Prints the PASTEL wordmark and tagline.
pub fn banner() {
    for (index, line) in LOGO.iter().enumerate() {
        out(&if index % 2 == 0 {
            pink(line)
        } else {
            blue(line)
        });
    }
    out(&dim("  your Minecraft server helper"));
}

pub fn title(text: &str) {
    out(&format!("{}{}", pink("◆ "), bold(&blue(text))));
}

pub fn step(text: &str) {
    out(&format!("{}{text}", blue("→ ")));
}

pub fn ok(text: &str) {
    out(&format!("{}{text}", mint("✓ ")));
}

pub fn warn(text: &str) {
    out(&format!("{}{text}", peach("! ")));
}

pub fn info(text: &str) {
    out(&format!("{}{text}", dim("· ")));
}

pub fn detail(text: &str) {
    out(&format!("  {}", dim(text)));
}

pub fn kv(key: &str, value: &str) {
    out(&format!("  {}  {value}", pink(&format!("{key:<12}"))));
}

pub fn blank() {
    out("");
}

/// A high-visibility success block, so the moment isn't lost in noise.
pub fn big_ok(text: &str) {
    big_block(text, "✓", mint);
}

/// A high-visibility failure block with the same weight as [`big_ok`].
pub fn big_fail(text: &str) {
    big_block(text, "✗", coral);
}

fn big_block(text: &str, mark: &str, color: fn(&str) -> String) {
    blank();
    let bar = "─".repeat((text.len() + 6).clamp(36, 52));
    out(&color(&format!("╭{bar}")));
    out(&format!(
        "{}{}",
        color(&format!("│  {mark}  ")),
        bold(&color(text))
    ));
    out(&color(&format!("╰{bar}")));
    blank();
}

/// A short end-of-command summary.
pub fn summary_box(lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let bar = "─".repeat(36);
    out(&blue(&format!("╭{bar}")));
    for line in lines {
        out(&format!("{}{line}", blue("│ ")));
    }
    out(&blue(&format!("╰{bar}")));
}

/// A friendly multi-line error for people who don't read stack traces.
pub fn error_message(title: &str, detail: &str, tips: &[&str]) {
    err(&format!("{}{title}", coral("✗ ")));
    if !detail.is_empty() {
        err(&format!("  {}", dim(detail)));
    }
    for tip in tips {
        err(&format!("  {}{tip}", blue("tip: ")));
    }
}

pub fn help_block() {
    banner();
    blank();
    let brand = brand();
    let lines = [
        pink("What is this?"),
        format!("  {brand} keeps your Minecraft server mods up to date"),
        "  and can start or stop the server for you.".to_owned(),
        String::new(),
        pink("Start here"),
        format!(
            "  {}  Get a modpack (slug, Modrinth URL, or .mrpack link)",
            blue("./pastel install <pack>")
        ),
        format!(
            "  {}                 Home — status + if a new pack is available",
            blue("./pastel")
        ),
        format!(
            "  {}             Refresh pin, start server in the background",
            blue("./pastel run")
        ),
        format!(
            "  {}         Live logs + type commands",
            blue("./pastel console")
        ),
        format!(
            "  {}            Stop the server safely",
            blue("./pastel stop")
        ),
        format!(
            "  {}          Kill a specific process (lost folder)",
            dim("  stop -pid N")
        ),
        format!(
            "  {}       Kill servers whose folder was deleted",
            dim("  stop -orphans")
        ),
        String::new(),
        pink("Pack care"),
        format!(
            "  {}         Re-download files for your current pin",
            blue("./pastel refresh")
        ),
        format!(
            "  {}          Pick a pack version and upgrade (Maven or Modrinth)",
            blue("./pastel update")
        ),
        format!("  {} upgrade", dim("  alias:")),
        format!(
            "  {}          Detailed status for this folder",
            blue("./pastel status")
        ),
        String::new(),
        pink("Install examples"),
        format!("  {}", dim("./pastel install aristea")),
        format!(
            "  {}",
            dim("./pastel install https://modrinth.com/modpack/aristea")
        ),
        format!("  {}", dim("./pastel install https://…/pack.mrpack")),
        format!(
            "  {}",
            dim(
                "./pastel install com.example.modpacks:example-pack:1.2.0 -repo https://maven.example.com"
            )
        ),
        String::new(),
        pink("More"),
        format!(
            "  {} install→get/add · console→attach/logs/terminal",
            dim("aliases:")
        ),
        format!(
            "  {}          Foreground mode (debug)",
            blue("./pastel run -f")
        ),
        format!(
            "  {}         Show {brand} version",
            blue("./pastel version")
        ),
        format!(
            "  {}     Install the latest verified {brand} release",
            blue("./pastel self-update")
        ),
        String::new(),
        pink("Optional flags"),
        format!("  {}       Memory for install", dim("-memory 4G")),
        format!(
            "  {}        Maven host for group:artifact:version pins",
            dim("-repo URL")
        ),
        format!("  {}             Skip confirmations (scripts)", dim("-yes")),
        format!(
            "  {}     Where your server.pastel file is",
            dim("-config path")
        ),
        format!(
            "  {}               Extra detail (for troubleshooting)",
            dim("-v")
        ),
        String::new(),
        format!(
            "{}{brand}{}",
            dim("Setup: drop "),
            dim(" in a folder → ./pastel install <pack> → ./pastel run")
        ),
        String::new(),
        dim("By running your server with Pastel you are indicating your agreement"),
        dim("to Mojang's EULA (https://aka.ms/MinecraftEULA)."),
    ];
    for line in lines {
        err(&line);
    }
}

/// Human-friendly sync progress. Non-verbose mode only shows changes.
pub struct SyncReport {
    verbose: bool,
    pruned: Vec<String>,
    would_prune: Vec<String>,
}

impl SyncReport {
    pub fn new(verbose: bool) -> Self {
        Self {
            verbose,
            pruned: Vec::new(),
            would_prune: Vec::new(),
        }
    }

    pub fn unchanged(&mut self, path: &str) {
        if self.verbose {
            info(&format!("already good  {}", dim(short_name(path))));
        }
    }

    pub fn download(&mut self, path: &str) {
        ok(&format!("downloaded    {}", blue(short_name(path))));
    }

    pub fn would_download(&mut self, path: &str) {
        step(&format!("would download  {}", blue(short_name(path))));
    }

    pub fn would_update(&mut self, path: &str) {
        step(&format!("would update    {}", blue(short_name(path))));
    }

    pub fn prune(&mut self, path: &str) {
        self.pruned.push(path.to_owned());
    }

    pub fn would_prune(&mut self, path: &str) {
        self.would_prune.push(path.to_owned());
    }

    /// Prints buffered prune lines, compressed when there are many.
    pub fn flush(&mut self) {
        print_prune_list(&std::mem::take(&mut self.pruned), false);
        print_prune_list(&std::mem::take(&mut self.would_prune), true);
    }
}

fn print_prune_list(list: &[String], dry: bool) {
    const MAX_LINES: usize = 5;
    if list.len() > MAX_LINES {
        if dry {
            step(&format!("would remove {} extra files", list.len()));
        } else {
            warn(&format!("removed {} extra mods/files", list.len()));
        }
        detail(&format!(
            "{}, {}, …",
            short_name(&list[0]),
            short_name(&list[1])
        ));
        return;
    }
    for path in list {
        if dry {
            step(&format!("would remove    {}", short_name(path)));
        } else {
            warn(&format!("removed extra  {}", short_name(path)));
        }
    }
}

fn short_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
