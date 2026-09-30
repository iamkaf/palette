//! A dedicated server Modstage runs for Chalk, followed through its log and driven over
//! its remote console.

use crate::rcon::Rcon;
use crate::{Result, modstage};
use std::collections::hash_map::RandomState;
use std::fs::File;
use std::hash::{BuildHasher, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The remote console settings for one server.
pub struct Console {
    pub port: u16,
    pub password: String,
}

impl Console {
    /// A free port and a fresh password, so other programs can't send commands to it.
    pub fn new() -> Result<Self> {
        let random = || RandomState::new().build_hasher().finish();
        Ok(Self {
            port: modstage::free_port()?,
            password: format!("{:016x}{:016x}", random(), random()),
        })
    }

    pub fn properties(&self) -> [(&'static str, String); 3] {
        [
            ("enable-rcon", "true".into()),
            ("rcon.port", self.port.to_string()),
            ("rcon.password", self.password.clone()),
        ]
    }

    pub fn connect(&self) -> Result<Rcon> {
        Rcon::connect(self.port, &self.password)
    }
}

pub struct Server {
    child: Child,
    pub log: Log,
    pub log_path: PathBuf,
}

/// How a server's start ended, with everything it logged until then.
pub enum Started {
    Ready(String),
    Stopped(String),
}

impl Server {
    /// Starts `instance` and keeps it running, writing its output to `log_path`.
    pub fn start(config: &Path, instance: &str, log_path: PathBuf) -> Result<Self> {
        let output = File::create(&log_path)?;
        let child = Command::new("modstage")
            .arg("--config")
            .arg(config)
            .args(["run", "server", instance, "--keep-alive"])
            .stdout(output.try_clone()?)
            .stderr(output)
            .stdin(Stdio::null())
            .spawn()
            .map_err(|error| format!("running modstage: {error}"))?;
        Ok(Self {
            child,
            log: Log::open(&log_path)?,
            log_path,
        })
    }

    /// Waits until the console accepts commands, or the server stops trying.
    pub fn wait_ready(&mut self) -> Result<Started> {
        let started = Instant::now();
        let mut text = String::new();
        loop {
            text.push_str(&self.log.read_new()?);
            if text.contains("RCON running on") {
                return Ok(Started::Ready(text));
            }
            if self.exited()?.is_some() {
                text.push_str(&self.log.read_new()?);
                return Ok(Started::Stopped(text));
            }
            if started.elapsed() > Duration::from_secs(600) {
                let _ = self.child.kill();
                return Ok(Started::Stopped(text));
            }
            thread::sleep(Duration::from_millis(250));
        }
    }

    pub fn exited(&mut self) -> Result<Option<ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    /// Stops the server through its console and returns what it logged meanwhile.
    pub fn stop(&mut self, rcon: &mut Rcon) -> Result<String> {
        let _ = rcon.command("stop");
        let started = Instant::now();
        while self.exited()?.is_none() {
            if started.elapsed() > Duration::from_secs(60) {
                let _ = self.child.kill();
                break;
            }
            thread::sleep(Duration::from_millis(250));
        }
        self.log.read_new()
    }
}

/// Follows the server's output as Modstage writes it.
pub struct Log {
    file: File,
    partial: String,
}

impl Log {
    fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            file: File::open(path)?,
            partial: String::new(),
        })
    }

    /// The complete lines written since the last read.
    pub fn read_new(&mut self) -> Result<String> {
        let mut bytes = Vec::new();
        self.file.read_to_end(&mut bytes)?;
        self.partial.push_str(&String::from_utf8_lossy(&bytes));
        let complete = self.partial.rfind('\n').map_or(0, |end| end + 1);
        let rest = self.partial.split_off(complete);
        Ok(std::mem::replace(&mut self.partial, rest))
    }

    /// Everything written until the log stays quiet for `settle`, up to 30 seconds.
    pub fn read_until_quiet(&mut self, settle: Duration) -> Result<String> {
        let started = Instant::now();
        let mut last_line = started;
        let mut text = String::new();
        while last_line.elapsed() < settle && started.elapsed() < Duration::from_secs(30) {
            thread::sleep(Duration::from_millis(50));
            let new = self.read_new()?;
            if !new.is_empty() {
                text.push_str(&new);
                last_line = Instant::now();
            }
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn the_log_hands_out_only_complete_lines() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("server.log");
        fs::write(&path, "first\nsec").expect("write");
        let mut log = Log::open(&path).expect("open");
        assert_eq!(log.read_new().expect("read"), "first\n");
        fs::write(&path, "first\nsecond\n").expect("write");
        assert_eq!(log.read_new().expect("read"), "second\n");
    }
}
