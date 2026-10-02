//! Supervises the Minecraft dedicated server process.

mod attach;
mod boot;
mod crash;
pub mod procs;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

pub use attach::attach;
pub use platform::{hold_fifo, send_command, supervise};

use crate::pack::Manifest;
use crate::{Error, Result, paths, state, ui};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

/// How long a server may take to save and exit before Pastel escalates.
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(30);

pub struct Options<'a> {
    pub root: &'a Path,
    pub java: &'a Path,
    pub xmx: &'a str,
    pub manifest: &'a Manifest,
    pub extra_args: &'a [String],
    pub nogui: bool,
    pub java_major: u32,
    /// Keep the server attached to this terminal.
    pub foreground: bool,
    /// Restart a ready background server after a crash.
    pub auto_restart: bool,
}

/// This server's Minecraft process, or its supervisor while it restarts Java.
/// Without a trusted PID file, Java processes for this folder still count.
pub fn running(root: &Path) -> Option<u32> {
    server_pid(root)
        .or_else(|| {
            procs::find_server_processes(root)
                .first()
                .map(|info| info.pid)
        })
        .or_else(|| supervisor_pid(root))
}

// A PID file can outlive its process after a crash or reboot, and the
// operating system may give the number to an unrelated program. Each tracked
// PID is trusted only while its command line still matches what Pastel started.

fn server_pid(root: &Path) -> Option<u32> {
    let info = procs::process(read_pid(&state::pid_path(root))?)?;
    let cmd = info.command_line();
    if cmd.is_empty() || !procs::looks_like_minecraft_server_cmd(&cmd) {
        return None;
    }
    // Launch arguments are usually absolute, but the working directory is the
    // stronger evidence where it is readable.
    (info.cwd.is_empty() || procs::mentions_root(&info, &procs::root_spellings(&absolute(root))))
        .then_some(info.pid)
}

fn supervisor_pid(root: &Path) -> Option<u32> {
    helper_pid(&state::supervisor_pid_path(root), "__supervise", root)
}

fn hold_pid(root: &Path) -> Option<u32> {
    helper_pid(&state::hold_pid_path(root), "__hold-fifo", root)
}

fn helper_pid(path: &Path, subcommand: &str, root: &Path) -> Option<u32> {
    let info = procs::process(read_pid(path)?)?;
    let cmd = info.command_line();
    let names_root = procs::root_spellings(root)
        .iter()
        .any(|root| cmd.contains(&*root.to_string_lossy()));
    (cmd.contains(subcommand) && names_root).then_some(info.pid)
}

fn read_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path)
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|pid| *pid > 0)
}

/// A PID that is alive, whatever it runs.
fn read_alive_pid(path: &Path) -> Option<u32> {
    read_pid(path).filter(|pid| procs::alive(*pid))
}

fn write_pid(path: &Path, pid: u32) -> io::Result<()> {
    fs::write(path, format!("{pid}\n"))
}

fn absolute(root: &Path) -> PathBuf {
    paths::absolute(root).unwrap_or_else(|_| root.to_path_buf())
}

/// What `pastel stop` should stop.
pub enum StopTarget {
    /// This folder's server: a console `stop`, then signals after a grace period.
    Server { force: bool },
    /// One process, such as a server left behind by a deleted folder.
    Pid(u32),
    /// Every Minecraft server whose folder was deleted while it ran.
    Orphans,
}

pub fn stop(root: &Path, target: StopTarget) -> Result<()> {
    let force = match target {
        StopTarget::Pid(pid) => {
            ui::step(&format!("Stopping process {pid}…"));
            kill_pid(pid)?;
            // If it was ours, clear tracking.
            cleanup_server_files(root);
            ui::ok(&format!("Process {pid} stopped."));
            return Ok(());
        }
        StopTarget::Orphans => return stop_orphans(),
        StopTarget::Server { force } => force,
    };

    if server_pids(root).is_empty() {
        cleanup_server_files(root);
        // Help after "I deleted the folder while the server was running".
        let orphans = procs::find_orphan_servers();
        let Some(first) = orphans.first() else {
            ui::info("The server is not running.");
            return Ok(());
        };
        ui::warn(
            "Nothing is tracked for this folder, but other Minecraft server process(es) are still running:",
        );
        for orphan in &orphans {
            let cwd = if orphan.cwd.is_empty() {
                "(unknown folder)"
            } else {
                &orphan.cwd
            };
            ui::detail(&format!("pid {}  ·  {cwd}", orphan.pid));
        }
        ui::blank();
        ui::step(&format!(
            "Stop a specific one:  {}",
            ui::blue(&format!("./pastel stop -pid {}", first.pid))
        ));
        if orphans.len() > 1 {
            ui::step(&format!(
                "Or stop all deleted-folder servers:  {}",
                ui::blue("./pastel stop -orphans")
            ));
        } else {
            ui::detail(&format!("Or:  {}", ui::blue("./pastel stop -orphans")));
        }
        return Ok(());
    }

    ui::step("Stopping the server…");
    if !force {
        // Prefer a clean Minecraft stop through the console. This must not hang
        // when nothing reads it, after a crash or a deleted folder.
        if send_command(root, "stop").is_err() {
            ui::detail("no live console — signalling the process directly");
        } else if wait_until(STOP_GRACE_PERIOD, Duration::from_millis(500), || {
            server_pids(root).is_empty()
        }) {
            cleanup_server_files(root);
            ui::ok("Server stopped. See you next time!");
            return Ok(());
        } else {
            ui::warn("Still shutting down… asking a bit harder.");
        }
    }

    // SIGTERM runs Minecraft's shutdown hook, which saves the world. Large packs
    // can take a while, so only force the kill after a generous grace period.
    for pid in server_pids(root) {
        let _ = platform::terminate(pid);
    }
    if !wait_until(STOP_GRACE_PERIOD, Duration::from_millis(500), || {
        server_pids(root).is_empty()
    }) {
        ui::warn("Forcing shutdown.");
        for pid in server_pids(root) {
            platform::kill(pid);
        }
    }
    cleanup_server_files(root);
    ui::ok("Server stopped.");
    Ok(())
}

fn stop_orphans() -> Result<()> {
    let orphans = procs::find_orphan_servers();
    if orphans.is_empty() {
        ui::info("No orphan Minecraft servers found (deleted-folder processes).");
        return Ok(());
    }
    ui::step(&format!(
        "Stopping {} orphan server process(es)…",
        orphans.len()
    ));
    for orphan in orphans {
        ui::detail(&format!("pid {}  ·  {}", orphan.pid, orphan.cwd));
        match kill_pid(orphan.pid) {
            Ok(()) => ui::ok(&format!("Stopped {}", orphan.pid)),
            Err(error) => ui::warn(&error.to_string()),
        }
    }
    Ok(())
}

/// Asks one process to exit, and kills it after the grace period.
pub fn kill_pid(pid: u32) -> Result<()> {
    if pid <= 1 {
        return Err(format!("invalid pid {pid}").into());
    }
    if !procs::alive(pid) {
        return Err(format!("process {pid} is not running").into());
    }
    platform::terminate(pid).map_err(|error| format!("couldn't stop process {pid}: {error}"))?;
    if !wait_until(STOP_GRACE_PERIOD, Duration::from_millis(100), || {
        !procs::alive(pid)
    }) {
        platform::kill(pid);
    }
    Ok(())
}

/// Polls `done` until it holds or `timeout` passes.
fn wait_until(timeout: Duration, every: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(every);
    }
}

fn server_pids(root: &Path) -> Vec<u32> {
    let mut pids: Vec<u32> = server_pid(root).into_iter().collect();
    for info in procs::find_server_processes(root) {
        if !pids.contains(&info.pid) {
            pids.push(info.pid);
        }
    }
    pids
}

fn cleanup_server_files(root: &Path) {
    let _ = fs::remove_file(state::pid_path(root));
    stop_supervisor(root);
    stop_hold(root);
}

fn stop_supervisor(root: &Path) {
    if let Some(pid) = supervisor_pid(root)
        && pid != std::process::id()
    {
        platform::terminate_supervisor(pid);
    }
    let _ = fs::remove_file(state::supervisor_pid_path(root));
}

fn stop_hold(root: &Path) {
    if let Some(pid) = hold_pid(root) {
        platform::kill(pid);
    }
    let _ = fs::remove_file(state::hold_pid_path(root));
    let _ = fs::remove_file(state::console_in_path(root));
}

/// Starts the server in the background, or attached to this terminal.
pub fn start(options: &Options<'_>) -> Result<()> {
    // Discovered processes count even without a PID file.
    if running(options.root).is_some() {
        return Err(
            "the server is already running — try: ./pastel console  or  ./pastel stop".into(),
        );
    }
    // Nothing runs here, so any server.pid is left from a crash or reboot and its
    // number may belong to another program by now. Startup must not mistake it
    // for the new server.
    let _ = fs::remove_file(state::pid_path(options.root));
    ensure_eula(options.root)
        .map_err(|error| Error::from(error).context("couldn't accept the Minecraft EULA"))?;
    fs::create_dir_all(state::dir(options.root))?;
    let manifest = options.manifest;
    let xmx = if options.xmx.is_empty() {
        "4G"
    } else {
        options.xmx
    };
    // Forge and NeoForge read user_jvm_args.txt when it's referenced.
    if let Some(rel) = manifest
        .launch
        .as_ref()
        .and_then(|launch| launch.jvm_args_file.as_ref())
    {
        ensure_user_jvm_args(options.root, rel, xmx)
            .map_err(|error| Error::from(error).context(format_args!("couldn't write {rel}")))?;
    }
    let args = manifest.java_args(options.root, xmx, options.extra_args, options.nogui)?;

    ui::blank();
    ui::title("Starting your Minecraft server");
    let mut bits = Vec::new();
    if !manifest.minecraft().is_empty() {
        bits.push(format!("Minecraft {}", manifest.minecraft()));
    }
    if !manifest.loader_name().is_empty() {
        bits.push(ui::loader(manifest.loader_name()));
    }
    bits.push(xmx.to_owned());
    match manifest.mod_count() {
        0 => {}
        1 => bits.push("1 mod".to_owned()),
        count => bits.push(format!("{count} mods")),
    }
    ui::out(&format!("  {}", bits.join(&ui::dim(" · "))));
    if options.java_major > 0 {
        ui::detail(&format!("Java {}", options.java_major));
    }
    ui::detail("By running this server you agree to Mojang's EULA");
    ui::detail("https://aka.ms/MinecraftEULA");

    if options.foreground {
        start_foreground(options.root, options.java, &args)
    } else {
        platform::start_background(options.root, options.java, &args, options.auto_restart)
    }
}

fn ensure_user_jvm_args(root: &Path, rel: &str, xmx: &str) -> io::Result<()> {
    let path = paths::join_slash(root, rel);
    if path.is_file() {
        return Ok(());
    }
    fs::write(
        path,
        format!(
            "# Generated by Pastel — friend memory setting is applied as -Xmx on the command line too.\n-Xmx{xmx}\n"
        ),
    )
}

fn start_foreground(root: &Path, java: &Path, args: &[String]) -> Result<()> {
    ui::info("Foreground mode — press Ctrl+C or type stop to shut down.");
    ui::blank();
    let mut child = Command::new(java)
        .args(args)
        .current_dir(root)
        .spawn()
        .map_err(|error| format!("couldn't start Java ({}): {error}", java.display()))?;
    let _ = write_pid(&state::pid_path(root), child.id());
    let status = child.wait();
    let _ = fs::remove_file(state::pid_path(root));
    ui::blank();
    let status = status?;
    if status.success() {
        ui::ok("Server shut down cleanly.");
    } else {
        ui::warn(&format!(
            "Server exited (code {}).",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Writes `eula=true` so the server starts without a manual edit.
/// <https://aka.ms/MinecraftEULA>
pub fn ensure_eula(root: &Path) -> io::Result<()> {
    let path = root.join("eula.txt");
    if let Ok(text) = fs::read_to_string(&path)
        && text
            .lines()
            .map(str::trim)
            .any(|line| !line.starts_with('#') && line.eq_ignore_ascii_case("eula=true"))
    {
        return Ok(());
    }
    fs::write(
        path,
        "# By changing the setting below to TRUE you are indicating your agreement to our EULA (https://aka.ms/MinecraftEULA).\n# Accepted automatically by Pastel when starting the server.\neula=true\n",
    )
}

/// Whether a background supervisor should launch Java again. Clean shutdowns
/// and startup failures (never ready) stay stopped.
fn should_restart(status: &io::Result<ExitStatus>, ready: bool, auto_restart: bool) -> bool {
    auto_restart && ready && matches!(status, Ok(status) if !status.success())
}

/// Shared supervision loop: runs Java until it stops for good.
///
/// `launch` starts one Java process and returns it with the console log
/// handle; `on_exit` runs after each exit, before any restart.
fn supervise_loop(
    root: &Path,
    auto_restart: bool,
    mut launch: impl FnMut() -> Result<(Child, fs::File)>,
    mut on_exit: impl FnMut(),
) -> Result<()> {
    let log_path = state::console_log_path(root);
    let latest_log = root.join("logs").join("latest.log");
    loop {
        let start_console = boot::log_size(&log_path);
        let start_latest = boot::log_size(&latest_log);
        let (mut child, mut log) = launch()?;
        if let Err(error) = write_pid(&state::pid_path(root), child.id()) {
            let _ = child.kill();
            return Err(error.into());
        }
        let status = child.wait();
        on_exit();
        let _ = fs::remove_file(state::pid_path(root));
        let ready = boot::log_contains_done_since(&log_path, start_console)
            || boot::log_contains_done_since(&latest_log, start_latest);
        if !should_restart(&status, ready, auto_restart) {
            drop(log);
            cleanup_server_files(root);
            let status = status?;
            return if status.success() {
                Ok(())
            } else {
                Err(status.to_string().into())
            };
        }
        let _ = io::Write::write_all(
            &mut log,
            "[Pastel] Server crashed; restarting in 5 seconds…\n".as_bytes(),
        );
        drop(log);
        thread::sleep(Duration::from_secs(5));
    }
}

/// Starts a hidden helper process: this executable with `args`.
fn spawn_helper(
    args: &[&std::ffi::OsStr],
    configure: impl FnOnce(&mut Command),
) -> io::Result<Child> {
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure(&mut command);
    command.spawn()
}

/// Waits for `child` on a thread and reports its exit once.
fn watch_exit(mut child: Child) -> Receiver<io::Result<ExitStatus>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(child.wait());
    });
    receiver
}

/// Whether the watched process has exited (its report may already be consumed).
fn exited(receiver: &Receiver<io::Result<ExitStatus>>) -> bool {
    !matches!(receiver.try_recv(), Err(TryRecvError::Empty))
}

fn wait_for_server_pid(
    root: &Path,
    supervisor: &Receiver<io::Result<ExitStatus>>,
    timeout: Duration,
) -> Result<u32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match supervisor.try_recv() {
            Ok(Ok(status)) if status.success() => {
                return Err("server supervisor exited before Java started".into());
            }
            Ok(Ok(status)) => return Err(format!("server supervisor exited: {status}").into()),
            Ok(Err(error)) => return Err(format!("server supervisor exited: {error}").into()),
            Err(TryRecvError::Disconnected) => {
                return Err("server supervisor exited before Java started".into());
            }
            Err(TryRecvError::Empty) => {}
        }
        if let Some(pid) = read_alive_pid(&state::pid_path(root)) {
            return Ok(pid);
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err("timed out waiting for Java to start".into())
}

/// Waits for the background server to boot, then tells the user what's next.
fn finish_background_start(
    root: &Path,
    supervisor: Receiver<io::Result<ExitStatus>>,
) -> Result<()> {
    let result = wait_for_server_pid(root, &supervisor, Duration::from_secs(10))
        .and_then(|pid| boot::wait_for_boot(root, pid, &supervisor));
    if let Err(error) = result {
        cleanup_server_files(root);
        return Err(error);
    }
    ui::big_ok("Your server is running in the background");
    ui::title("What you can do now");
    for (command, description) in [
        ("./pastel console", "See the live log and type commands"),
        ("./pastel stop", "Shut the server down when you're done"),
        ("./pastel", "Check status anytime"),
    ] {
        ui::out(&format!(
            "  {}  {description}",
            ui::blue(&format!("{command:<18}"))
        ));
    }
    ui::blank();
    ui::detail("You can close this terminal — the server keeps going.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit_status(code: i32) -> io::Result<ExitStatus> {
        let mut command = if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/c", &format!("exit {code}")]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", &format!("exit {code}")]);
            command
        };
        command.status()
    }

    #[test]
    fn only_ready_servers_that_crash_restart() {
        let crashed = exit_status(7);
        assert!(should_restart(&crashed, true, true));
        assert!(!should_restart(&crashed, false, true), "startup failure");
        assert!(!should_restart(&crashed, true, false), "auto restart off");
        assert!(
            !should_restart(&exit_status(0), true, true),
            "clean shutdown"
        );
    }

    #[test]
    fn stale_pid_files_never_claim_unrelated_processes() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(state::dir(root.path())).unwrap();
        // This test process is alive but is neither a Pastel helper nor a server.
        for path in [
            state::pid_path(root.path()),
            state::supervisor_pid_path(root.path()),
            state::hold_pid_path(root.path()),
        ] {
            write_pid(&path, std::process::id()).unwrap();
        }
        assert_eq!(server_pid(root.path()), None);
        assert_eq!(supervisor_pid(root.path()), None);
        assert_eq!(hold_pid(root.path()), None);
    }

    #[test]
    fn eula_is_accepted_once() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("eula.txt"), "#eula=true\neula=false\n").unwrap();
        ensure_eula(root.path()).unwrap();
        let text = fs::read_to_string(root.path().join("eula.txt")).unwrap();
        assert!(text.ends_with("eula=true\n"));
        fs::write(root.path().join("eula.txt"), "EULA=TRUE\n").unwrap();
        ensure_eula(root.path()).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("eula.txt")).unwrap(),
            "EULA=TRUE\n"
        );
    }
}
