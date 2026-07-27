//! Small Win32 / OS helpers shared across modules.

use std::ffi::OsStr;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

/// Convert a Rust string to a NUL-terminated UTF-16 buffer for the `W` APIs.
pub fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Convert a NUL-terminated (or full) UTF-16 slice to a Rust String.
pub fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    std::ffi::OsString::from_wide(&buf[..len])
        .to_string_lossy()
        .into_owned()
}

/// `yyyyMMdd-HHmmss` local timestamp, matching the AHK naming scheme.
pub fn now_stamp() -> String {
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `<exe dir>\CursorDrop.log`, restarted once per run if it grew past ~1 MB
/// (nothing else ever prunes it and every upload writes several lines).
pub fn log_path() -> &'static PathBuf {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| {
        let p = exe_dir().join("CursorDrop.log");
        if std::fs::metadata(&p).map(|m| m.len() > 1_000_000).unwrap_or(false) {
            let _ = std::fs::remove_file(&p);
        }
        p
    })
}

/// `<exe dir>\CursorDrop.ini`
pub fn config_path() -> &'static PathBuf {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| exe_dir().join("CursorDrop.ini"))
}

/// `%TEMP%\CursorDrop_clips`, created on first use.
pub fn clip_dir() -> &'static PathBuf {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| {
        let d = std::env::temp_dir().join("CursorDrop_clips");
        let _ = std::fs::create_dir_all(&d);
        // Every pasted image lands here and nothing else prunes it; a
        // screenshot is megabytes, so drop day-old ones once per run.
        if let Ok(entries) = std::fs::read_dir(&d) {
            for e in entries.flatten() {
                let age = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok());
                if age.is_some_and(|a| a.as_secs() > 24 * 60 * 60) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        d
    })
}

/// Append a timestamped line to the log file (best effort).
pub fn log(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        let _ = writeln!(f, "{} {}", now_stamp(), msg);
    }
}
