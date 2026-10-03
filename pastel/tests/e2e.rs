//! A real Fabric server driven through Pastel's commands: install a pack, run
//! it in the background, send a console command, survive a crash, and stop.
//! It downloads Minecraft, Fabric, and usually Java, so it only runs on request:
//!
//! ```text
//! cargo test -p pastel --locked --test e2e -- --ignored
//! ```
//!
//! The server folder stays in `target/tmp/pastel-e2e` for inspection.

use pastel::{runtime, state};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const FABRIC_API_JAR: &str = "mods/fabric-api-0.161.0+26.3.jar";
const READY: &str = r#"For help, type "help""#;

#[test]
#[ignore = "downloads and boots a real Minecraft server"]
fn fabric_server_lifecycle() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("pastel-e2e");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    write_pack(&root.join("e2e.mrpack"));
    let server = Server { root };

    server.pastel(&["install", "./e2e.mrpack"]);
    assert!(server.root.join(FABRIC_API_JAR).is_file());
    assert_eq!(
        fs::read_to_string(server.root.join("server.properties")).unwrap(),
        "server-port=0\n"
    );

    // Without a terminal, run returns once Minecraft is ready.
    server.pastel(&["run"]);
    let first = runtime::running(&server.root).expect("server running after run");

    server.console("say pastel-e2e");
    server.wait_for("the console command", |server| {
        server.console_log().contains("[Server] pastel-e2e")
    });

    hard_kill(first);
    server.wait_for("the crash restart", |server| {
        server.console_log().matches(READY).count() == 2
    });
    let second = runtime::running(&server.root).expect("server running after restart");
    assert_ne!(first, second);

    server.pastel(&["stop"]);
    assert_eq!(runtime::running(&server.root), None);
    assert!(server.console_log().contains("Saving worlds"));
    // A requested stop is not a crash, so it stays stopped past the restart delay.
    thread::sleep(Duration::from_secs(7));
    assert_eq!(runtime::running(&server.root), None);
}

/// A minimal Fabric pack with one hashed download and a server-only override
/// that lets Minecraft pick a free port.
fn write_pack(path: &Path) {
    let index = serde_json::json!({
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": "1.0.0",
        "name": "Pastel E2E",
        "dependencies": { "minecraft": "26.3", "fabric-loader": "0.19.5" },
        "files": [{
            "path": FABRIC_API_JAR,
            "hashes": {
                "sha1": "53f9ee02c3702734d90520aa8c9719bf0e1c8d4f",
                "sha512": "ed6b2586d6fde11fde8472f5a527c51e99b67026e46f94d4bfd85e7e28ce5ee299173ee16ad576ceb51f39f98d30a811086a6deb1a86a524859cc16e12da109d"
            },
            "env": { "client": "required", "server": "required" },
            "downloads": ["https://cdn.modrinth.com/data/P7dR8mSH/versions/bNnaTiuM/fabric-api-0.161.0%2B26.3.jar"],
            "fileSize": 2620967
        }]
    });
    let mut pack = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    pack.start_file("modrinth.index.json", options).unwrap();
    pack.write_all(index.to_string().as_bytes()).unwrap();
    pack.start_file("server-overrides/server.properties", options)
        .unwrap();
    pack.write_all(b"server-port=0\n").unwrap();
    pack.finish().unwrap();
}

struct Server {
    root: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        if runtime::running(&self.root).is_some() {
            let _ = self.command(&["stop", "-force"]).status();
        }
    }
}

impl Server {
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pastel"));
        command
            .args(args)
            .current_dir(&self.root)
            .stdin(Stdio::null());
        command
    }

    fn pastel(&self, args: &[&str]) {
        let status = self.command(args).status().unwrap();
        assert!(
            status.success(),
            "pastel {} failed: {status}",
            args.join(" ")
        );
    }

    /// Sends one line through `pastel console`, which leaves at end of input.
    fn console(&self, line: &str) {
        let mut console = self
            .command(&["console"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(console.stdin.take().unwrap(), "{line}").unwrap();
        let status = console.wait().unwrap();
        assert!(status.success(), "pastel console failed: {status}");
    }

    fn console_log(&self) -> String {
        fs::read_to_string(state::console_log_path(&self.root)).unwrap_or_default()
    }

    fn wait_for(&self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(300);
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(250));
        }
    }
}

/// Ends Java the way an out-of-memory killer would, without a shutdown.
#[cfg(unix)]
fn hard_kill(pid: u32) {
    let status = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(windows)]
fn hard_kill(pid: u32) {
    let status = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}
