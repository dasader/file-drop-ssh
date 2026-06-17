//! Terminal mode: read the SSH alias + remote dir from config, resolve the
//! remote absolute path, copy the remote paths to the clipboard, and sync the
//! files over scp on a background thread. No editor window / auto-paste.

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use crate::clipboard::set_clipboard_text;
use crate::config::Server;
use crate::sys::{log, now_stamp};
use crate::util::{sanitize_filename, shell_quote};
use crate::StateKind;

const SSH_TIMEOUT: u32 = 30;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Entry point for drag-drop and clipboard paste (GUI). Runs on a worker thread
/// so the UI thread never blocks on ssh. `server` is the currently active one.
pub fn handle_files(files: Vec<PathBuf>, server: Server) {
    crate::set_state(StateKind::Uploading, "Preparing...");
    std::thread::spawn(move || {
        run(files, &server);
    });
}

/// Resolve remote dir, copy remote paths to the clipboard, mkdir+touch, scp.
/// Synchronous; returns true on full success. Also used by CLI mode.
pub fn run(files: Vec<PathBuf>, server: &Server) -> bool {
    let files: Vec<PathBuf> = files.into_iter().filter(|p| p.exists()).collect();
    if files.is_empty() {
        crate::set_state(StateKind::Error, "No files");
        return false;
    }

    log(&format!(
        "Upload target: server '{}' alias={}",
        server.name, server.alias
    ));

    let remote_dir = match resolve_remote_dir(&server.alias, &server.remote_dir) {
        Some(d) => d,
        None => {
            crate::set_state(StateKind::Error, "Remote unreachable");
            return false;
        }
    };

    let ts = now_stamp();
    let mut remote_files: Vec<String> = Vec::new();
    for (i, p) in files.iter().enumerate() {
        let fname = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let remote_name = format!("{}-{}-{}", ts, i, sanitize_filename(&fname));
        remote_files.push(format!("{}/{}", remote_dir, remote_name));
    }

    // Single-quote each remote path so it pastes as literal absolute path(s).
    let payload = remote_files
        .iter()
        .map(|p| format!("'{}'", p))
        .collect::<Vec<_>>()
        .join(" ");
    set_clipboard_text(&payload);
    log(&format!("Clipboard set: {}", payload));

    let total = remote_files.len();

    // mkdir + touch so the paths exist immediately (before scp finishes).
    let mut remote_cmd = format!("mkdir -p {}", shell_quote(&remote_dir));
    remote_cmd.push_str(" && touch");
    for r in &remote_files {
        remote_cmd.push(' ');
        remote_cmd.push_str(&shell_quote(r));
    }
    if !run_ssh(&server.alias, &remote_cmd) {
        crate::set_state(StateKind::Error, "Remote prep failed");
        return false;
    }

    let mut fails = 0;
    for (i, (local, remote)) in files.iter().zip(remote_files.iter()).enumerate() {
        let fname = local
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        crate::set_state(
            StateKind::Uploading,
            &format!("Syncing {}/{} {}", i + 1, total, fname),
        );
        if run_scp(local, &server.alias, remote) {
            log(&format!("OK: {} -> {}", local.display(), remote));
        } else {
            log(&format!("FAIL scp: {}", local.display()));
            fails += 1;
        }
    }

    if fails > 0 {
        crate::set_state(StateKind::Error, &format!("{} upload(s) failed", fails));
        false
    } else {
        let s = if total > 1 { "s" } else { "" };
        crate::set_state(StateKind::Success, &format!("{} file{} ready", total, s));
        true
    }
}

/// Turn the configured remote dir into an absolute path on the remote,
/// expanding a leading `~` via the remote `$HOME` (queried once, cached).
fn resolve_remote_dir(alias: &str, dir: &str) -> Option<String> {
    if dir.starts_with('/') {
        return Some(dir.trim_end_matches('/').to_string());
    }
    let home = remote_home(alias)?;
    let rest = dir.trim_start_matches('~').trim_start_matches('/');
    let mut full = home.trim_end_matches('/').to_string();
    if !rest.is_empty() {
        full.push('/');
        full.push_str(rest);
    }
    Some(full)
}

fn remote_home(alias: &str) -> Option<String> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(h) = cache.lock().unwrap().get(alias) {
        return Some(h.clone());
    }
    let out = Command::new("ssh")
        .arg("-o")
        .arg(format!("ConnectTimeout={}", SSH_TIMEOUT))
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(alias)
        .arg("echo $HOME")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        log(&format!(
            "remote_home failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
        return None;
    }
    let home = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if home.is_empty() {
        return None;
    }
    log(&format!("Remote $HOME for {}: {}", alias, home));
    cache.lock().unwrap().insert(alias.to_string(), home.clone());
    Some(home)
}

fn run_ssh(alias: &str, remote_cmd: &str) -> bool {
    log(&format!("ssh {} \"{}\"", alias, remote_cmd));
    Command::new("ssh")
        .arg("-o")
        .arg(format!("ConnectTimeout={}", SSH_TIMEOUT))
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(alias)
        .arg(remote_cmd)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_scp(local: &Path, alias: &str, remote: &str) -> bool {
    // Modern OpenSSH scp uses the SFTP protocol: the remote path is taken
    // literally (NOT shell-expanded), so it must NOT be quoted, or the quotes
    // become part of the filename. Sanitized names never contain spaces.
    let target = format!("{}:{}", alias, remote);
    Command::new("scp")
        .arg("-o")
        .arg(format!("ConnectTimeout={}", SSH_TIMEOUT))
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(local)
        .arg(&target)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
