# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

CursorDrop is a single-binary **Windows** utility (Rust + `windows-sys`, no GUI
framework — raw Win32). An always-on-top pill widget accepts dropped files or
clipboard paste, SCPs them to a remote host, and copies the resulting **remote
absolute path** to the clipboard. Intended use: paste that path into a remote
Claude Code session running over SSH in a terminal (WezTerm). It does NOT
auto-paste or inspect any editor window — the target host/dir come from
`CursorDrop.ini` next to the exe.

## Build / test

Shipping build runs on Windows (MSVC, static CRT → standalone exe):

```powershell
cargo build --release     # -> target\release\CursorDrop.exe (~340 KB)
cargo test                # unit tests (config parser, util)
cargo test parses_multiple_servers_in_order   # a single test
```

**Verifying on the Linux dev box**: the crate is Windows-only (`sys.rs` uses
`std::os::windows`), so it cannot build for the native Linux target and
`cargo test` does not run there. Instead cross-compile:

```bash
cargo check --target x86_64-pc-windows-gnu               # type-check everything
cargo build --release --target x86_64-pc-windows-gnu     # real PE32+ exe (verification only)
```

This needs `build-essential` + `gcc-mingw-w64-x86-64` (for the `windows_x86_64_gnu`
build script and the mingw linker). To run the pure `config.rs` parser tests on
Linux, extract `parse()` + its `#[cfg(test)]` module into a standalone file and
`rustc --test` it (the rest of the crate won't compile off-Windows).

## Architecture

Single-threaded Win32 message loop on the main thread; **all SSH/scp work runs
on spawned worker threads** so the UI never blocks. Worker threads report
progress back via `set_state()` → `PostMessageW(WM_APP_STATE)`, which the
window proc repaints. State machine: `Idle / Reading / Uploading / Success /
Error`, each with its own color palette; Success/Error auto-revert to Idle on a
timer.

Module responsibilities:

| File | Role |
|------|------|
| `src/main.rs` | Win32 window/WndProc, tray + right-click menu, state machine, input, CLI mode |
| `src/config.rs` | `CursorDrop.ini` parse/default-create → `Vec<Server>` |
| `src/upload.rs` | remote `$HOME` resolve + path calc + clipboard + `ssh`/`scp` (worker thread) |
| `src/clipboard.rs` | clipboard file list / bitmap (GDI+ → PNG) + set text |
| `src/sys.rs` | UTF-16 conversion, timestamps, log/ini/clip paths (Windows-only) |
| `src/util.rs` | pure string logic (shell-quote, filename sanitize) + tests |

**Multi-server flow**: `config::load()` reads all `[Server:<name>]` sections (and
the legacy `[Remote]` section) into `Vec<Server>`, loaded once into the `SERVERS`
`OnceLock` in main.rs. The active server is an `AtomicUsize` (`ACTIVE`), **session-only
— never written back to the ini**; defaults to the first server. The right-click
menu lists servers (checkmark on active) when there's more than one; selecting one
just updates `ACTIVE`. `upload::run(files, &Server)` always uploads to the active
server. CLI mode (`CursorDrop.exe <file>...`) uses the active (= first) server and
exits.

## Gotchas / invariants

- **SSH is `BatchMode=yes`, no console** — passwordless key auth is mandatory
  (passphrase-less key or pre-loaded ssh-agent). A passphrase prompt hangs silently.
- **scp remote path must NOT be quoted**: modern OpenSSH scp uses SFTP and takes
  the remote path literally, so quotes would become part of the filename.
  Filenames are sanitized (no spaces) before use. The *clipboard* payload, by
  contrast, single-quotes each path so it pastes as a literal absolute path.
- `config::parse()` is intentionally **pure (no I/O)** so it stays unit-testable;
  keep it that way. A server is dropped if it has no `Alias`; `RemoteDir` defaults
  when omitted; `load()` guarantees at least one server.
- Remote `$HOME` is queried once per alias and cached (for `~` expansion).
