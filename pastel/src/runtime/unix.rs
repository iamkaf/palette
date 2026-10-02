//! Background servers on macOS and Linux: a supervisor in its own session and
//! a console FIFO kept open by a small hold process.

use super::{
    finish_background_start, spawn_helper, stop_hold, supervise_loop, watch_exit, write_pid,
};
use crate::{Context, Result, state};
use nix::sys::signal::{Signal, kill as send_signal, killpg};
use nix::sys::stat::Mode;
use nix::unistd::{Pid, mkfifo, setsid};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

pub fn terminate(pid: u32) -> Result<()> {
    signal(pid, Signal::SIGTERM)
}

pub fn kill(pid: u32) {
    let _ = signal(pid, Signal::SIGKILL);
}

fn signal(pid: u32, signal: Signal) -> Result<()> {
    let pid = i32::try_from(pid).map_err(|_| format!("invalid pid {pid}"))?;
    Ok(send_signal(Pid::from_raw(pid), signal)?)
}

/// The supervisor leads its own session and Java inherits its process group.
/// Signaling the group means an aborted startup can't leave Java orphaned.
pub fn terminate_supervisor(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        let _ = killpg(Pid::from_raw(pid), Signal::SIGTERM);
    }
}

pub fn start_background(
    root: &Path,
    java: &Path,
    args: &[String],
    auto_restart: bool,
) -> Result<()> {
    let fifo = state::console_in_path(root);
    let log_path = state::console_log_path(root);
    let _ = fs::remove_file(&fifo);
    mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).context("console fifo")?;

    let hold = spawn_helper(&["__hold-fifo".as_ref(), fifo.as_os_str()], |_| {})
        .context("console hold")?;
    // Without its PID file nothing could stop the hold process later.
    if let Err(error) = write_pid(&state::hold_pid_path(root), hold.id()) {
        kill(hold.id());
        return Err(crate::Error::from(error).context("console hold"));
    }
    thread::sleep(Duration::from_millis(100));
    if let Err(error) = File::create(&log_path) {
        stop_hold(root);
        return Err(error.into());
    }

    let mut supervisor_args = vec![
        "__supervise".as_ref(),
        root.as_os_str(),
        if auto_restart { "true" } else { "false" }.as_ref(),
        "--".as_ref(),
        java.as_os_str(),
    ];
    supervisor_args.extend(args.iter().map(std::ffi::OsStr::new));
    let supervisor = match spawn_helper(&supervisor_args, |_| {}) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            stop_hold(root);
            return Err(format!("couldn't start server supervisor: {error}").into());
        }
    };
    if let Err(error) = write_pid(&state::supervisor_pid_path(root), supervisor.id()) {
        kill(supervisor.id());
        stop_hold(root);
        return Err(error.into());
    }
    finish_background_start(root, watch_exit(supervisor))
}

/// Owns the background Java process. A server that reached Minecraft's ready
/// message restarts after a non-zero exit; clean shutdowns and startup
/// failures stay stopped.
pub fn supervise(root: &Path, java: &str, args: &[String], auto_restart: bool) -> Result<()> {
    // Leave the launching terminal's session, so closing it can't stop the server.
    let _ = setsid();
    let log_path = state::console_log_path(root);
    supervise_loop(
        root,
        auto_restart,
        || {
            // The hold process keeps a writer open, so this doesn't block.
            let stdin =
                File::open(state::console_in_path(root)).context("open console fifo for server")?;
            let log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)?;
            let child = Command::new(java)
                .args(args)
                .current_dir(root)
                .stdin(Stdio::from(stdin))
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log.try_clone()?))
                .spawn()
                .map_err(|error| format!("couldn't start Java ({java}): {error}"))?;
            Ok((child, log))
        },
        || {},
    )
}

/// Keeps the console FIFO open for writing so the server never reads EOF
/// between commands. Runs until it's killed.
pub fn hold_fifo(path: &Path) -> Result<()> {
    let _ = setsid();
    let _fifo = OpenOptions::new().read(true).write(true).open(path)?;
    loop {
        thread::park();
    }
}

/// Sends one console line to the background server.
pub fn send_command(root: &Path, line: &str) -> Result<()> {
    let fifo = state::console_in_path(root);
    if !fifo.exists() {
        return Err("console not available (is the server running via ./pastel run?)".into());
    }
    // Non-blocking, so a console with no reader fails instead of hanging.
    let mut console = OpenOptions::new()
        .write(true)
        .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
        .open(&fifo)
        .context("open console")?;
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }
    console.write_all(format!("{line}\n").as_bytes())?;
    Ok(())
}
