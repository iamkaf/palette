//! The background supervisor, run through the real binary with a fake server
//! that prints Minecraft's ready line and stops when told to.

use pastel::runtime::{self, StopTarget};
use pastel::state;
use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const DONE: &str = r#"[00:00:00] [Server thread/INFO]: Done (0.1s)! For help, type "help""#;

enum Server {
    /// Ready, then exits cleanly on `stop`.
    Wait,
    /// Ready, then crashes every time.
    Crash,
    /// Crashes after its first boot, then behaves like `Wait`.
    CrashOnce,
}

/// A fake server command line. The trailing words make it look like a
/// dedicated server to Pastel's process checks.
#[cfg(unix)]
fn server_command(server: Server) -> Vec<String> {
    let wait = format!("echo '{DONE}'; while read line; do [ \"$line\" = stop ] && exit 0; done");
    let crash = format!("echo '{DONE}'; exit 7");
    let script = match server {
        Server::Wait => wait,
        Server::Crash => crash,
        Server::CrashOnce => {
            format!("if [ -f crashed ]; then {wait}; else touch crashed; {crash}; fi")
        }
    };
    ["sh", "-c", &script, "java", "-jar", "server.jar", "nogui"]
        .map(String::from)
        .to_vec()
}

#[cfg(windows)]
fn server_command(server: Server) -> Vec<String> {
    let wait = format!(
        "Write-Output '{DONE}'; $r = New-Object System.IO.StreamReader([Console]::OpenStandardInput()); while (($line = $r.ReadLine()) -ne $null) {{ if ($line -eq 'stop') {{ break }} }}"
    );
    let crash = format!("Write-Output '{DONE}'; exit 7");
    let script = match server {
        Server::Wait => wait,
        Server::Crash => crash,
        Server::CrashOnce => format!(
            "if (Test-Path crashed) {{ {wait} }} else {{ New-Item crashed | Out-Null; {crash} }}"
        ),
    };
    [
        "powershell",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &script,
    ]
    .map(String::from)
    .to_vec()
}

struct Background {
    root: tempfile::TempDir,
    supervisor: Child,
    hold: Option<Child>,
}

impl Drop for Background {
    fn drop(&mut self) {
        let _ = self.supervisor.kill();
        let _ = self.supervisor.wait();
        if let Some(hold) = &mut self.hold {
            let _ = hold.kill();
            let _ = hold.wait();
        }
    }
}

/// Starts the supervisor the way `pastel run` does.
fn start(server: Server) -> Background {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(state::dir(root.path())).unwrap();
    fs::create_dir_all(root.path().join("logs")).unwrap();
    let hold = start_console(root.path());
    fs::write(state::console_log_path(root.path()), "").unwrap();
    let supervisor = Command::new(env!("CARGO_BIN_EXE_pastel"))
        .args(["__supervise"])
        .arg(root.path())
        .args(["true", "--"])
        .args(server_command(server))
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    if cfg!(unix) {
        fs::write(
            state::supervisor_pid_path(root.path()),
            format!("{}\n", supervisor.id()),
        )
        .unwrap();
    }
    Background {
        root,
        supervisor,
        hold,
    }
}

/// A FIFO and the process that keeps it open.
#[cfg(unix)]
fn start_console(root: &Path) -> Option<Child> {
    let fifo = state::console_in_path(root);
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let hold = Command::new(env!("CARGO_BIN_EXE_pastel"))
        .arg("__hold-fifo")
        .arg(&fifo)
        .spawn()
        .unwrap();
    fs::write(state::hold_pid_path(root), format!("{}\n", hold.id())).unwrap();
    Some(hold)
}

#[cfg(windows)]
fn start_console(_: &Path) -> Option<Child> {
    None
}

impl Background {
    fn path(&self) -> &Path {
        self.root.path()
    }

    fn log(&self) -> String {
        fs::read_to_string(state::console_log_path(self.path())).unwrap_or_default()
    }

    fn launches(&self) -> usize {
        self.log().matches("Done (0.1s)!").count()
    }

    fn wait_for(&self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if done(self) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for {what}; console log:\n{}", self.log());
    }

    /// Waits until the console accepts a command, then sends it.
    fn send(&self, line: &str) {
        self.wait_for("the console", |background| {
            fs::metadata(state::pid_path(background.path())).is_ok()
                && runtime::send_command(background.path(), line).is_ok()
        });
    }

    fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(status) = self.supervisor.try_wait().unwrap() {
                return status;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("the supervisor kept running; console log:\n{}", self.log());
    }
}

#[test]
fn console_stop_shuts_the_server_down_cleanly() {
    let mut background = start(Server::Wait);
    background.wait_for("the ready line", |background| background.launches() == 1);
    background.send("stop");
    assert!(background.wait_for_exit().success());
    assert!(!state::pid_path(background.path()).exists());
    assert!(!state::console_in_path(background.path()).exists());
    assert_eq!(background.launches(), 1);
}

#[test]
fn a_ready_server_that_crashes_restarts() {
    let mut background = start(Server::CrashOnce);
    background.wait_for("the restart", |background| background.launches() == 2);
    assert!(
        background
            .log()
            .contains("[Pastel] Server crashed; restarting in 5 seconds…")
    );
    background.send("stop");
    assert!(background.wait_for_exit().success());
    assert_eq!(background.launches(), 2);
}

#[test]
fn stop_during_the_restart_delay_stops_the_supervisor() {
    let mut background = start(Server::Crash);
    background.wait_for("the restart delay", |background| {
        background.log().contains("restarting in 5 seconds")
            && !state::pid_path(background.path()).exists()
    });
    runtime::stop(background.path(), StopTarget::Server { force: false }).unwrap();
    background.wait_for_exit();
    thread::sleep(Duration::from_millis(5500));
    assert_eq!(background.launches(), 1, "the server restarted after stop");
}
