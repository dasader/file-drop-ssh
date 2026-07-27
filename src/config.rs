//! Tiny INI config (`CursorDrop.ini`, next to the exe). Terminal mode: there is
//! no editor window to inspect, so the SSH alias and remote directory come from
//! here. Multiple servers are listed as `[Server:<name>]` sections; the
//! right-click menu switches the active one (session-only, never written back).

use crate::sys;

#[derive(Clone)]
pub struct Server {
    pub name: String,
    pub alias: String,
    pub remote_dir: String,
}

const DEFAULT_REMOTE_DIR: &str = "~/.cursor-drop-files";

const DEFAULT_INI: &str = "\
; CursorDrop config — list servers as [Server:<name>] sections.
;   Alias     = SSH host alias from your ~/.ssh/config (the host Claude Code runs on)
;   RemoteDir = upload target on the remote. '~' expands to the remote $HOME;
;               an absolute path (starting with '/') is used as-is.
; Right-click the widget to switch the active server. The first server listed is
; the default active one.
[Server:prod]
Alias=myserver
RemoteDir=~/.cursor-drop-files

;[Server:dev]
;Alias=devbox
;RemoteDir=~/uploads
";

/// Load all configured servers, creating a default file on first run.
/// Always returns at least one server.
pub fn load() -> Vec<Server> {
    let path = sys::config_path();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => {
            if std::fs::write(path, DEFAULT_INI).is_ok() {
                sys::log("Wrote default CursorDrop.ini");
            }
            DEFAULT_INI.to_string()
        }
    };

    // A garbage ini falls back to what the default one would have produced
    // (`default_ini_has_one_server` keeps that non-empty).
    let mut servers = parse(&text);
    if servers.is_empty() {
        servers = parse(DEFAULT_INI);
    }

    for s in &servers {
        sys::log(&format!(
            "Config server: name={} alias={} remoteDir={}",
            s.name, s.alias, s.remote_dir
        ));
    }
    servers
}

/// Parse INI text into a list of servers. Pure (no I/O) so it is unit-testable.
///
/// A new server starts at a `[Server:<name>]` header (name = `<name>`); other
/// section headers are ignored. `Alias` / `RemoteDir` keys apply to the server
/// currently being built. A server is only kept if it has a non-empty `Alias`;
/// its `RemoteDir` falls back to the default when omitted.
fn parse(text: &str) -> Vec<Server> {
    let mut servers: Vec<Server> = Vec::new();
    // (name, alias, remote_dir) for the server currently being built.
    let mut cur: Option<(String, String, String)> = None;

    let flush = |cur: &mut Option<(String, String, String)>, out: &mut Vec<Server>| {
        if let Some((name, alias, remote_dir)) = cur.take() {
            if !alias.is_empty() {
                let remote_dir = if remote_dir.is_empty() {
                    DEFAULT_REMOTE_DIR.to_string()
                } else {
                    remote_dir
                };
                out.push(Server { name, alias, remote_dir });
            }
        }
    };

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            let section = section.trim();
            flush(&mut cur, &mut servers);
            if let Some(name) = section.strip_prefix("Server:") {
                let name = name.trim();
                if !name.is_empty() {
                    cur = Some((name.to_string(), String::new(), String::new()));
                }
            }
            // Unknown sections leave `cur` as None, so their keys are ignored.
            continue;
        }
        if let Some((_, alias, remote_dir)) = cur.as_mut() {
            if let Some((k, v)) = line.split_once('=') {
                let key = k.trim().to_ascii_lowercase();
                let val = v.trim();
                if val.is_empty() {
                    continue;
                }
                match key.as_str() {
                    "alias" => *alias = val.to_string(),
                    "remotedir" => *remote_dir = val.to_string(),
                    _ => {}
                }
            }
        }
    }
    flush(&mut cur, &mut servers);
    servers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_servers_in_order() {
        let ini = "\
[Server:prod]
Alias=myserver
RemoteDir=~/.cursor-drop-files

[Server:dev]
Alias=devbox
RemoteDir=~/uploads
";
        let s = parse(ini);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "prod");
        assert_eq!(s[0].alias, "myserver");
        assert_eq!(s[0].remote_dir, "~/.cursor-drop-files");
        assert_eq!(s[1].name, "dev");
        assert_eq!(s[1].alias, "devbox");
        assert_eq!(s[1].remote_dir, "~/uploads");
    }

    #[test]
    fn remote_dir_defaults_when_omitted() {
        let s = parse("[Server:x]\nAlias=h\n");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].remote_dir, DEFAULT_REMOTE_DIR);
    }

    #[test]
    fn server_without_alias_is_dropped() {
        let s = parse("[Server:x]\nRemoteDir=~/foo\n");
        assert!(s.is_empty());
    }

    /// `load()` relies on this: a garbage ini falls back to `parse(DEFAULT_INI)`,
    /// and callers index `servers()[0]`.
    #[test]
    fn default_ini_has_one_server() {
        let s = parse(DEFAULT_INI);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "prod");
        assert_eq!(s[0].remote_dir, DEFAULT_REMOTE_DIR);
    }

    #[test]
    fn empty_or_garbage_yields_no_servers() {
        assert!(parse("").is_empty());
        assert!(parse("; just a comment\n[Other]\nkey=val\n").is_empty());
        // The legacy [Remote] section is no longer recognized.
        assert!(parse("[Remote]\nAlias=myserver\n").is_empty());
    }

    #[test]
    fn comments_and_blank_lines_ignored() {
        let ini = "\
; header comment
[Server:a]
# hash comment
Alias=ha

;[Server:commented]
;Alias=nope
";
        let s = parse(ini);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "a");
        assert_eq!(s[0].alias, "ha");
    }
}
