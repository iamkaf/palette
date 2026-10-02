//! The live server console: the log as it grows, and a prompt for commands.

use super::{cleanup_server_files, running, send_command, supervisor_pid};
use crate::{Result, state, ui};
use std::fs::File;
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

/// Opens the live console. Ctrl+C or Ctrl+D leaves while the server keeps
/// running, and the console closes by itself if the server stops.
pub fn attach(root: &Path) -> Result<()> {
    if running(root).is_none() {
        return Err(
            "server is not running — start it with ./pastel run, then ./pastel console".into(),
        );
    }
    let log_path = state::console_log_path(root);
    if !log_path.exists() {
        let _ = File::create(&log_path);
    }

    ui::out(&format!(
        "{}{}",
        ui::brand(),
        ui::dim("  ·  server console")
    ));
    ui::blank();
    ui::detail("——— recent log ———");
    let offset = Arc::new(AtomicU64::new(0));
    if let Ok(bytes) = std::fs::read(&log_path) {
        let tail = tail_lines(&bytes, 30);
        if tail.is_empty() {
            ui::detail("(no log output yet)");
        } else {
            let mut stdout = io::stdout();
            let _ = stdout.write_all(tail);
            if !tail.ends_with(b"\n") {
                let _ = writeln!(stdout);
            }
        }
        offset.store(bytes.len() as u64, Ordering::SeqCst);
    }
    ui::blank();

    ui::title("How to use this");
    ui::info(&format!(
        "Type a command and press {} to send it.",
        ui::blue("Enter")
    ));
    ui::info(&format!(
        "Press {} to leave — the server {}.",
        ui::blue("Ctrl+C"),
        ui::bold("keeps running")
    ));
    ui::detail("If the server crashes, this console exits on its own.");
    ui::detail(&format!("Full shutdown: {}", ui::blue("./pastel stop")));
    ui::blank();
    ui::title("Handy commands");
    for (command, description) in [
        ("list", "Who is online"),
        ("say <message>", "Broadcast a chat message"),
        ("op <player>", "Make someone an operator"),
        ("deop <player>", "Remove operator status"),
        ("kick <player>", "Disconnect a player"),
        (
            "whitelist add <player>",
            "Allow someone when whitelist is on",
        ),
        ("gamemode survival <player>", "Set a player's mode"),
        ("tp <player> <x> <y> <z>", "Teleport a player"),
        ("time set day", "Change time (night, noon, …)"),
        ("weather clear", "Change weather (rain, thunder)"),
        ("save-all", "Force a world save"),
        ("stop", "Shut down the server"),
    ] {
        ui::out(&format!(
            "  {}  {description}",
            ui::blue(&format!("{command:<28}"))
        ));
    }
    ui::blank();
    ui::out(&ui::dim("——— live (type below) ———"));

    let stop = Arc::new(AtomicBool::new(false));
    let follower = {
        let (path, offset, stop) = (log_path.clone(), Arc::clone(&offset), Arc::clone(&stop));
        thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(150));
                copy_new_output(&path, &offset);
            }
        })
    };
    let result = console_loop(root, &log_path, &offset);
    stop.store(true, Ordering::SeqCst);
    let _ = follower.join();
    result
}

enum Input {
    Line(String),
    End,
    Failed(io::Error),
}

fn console_loop(root: &Path, log_path: &Path, offset: &AtomicU64) -> Result<()> {
    let (sender, input) = mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let event = match line {
                Ok(line) => Input::Line(line),
                Err(error) => Input::Failed(error),
            };
            let failed = matches!(event, Input::Failed(_));
            if sender.send(event).is_err() || failed {
                return;
            }
        }
        let _ = sender.send(Input::End);
    });

    let leave_because_dead = || {
        copy_new_output(log_path, offset);
        ui::blank();
        ui::warn("The server is no longer running — left the console.");
        ui::detail(&format!(
            "If it crashed, see {}",
            ui::blue("logs/latest.log")
        ));
        cleanup_after_death(root);
    };
    let mut next_check = Instant::now() + Duration::from_millis(400);
    loop {
        let wait = next_check.saturating_duration_since(Instant::now());
        match input.recv_timeout(wait) {
            Err(RecvTimeoutError::Timeout) => {
                next_check = Instant::now() + Duration::from_millis(400);
                if running(root).is_none() {
                    leave_because_dead();
                    return Ok(());
                }
            }
            Ok(Input::Failed(error)) => return Err(error.into()),
            Ok(Input::End) | Err(RecvTimeoutError::Disconnected) => {
                ui::blank();
                if running(root).is_some() {
                    ui::ok("Left the console. Server is still running.");
                    ui::detail("Use ./pastel stop when you want to shut it down.");
                } else {
                    ui::warn("Left the console. Server is not running.");
                }
                return Ok(());
            }
            Ok(Input::Line(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                ui::out(&format!("\r{}{line}\x1b[K", ui::pink("» ")));
                if let Err(error) = send_command(root, line) {
                    ui::warn(&error.to_string());
                    if running(root).is_none() {
                        leave_because_dead();
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// The supervisor may be between crashed Java processes. It owns restarts and
/// cleanup, so the console only cleans up when the supervisor is gone too.
fn cleanup_after_death(root: &Path) {
    if supervisor_pid(root).is_none() {
        cleanup_server_files(root);
    }
}

/// Prints log output written since `offset`, starting over if the log shrank.
fn copy_new_output(path: &Path, offset: &AtomicU64) {
    let Ok(mut file) = File::open(path) else {
        return;
    };
    let Ok(size) = file.metadata().map(|metadata| metadata.len()) else {
        return;
    };
    let mut current = offset.load(Ordering::SeqCst);
    if size < current {
        current = 0;
    }
    if size == current || file.seek(SeekFrom::Start(current)).is_err() {
        return;
    }
    let mut bytes = Vec::new();
    if file.take(size - current).read_to_end(&mut bytes).is_ok() {
        let mut stdout = io::stdout();
        let _ = stdout.write_all(&bytes);
        let _ = stdout.flush();
        offset.store(current + bytes.len() as u64, Ordering::SeqCst);
    }
}

/// The last `count` lines of `bytes`.
fn tail_lines(bytes: &[u8], count: usize) -> &[u8] {
    let body = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let start = body
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, byte)| **byte == b'\n')
        .nth(count - 1)
        .map_or(0, |(index, _)| index + 1);
    &bytes[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_supervisor_keeps_its_tracking_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(state::dir(root.path())).unwrap();
        std::fs::write(state::console_in_path(root.path()), "endpoint\n").unwrap();
        // This test binary, sleeping, with a command line that looks like this
        // folder's supervisor.
        let mut helper = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime::attach::tests::fake_helper_process",
                "__supervise",
            ])
            .arg(root.path())
            .env("PASTEL_FAKE_HELPER", "1")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        thread::sleep(Duration::from_millis(200));
        super::super::write_pid(&state::supervisor_pid_path(root.path()), helper.id()).unwrap();

        cleanup_after_death(root.path());
        assert!(state::supervisor_pid_path(root.path()).exists());
        assert!(state::console_in_path(root.path()).exists());

        helper.kill().unwrap();
        helper.wait().unwrap();
        cleanup_after_death(root.path());
        assert!(!state::console_in_path(root.path()).exists());
    }

    #[test]
    fn fake_helper_process() {
        if std::env::var_os("PASTEL_FAKE_HELPER").is_some() {
            thread::sleep(Duration::from_secs(60));
        }
    }

    #[test]
    fn tails_the_last_lines() {
        assert_eq!(tail_lines(b"a\nb\nc\n", 2), b"b\nc\n");
        assert_eq!(tail_lines(b"a\nb", 5), b"a\nb");
    }
}
