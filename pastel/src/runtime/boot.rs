//! Watching a background server boot, and explaining it when it fails.

use super::{crash, exited, procs};
use crate::{Error, Result, paths, state, ui};
use regex::Regex;
use std::fs::File;
use std::io::{self, IsTerminal, Read, Seek, SeekFrom};
use std::path::Path;
use std::process::ExitStatus;
use std::sync::LazyLock;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// `[04:19:30] [Server thread/INFO]: Done (0.652s)! For help, type "help"`
static SERVER_DONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)Done\s*\([^)]*\)!\s*For help").expect("valid pattern"));

/// Hard startup failures from Fabric, Forge, and the game itself.
static SERVER_FAILED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)Failed to start the minecraft server|Could not execute entrypoint|Exception in server tick loop|Minecraft has crashed|#@!@# Game crashed|A fatal exception has occurred|Unable to begin loading",
    )
    .expect("valid pattern")
});

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

type Supervisor = Receiver<io::Result<ExitStatus>>;

struct Logs<'a> {
    root: &'a Path,
    pid: u32,
    console: std::path::PathBuf,
    latest: std::path::PathBuf,
    console_start: u64,
    latest_start: u64,
}

impl Logs<'_> {
    fn failed(&self) -> bool {
        log_matches_since(&self.console, self.console_start, &SERVER_FAILED)
            || log_matches_since(&self.latest, self.latest_start, &SERVER_FAILED)
    }

    fn done(&self) -> bool {
        log_contains_done_since(&self.console, self.console_start)
            || log_contains_done_since(&self.latest, self.latest_start)
    }

    /// Whether the PID is alive and still this server's Minecraft, so a reused
    /// PID never looks like a running server.
    fn server_alive(&self) -> bool {
        let Some(info) = procs::process(self.pid) else {
            return false;
        };
        if info.args.is_empty() && info.cwd.is_empty() {
            return true;
        }
        let root = paths::absolute(self.root).unwrap_or_else(|_| self.root.to_path_buf());
        procs::is_minecraft_server_process(&info, &procs::root_spellings(&root))
    }

    /// Prints a plain-language failure and returns an already-explained error.
    fn early_exit(&self) -> Error {
        let mut log = read_tail(&self.latest, 64 * 1024);
        if log.is_empty() {
            log = read_tail(&self.console, 64 * 1024);
        }
        let summary = crash::summarize(&log);

        // The same weight as "Your server is running", so it can't be missed.
        ui::big_fail("The server couldn't start");
        ui::info("Minecraft quit before it was ready to play.");
        ui::blank();
        ui::warn(summary.headline);
        for line in &summary.details {
            ui::detail(line);
        }
        if !summary.mods.is_empty() {
            ui::blank();
            ui::title("Mods that look involved");
            for name in &summary.mods {
                ui::out(&format!("  {}{name}", ui::pink("· ")));
            }
        }
        ui::blank();
        ui::title("What you can try");
        ui::step(&format!(
            "Remove the problem jar(s) from the {} folder",
            ui::blue("mods/")
        ));
        ui::step(&format!(
            "Or reinstall the pack cleanly:  {}",
            ui::blue("./pastel install … -yes")
        ));
        ui::detail(&format!(
            "While debugging: set {} in server.pastel so run doesn't put jars back",
            ui::blue("sync_on_run = false")
        ));
        ui::blank();
        ui::detail(&format!(
            "Full technical log: {}",
            ui::blue("logs/latest.log")
        ));
        Error::explained("server couldn't start")
    }

    /// Many bad mods explode in the second after Done, so watch briefly.
    fn confirm_still_alive(&self, supervisor: &Supervisor) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline {
            if exited(supervisor) {
                return Err(self.early_exit());
            }
            if log_matches_since(&self.console, 0, &SERVER_FAILED)
                || log_matches_since(&self.latest, 0, &SERVER_FAILED)
            {
                let _ = supervisor.recv_timeout(Duration::from_millis(500));
                return Err(self.early_exit());
            }
            if !self.server_alive() {
                return Err(self.early_exit());
            }
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }
}

/// Shows a live boot status until Minecraft logs that it's ready, the user
/// presses Enter (the server keeps booting), or the server fails.
pub fn wait_for_boot(root: &Path, pid: u32, supervisor: &Supervisor) -> Result<()> {
    let console = state::console_log_path(root);
    let latest = root.join("logs").join("latest.log");
    let logs = Logs {
        root,
        pid,
        console_start: log_size(&console),
        // Often 0 until the logger starts.
        latest_start: log_size(&latest),
        console,
        latest,
    };
    if !io::stdin().is_terminal() {
        return wait_headless(&logs, supervisor, Duration::from_secs(10 * 60));
    }

    let (enter_sender, enter) = mpsc::channel();
    thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buffer = [0; 64];
        while let Ok(read) = stdin.read(&mut buffer) {
            if read == 0 {
                return;
            }
            if buffer[..read]
                .iter()
                .any(|byte| matches!(byte, b'\n' | b'\r'))
            {
                let _ = enter_sender.send(());
                return;
            }
        }
    });

    ui::blank();
    ui::step("Starting Minecraft…");
    ui::out(&format!(
        "  {}{}{}",
        ui::dim("Press "),
        ui::blue("Enter"),
        ui::dim(" to finish anytime — the server keeps booting.")
    ));
    ui::blank();

    let mut status = StatusLine::default();
    let mut frame = 0;
    loop {
        thread::sleep(Duration::from_millis(100));
        if exited(supervisor) {
            status.clear();
            return Err(logs.early_exit());
        }
        if enter.try_recv().is_ok() {
            status.clear();
            // If it already died, report the crash instead of "continuing".
            if exited(supervisor) || !logs.server_alive() {
                return Err(logs.early_exit());
            }
            ui::detail("Continuing in the background…");
            return Ok(());
        }
        // Fatal log lines can appear before the process is reaped.
        if logs.failed() {
            let _ = supervisor.recv_timeout(Duration::from_millis(800));
            status.clear();
            return Err(logs.early_exit());
        }
        if !logs.server_alive() {
            let _ = supervisor.recv_timeout(Duration::from_millis(300));
            status.clear();
            return Err(logs.early_exit());
        }
        // Only a Done written during this boot counts.
        if logs.done() {
            let confirmed = logs.confirm_still_alive(supervisor);
            status.clear();
            confirmed?;
            ui::ok("Minecraft is ready");
            return Ok(());
        }
        let mut last = last_line_since(&logs.latest, logs.latest_start);
        if last.is_empty() {
            last = last_line_since(&logs.console, logs.console_start);
        }
        let spin = ui::blue(SPINNER[frame % SPINNER.len()]);
        frame += 1;
        let text = if last.is_empty() {
            ui::dim("waiting for log…")
        } else {
            ui::dim(&truncate_chars(&last, 96))
        };
        status.print(&format!("{spin} {text}"));
    }
}

fn wait_headless(logs: &Logs<'_>, supervisor: &Supervisor, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if exited(supervisor) {
            return Err(logs.early_exit());
        }
        if logs.failed() {
            let _ = supervisor.recv_timeout(Duration::from_millis(800));
            return Err(logs.early_exit());
        }
        if !logs.server_alive() {
            return Err(logs.early_exit());
        }
        if logs.done() {
            return logs.confirm_still_alive(supervisor);
        }
        thread::sleep(Duration::from_millis(200));
    }
    if !logs.server_alive() {
        return Err(logs.early_exit());
    }
    Ok(())
}

/// A single terminal line that redraws in place.
#[derive(Default)]
struct StatusLine {
    width: usize,
}

impl StatusLine {
    fn print(&mut self, line: &str) {
        let visible = visible_len(line);
        let pad = self.width.saturating_sub(visible);
        ui::out_inline(&format!("\r{line}{}", " ".repeat(pad)));
        self.width = visible;
    }

    fn clear(&self) {
        let width = if self.width < 1 { 80 } else { self.width };
        ui::out_inline(&format!("\r{}\r", " ".repeat(width)));
    }
}

/// Length without ANSI escape sequences.
fn visible_len(text: &str) -> usize {
    let mut count = 0;
    let mut escape = false;
    for c in text.chars() {
        if c == '\x1b' {
            escape = true;
        } else if escape {
            escape = !c.is_ascii_alphabetic();
        } else {
            count += 1;
        }
    }
    count
}

fn truncate_chars(text: &str, max: usize) -> String {
    if max <= 1 || text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max - 1).collect();
    out.push('…');
    out
}

pub fn log_size(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

/// The log text written after byte `start`.
fn read_since(path: &Path, start: u64) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn read_tail(path: &Path, max: u64) -> String {
    read_since(path, log_size(path).saturating_sub(max))
}

pub fn log_contains_done_since(path: &Path, start: u64) -> bool {
    log_matches_since(path, start, &SERVER_DONE)
}

fn log_matches_since(path: &Path, start: u64, pattern: &Regex) -> bool {
    pattern.is_match(&read_since(path, start))
}

fn last_line_since(path: &Path, start: u64) -> String {
    read_since(path, start)
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn recognizes_ready_and_failed_servers() {
        for line in [
            r#"[04:19:30] [Server thread/INFO]: Done (0.652s)! For help, type "help""#,
            r#"Done (1.0s)! For help, type "help""#,
        ] {
            assert!(SERVER_DONE.is_match(line), "{line}");
        }
        for line in [
            "[04:45:16] [main/ERROR]: Failed to start the minecraft server",
            "java.lang.RuntimeException: Could not execute entrypoint stage 'main'",
        ] {
            assert!(SERVER_FAILED.is_match(line), "{line}");
        }
        assert!(!SERVER_FAILED.is_match("Preparing spawn area: 80%"));
    }

    #[test]
    fn only_this_boots_done_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("console.log");
        std::fs::write(&path, "[old] Done (0.1s)! For help, type \"help\"\n").unwrap();
        let start = log_size(&path);
        assert!(!log_contains_done_since(&path, start));
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            "[04:21:28] [Server thread/INFO]: Done (0.654s)! For help, type \"help\""
        )
        .unwrap();
        writeln!(
            file,
            "[04:21:28] ThreadedAnvilChunkStorage: All dimensions are saved"
        )
        .unwrap();
        assert!(log_contains_done_since(&path, start));
        assert!(last_line_since(&path, start).contains("dimensions are saved"));
    }

    #[test]
    fn truncates_by_characters() {
        assert_eq!(
            truncate_chars("abcdefghijklmnopqrstuvwxyz", 10),
            "abcdefghi…"
        );
    }
}
