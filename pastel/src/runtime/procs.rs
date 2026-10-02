//! Finding this server's processes, and telling them apart from unrelated ones.

use crate::paths;
use std::path::{Path, PathBuf};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// A process as Pastel sees it.
#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub pid: u32,
    pub args: Vec<String>,
    /// The working directory, or empty when unreadable. On Linux it ends with
    /// ` (deleted)` after the folder was removed under the process.
    pub cwd: String,
}

impl ProcInfo {
    pub fn command_line(&self) -> String {
        self.args.join(" ")
    }
}

fn refresh(which: ProcessesToUpdate<'_>) -> Vec<ProcInfo> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        which,
        true,
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always),
    );
    system
        .processes()
        .iter()
        .filter(|(_, process)| process.thread_kind().is_none())
        .map(|(pid, process)| ProcInfo {
            pid: pid.as_u32(),
            args: process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
            cwd: process
                .cwd()
                .map(|cwd| cwd.display().to_string())
                .unwrap_or_default(),
        })
        .collect()
}

/// The running process `pid`, if there is one.
pub fn process(pid: u32) -> Option<ProcInfo> {
    if pid == 0 {
        return None;
    }
    refresh(ProcessesToUpdate::Some(&[Pid::from_u32(pid)]))
        .into_iter()
        .find(|info| info.pid == pid)
}

pub fn alive(pid: u32) -> bool {
    process(pid).is_some()
}

/// Ends `pid` at once. Returns whether Windows accepted the request.
#[cfg(windows)]
pub fn kill(pid: u32) -> bool {
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().without_tasks(),
    );
    system.process(pid).is_some_and(sysinfo::Process::kill)
}

/// The server folder as given and, when it differs, its real path, so a
/// folder reached through a symbolic link or typed in another case still
/// matches the server's working directory.
pub fn root_spellings(root: &Path) -> Vec<PathBuf> {
    let given = paths::clean(root);
    let mut spellings = vec![given.clone()];
    if let Ok(real) = root.canonicalize()
        && real != given
    {
        spellings.push(real);
    }
    spellings
}

/// Java processes that look like this server's dedicated Minecraft process.
pub fn find_server_processes(root: &Path) -> Vec<ProcInfo> {
    let roots = root_spellings(root);
    let mut found: Vec<ProcInfo> = refresh(ProcessesToUpdate::All)
        .into_iter()
        .filter(|info| info.pid > 1 && is_minecraft_server_process(info, &roots))
        .collect();
    found.sort_by_key(|info| info.pid);
    found
}

/// Minecraft servers whose working folder was deleted while they ran.
pub fn find_orphan_servers() -> Vec<ProcInfo> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    let mut found: Vec<ProcInfo> = refresh(ProcessesToUpdate::All)
        .into_iter()
        .filter(|info| {
            info.pid > 1
                && looks_like_minecraft_server_cmd(&info.command_line())
                && (info.cwd.is_empty() || info.cwd.contains("(deleted)"))
        })
        .collect();
    found.sort_by_key(|info| info.pid);
    found
}

/// Matches a dedicated server for the folder, by a path in its arguments or
/// by its working directory, since launch arguments are often relative.
pub fn is_minecraft_server_process(info: &ProcInfo, roots: &[PathBuf]) -> bool {
    looks_like_minecraft_server_cmd(&info.command_line()) && mentions_root(info, roots)
}

/// Whether the arguments or working directory point at one of `roots`.
pub fn mentions_root(info: &ProcInfo, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|root| command_contains_path(&info.args, root) || cwd_matches_root(&info.cwd, root))
}

pub fn looks_like_minecraft_server_cmd(cmd: &str) -> bool {
    let low = cmd.to_lowercase();
    if !low.contains("java") || low.contains("__hold-fifo") || low.contains("__supervise") {
        return false;
    }
    // Leave common non-server Java alone: build tools and editors.
    if ["gradle", "jdt.ls", "language server", "intellij"]
        .iter()
        .any(|tool| low.contains(tool))
    {
        return false;
    }
    low.contains("fabric-server-")
        || low.contains("quilt-server-")
        || low.contains("unix_args.txt")
        || low.contains("win_args.txt")
        || (low.contains("-jar") && (low.contains("nogui") || low.contains("server.jar")))
        || (low.contains("neoforge") && low.contains('@'))
}

pub fn cwd_matches_root(cwd: &str, root: &Path) -> bool {
    let cwd = cwd.trim();
    if cwd.is_empty() || root.as_os_str().is_empty() {
        return false;
    }
    let cwd = paths::clean(Path::new(cwd.strip_suffix(" (deleted)").unwrap_or(cwd)));
    let root = paths::clean(root);
    if cfg!(windows) {
        cwd.to_string_lossy()
            .eq_ignore_ascii_case(&root.to_string_lossy())
    } else {
        cwd == root
    }
}

/// Whether the command line mentions `root` or a path inside it. A match must
/// end at a path boundary, so `/srv/mc` doesn't claim `/srv/mc-test`.
#[cfg(not(windows))]
pub fn command_contains_path(args: &[String], root: &Path) -> bool {
    let root = root.to_string_lossy();
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return false;
    }
    let cmd = args.join(" ");
    let mut rest = cmd.as_str();
    while let Some(index) = rest.find(root) {
        let end = index + root.len();
        if matches!(rest.as_bytes().get(end), None | Some(b'/' | b' ')) {
            return true;
        }
        rest = &rest[index + 1..];
    }
    false
}

/// Whether an absolute argument (or `@args` file) is `root` or inside it.
#[cfg(windows)]
pub fn command_contains_path(args: &[String], root: &Path) -> bool {
    let root = paths::clean(root).to_string_lossy().to_lowercase();
    if root.is_empty() {
        return false;
    }
    let prefix = format!("{}\\", root.trim_end_matches(['\\', '/']));
    args.iter().any(|arg| {
        let candidate = Path::new(arg.strip_prefix('@').unwrap_or(arg));
        if !candidate.is_absolute() {
            return false;
        }
        let candidate = paths::clean(candidate).to_string_lossy().to_lowercase();
        candidate == root || candidate.starts_with(&prefix)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(cmd: &str) -> Vec<String> {
        cmd.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn working_directories_match_even_after_deletion() {
        let root = Path::new("/srv/pastel");
        assert!(cwd_matches_root("/srv/pastel", root));
        assert!(cwd_matches_root("/srv/pastel (deleted)", root));
        assert!(!cwd_matches_root("/srv/other", root));
    }

    #[test]
    fn recognizes_dedicated_server_command_lines() {
        for cmd in [
            "java -Xmx4G -jar fabric-server-mc.26.2-loader.0.19.3-launcher.1.1.1.jar nogui",
            "/path/.pastel/jre/21/bin/java -Xmx4G -jar fabric-server-mc.1.21.1-loader.0.16.14-launcher.1.1.1.jar nogui",
            "java @user_jvm_args.txt @libraries/net/neoforged/neoforge/21/unix_args.txt nogui",
        ] {
            assert!(looks_like_minecraft_server_cmd(cmd), "{cmd}");
        }
        for cmd in [
            "java -jar gradle-server.jar",
            "java __hold-fifo /tmp/x",
            "/usr/bin/python3",
        ] {
            assert!(!looks_like_minecraft_server_cmd(cmd), "{cmd}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn command_paths_stop_at_path_boundaries() {
        let root = Path::new("/srv/mc");
        assert!(!command_contains_path(
            &args("java -jar /srv/mc-test/fabric-server.jar nogui"),
            root
        ));
        assert!(command_contains_path(
            &args("java -jar /srv/mc/fabric-server.jar nogui"),
            root
        ));
    }

    #[cfg(windows)]
    #[test]
    fn command_paths_match_case_insensitively_at_boundaries() {
        let cmd = args(r"C:\Java\bin\java.exe -jar C:\Servers\MyPack\fabric-server-mc.jar nogui");
        assert!(command_contains_path(&cmd, Path::new(r"C:\Servers\MyPack")));
        assert!(command_contains_path(&cmd, Path::new(r"c:\servers\mypack")));
        assert!(!command_contains_path(&cmd, Path::new(r"C:\Servers\My")));
        assert!(!command_contains_path(
            &args(r"java -jar C:\Servers\MyPack-Dev\server.jar nogui"),
            Path::new(r"C:\Servers\MyPack")
        ));
    }

    #[test]
    fn the_current_process_is_alive() {
        assert!(alive(std::process::id()));
        assert!(!alive(99_999_999));
    }
}
