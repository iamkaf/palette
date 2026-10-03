//! Background servers on Windows: a supervisor in a kill-on-close job, and an
//! owner-restricted named pipe for console commands.

use super::{finish_background_start, kill_pid, procs, spawn_helper, supervise_loop, watch_exit};
use crate::{Context, Result, state};
use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, RandomState};
use std::io::{self, BufRead, BufReader, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::ptr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, GetCurrentProcess, OpenProcessToken,
};

const CONSOLE_NOT_AVAILABLE: &str =
    "console not available (is the server running via ./pastel run?)";

/// Windows has no polite signal for a hidden console process, so this ends it.
pub fn terminate(pid: u32) -> Result<()> {
    if procs::kill(pid) {
        Ok(())
    } else {
        Err("Windows refused to end it".into())
    }
}

pub fn kill(pid: u32) {
    procs::kill(pid);
}

/// Ending the supervisor closes its job, which ends Java with it.
pub fn terminate_supervisor(pid: u32) {
    let _ = kill_pid(pid);
}

pub fn start_background(
    root: &Path,
    java: &Path,
    args: &[String],
    auto_restart: bool,
) -> Result<()> {
    File::create(state::console_log_path(root))?;
    let mut supervisor_args = vec![
        "__supervise".as_ref(),
        root.as_os_str(),
        if auto_restart { "true" } else { "false" }.as_ref(),
        "--".as_ref(),
        java.as_os_str(),
    ];
    supervisor_args.extend(args.iter().map(std::ffi::OsStr::new));
    let supervisor = spawn_helper(&supervisor_args, |command| {
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    })
    .map_err(|error| format!("couldn't start server supervisor: {error}"))?;
    finish_background_start(root, watch_exit(supervisor))
}

/// Owns the background Java process. The supervisor joins a kill-on-close job
/// before publishing its PID, so Java dies with it if it is ended abruptly.
pub fn supervise(root: &Path, java: &str, args: &[String], auto_restart: bool) -> Result<()> {
    join_kill_on_close_job().context("supervisor job")?;
    fs::write(
        state::supervisor_pid_path(root),
        format!("{}\n", std::process::id()),
    )?;

    // Console lines arrive through the pipe and feed Java's stdin, which stays
    // open while `pastel console` sessions come and go.
    let listener = ConsoleListener::bind().context("console listener")?;
    fs::write(
        state::console_in_path(root),
        format!("pipe:{}\n", listener.name),
    )?;
    let stdin: Arc<Mutex<Option<ChildStdin>>> = Arc::default();
    {
        let stdin = Arc::clone(&stdin);
        thread::spawn(move || listener.serve(&stdin));
    }

    let log_path = state::console_log_path(root);
    let result = supervise_loop(
        root,
        auto_restart,
        || {
            let log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)?;
            let mut child = Command::new(java)
                .args(args)
                .current_dir(root)
                .stdin(Stdio::piped())
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log.try_clone()?))
                .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
                .spawn()
                .map_err(|error| format!("couldn't start Java ({java}): {error}"))?;
            *lock(&stdin) = child.stdin.take();
            Ok((child, log))
        },
        || *lock(&stdin) = None,
    );
    let _ = fs::remove_file(state::console_in_path(root));
    result
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn join_kill_on_close_job() -> io::Result<()> {
    // SAFETY: the job handle and limit structure are created and owned here, and
    // the structure outlives the call that reads it. The job handle deliberately
    // stays open for the supervisor's lifetime: closing the last handle would end
    // this process as a job member.
    unsafe {
        let job = CreateJobObjectW(ptr::null(), ptr::null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const info).cast::<c_void>(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
            || AssignProcessToJobObject(job, GetCurrentProcess()) == 0
        {
            let error = io::Error::last_os_error();
            drop(OwnedHandle::from_raw_handle(job));
            return Err(error);
        }
    }
    Ok(())
}

/// A named pipe that only this Windows account, administrators, and SYSTEM can
/// open, and only from this machine.
struct ConsoleListener {
    name: String,
    wide_name: Vec<u16>,
    /// The instance waiting for the next client.
    pending: OwnedHandle,
}

impl ConsoleListener {
    fn bind() -> io::Result<Self> {
        let state = RandomState::new();
        let name = format!(
            r"\\.\pipe\pastel-console-{:016x}{:016x}",
            state.hash_one(std::process::id()),
            RandomState::new().hash_one(Instant::now())
        );
        let wide_name = wide(&name);
        // The first instance claims the name, so nobody can create it first.
        let pending = create_instance(&wide_name, true)?;
        Ok(Self {
            name,
            wide_name,
            pending,
        })
    }

    fn serve(self, stdin: &Arc<Mutex<Option<ChildStdin>>>) {
        let Self {
            wide_name,
            mut pending,
            ..
        } = self;
        loop {
            // SAFETY: `pending` is a pipe instance this process owns.
            let connected = unsafe { ConnectNamedPipe(pending.as_raw_handle(), ptr::null_mut()) }
                != 0
                || io::Error::last_os_error().raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32);
            let next = match create_instance(&wide_name, false) {
                Ok(next) => next,
                Err(_) => return,
            };
            let connection = File::from(std::mem::replace(&mut pending, next));
            if connected {
                let stdin = Arc::clone(stdin);
                thread::spawn(move || feed(connection, &stdin));
            }
        }
    }
}

/// Writes each console line from one client to the server's stdin.
fn feed(connection: File, stdin: &Mutex<Option<ChildStdin>>) {
    for line in BufReader::new(connection).lines() {
        let Ok(line) = line else {
            return;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut stdin = lock(stdin);
        let Some(stdin) = stdin.as_mut() else {
            return;
        };
        if writeln!(stdin, "{line}").is_err() {
            return;
        }
    }
}

fn create_instance(wide_name: &[u16], first: bool) -> io::Result<OwnedHandle> {
    let descriptor = owner_only_descriptor()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let mode = if first {
        PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE
    } else {
        PIPE_ACCESS_INBOUND
    };
    // SAFETY: the name is NUL-terminated, and the attributes and descriptor
    // outlive the call. A valid handle is owned by the returned OwnedHandle.
    unsafe {
        let handle = CreateNamedPipeW(
            wide_name.as_ptr(),
            mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            0,
            64 * 1024,
            0,
            &attributes,
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedHandle::from_raw_handle(handle))
    }
}

/// Memory Windows allocated with `LocalAlloc`, freed on drop.
struct LocalMemory(*mut c_void);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        // SAFETY: the pointer came from a Windows API that documents LocalFree.
        unsafe {
            LocalFree(self.0);
        }
    }
}

/// A protected DACL that denies network logons, then allows only SYSTEM,
/// administrators, and the account that started Pastel.
fn owner_only_descriptor() -> io::Result<LocalMemory> {
    let sddl = wide(&console_pipe_sddl(&current_user_sid()?));
    let mut descriptor = ptr::null_mut();
    // SAFETY: the SDDL string is NUL-terminated; Windows allocates the descriptor.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalMemory(descriptor))
}

fn console_pipe_sddl(user_sid: &str) -> String {
    format!("D:P(D;;GA;;;NU)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{user_sid})")
}

fn current_user_sid() -> io::Result<String> {
    // SAFETY: every handle and buffer is owned here and outlives the calls that
    // use it; the TOKEN_USER buffer is 8-byte aligned and sized by Windows.
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = OwnedHandle::from_raw_handle(token);
        let mut size = 0u32;
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut size,
        );
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut text = ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return Err(io::Error::last_os_error());
        }
        let text_memory = LocalMemory(text.cast());
        let length = (0..).take_while(|&index| *text.add(index) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        drop(text_memory);
        if sid.is_empty() {
            return Err(io::Error::other("current user has no security identifier"));
        }
        Ok(sid)
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The console isn't used on Windows; the named pipe replaces it.
pub fn hold_fifo(_: &Path) -> Result<()> {
    Err("console hold is not used on Windows".into())
}

/// Sends one console line to the background server.
pub fn send_command(root: &Path, line: &str) -> Result<()> {
    let address =
        fs::read_to_string(state::console_in_path(root)).map_err(|_| CONSOLE_NOT_AVAILABLE)?;
    let name = address
        .trim()
        .strip_prefix("pipe:")
        .ok_or(CONSOLE_NOT_AVAILABLE)?
        .to_owned();
    // Another client may hold the only free instance for a moment.
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut pipe = loop {
        match OpenOptions::new().write(true).open(&name) {
            Ok(pipe) => break pipe,
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(crate::Error::from(error).context("open console")),
        }
    };
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }
    writeln!(pipe, "{line}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_console_pipe_is_owner_restricted() {
        let sddl = console_pipe_sddl(&current_user_sid().unwrap());
        assert!(!sddl.contains(";;;WD)") && !sddl.contains(";;;AN)"));
        assert!(sddl.starts_with("D:P(D;;GA;;;NU)"));
        owner_only_descriptor().unwrap();
    }

    #[test]
    fn console_lines_reach_the_server() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(state::dir(root.path())).unwrap();
        let received = root.path().join("received.txt");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::windows::tests::fake_server_process"])
            .env("PASTEL_FAKE_SERVER_OUT", &received)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = Arc::new(Mutex::new(child.stdin.take()));
        let listener = ConsoleListener::bind().unwrap();
        fs::write(
            state::console_in_path(root.path()),
            format!("pipe:{}\n", listener.name),
        )
        .unwrap();
        {
            let stdin = Arc::clone(&stdin);
            thread::spawn(move || listener.serve(&stdin));
        }
        send_command(root.path(), "say hello").unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(fs::read_to_string(received).unwrap(), "say hello\n");
    }

    /// Stands in for Java: this test binary, copying one stdin line to a file.
    #[test]
    fn fake_server_process() {
        if let Some(out) = std::env::var_os("PASTEL_FAKE_SERVER_OUT") {
            let mut line = String::new();
            io::stdin().read_line(&mut line).unwrap();
            fs::write(out, line).unwrap();
        }
    }
}
