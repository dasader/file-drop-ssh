//! Tiny INI config (`CursorDrop.ini`, next to the exe). Terminal mode: there is
//! no editor window to inspect, so the SSH alias and remote directory come from
//! here.

use crate::sys;

pub struct Config {
    pub alias: String,
    pub remote_dir: String,
}

const DEFAULT_ALIAS: &str = "myserver";
const DEFAULT_REMOTE_DIR: &str = "~/.cursor-drop-files";

const DEFAULT_INI: &str = "\
; CursorDrop config
;   Alias     = SSH host alias from your ~/.ssh/config (the host Claude Code runs on)
;   RemoteDir = upload target on the remote. '~' is expanded to the remote $HOME.
;               An absolute path (starting with '/') is used as-is.
[Remote]
Alias=myserver
RemoteDir=~/.cursor-drop-files
";

/// Load config, creating a default file on first run.
pub fn load() -> Config {
    let mut alias = DEFAULT_ALIAS.to_string();
    let mut remote_dir = DEFAULT_REMOTE_DIR.to_string();

    let path = sys::config_path();
    match std::fs::read_to_string(path) {
        Ok(text) => {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty()
                    || line.starts_with('#')
                    || line.starts_with(';')
                    || line.starts_with('[')
                {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    let key = k.trim().to_ascii_lowercase();
                    let val = v.trim().to_string();
                    if val.is_empty() {
                        continue;
                    }
                    match key.as_str() {
                        "alias" => alias = val,
                        "remotedir" => remote_dir = val,
                        _ => {}
                    }
                }
            }
        }
        Err(_) => {
            if std::fs::write(path, DEFAULT_INI).is_ok() {
                sys::log("Wrote default CursorDrop.ini");
            }
        }
    }

    sys::log(&format!("Config: alias={} remoteDir={}", alias, remote_dir));
    Config { alias, remote_dir }
}
