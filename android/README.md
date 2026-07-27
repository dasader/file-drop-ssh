# CursorDrop for Android (Termux)

A faithful port of the Windows pill widget's flow, driven by the **Android share
sheet** instead of drag-and-drop:

> **Share a file → Termux** ⇒ `scp` it to the active remote server **and** copy
> the resulting **remote absolute path** to the clipboard.

Then paste that path straight into a remote Claude Code session running over SSH
in **Terminus** or **Termux**. Same `CursorDrop.ini` format as the desktop app,
including multi-server `[Server:<name>]` sections — switchable and remembered
between shares.

## What's here

| File | Role |
|------|------|
| `cursor-drop.sh` | the whole thing: INI parse, server select, `~`/`$HOME` resolve, clipboard, `ssh`/`scp` |
| `termux-file-editor` | share/open-a-**file** hook → calls `cursor-drop.sh` with the file path(s) |
| `termux-url-opener` | share-**text/URL** hook → handles a path / `content://` URI on stdin |
| `CursorDrop.ini` | example multi-server config |

## Requirements

- **Termux** and **Termux:API** — install both from **F-Droid** (the Play Store
  builds are outdated). Termux:API provides `termux-clipboard-set`,
  `termux-toast`, `termux-dialog`, `termux-storage-get`.
- Passwordless **SSH key auth** to each server (a passphrase-less key, or one
  pre-loaded in `ssh-agent`). SSH runs with `BatchMode=yes`, so a passphrase
  prompt would just fail — exactly like the desktop app.

## Install

```bash
pkg update && pkg install openssh termux-api
mkdir -p ~/bin ~/.config/cursor-drop

# copy the three scripts (adjust the source path to wherever you cloned this)
cp android/cursor-drop.sh      ~/bin/cursor-drop.sh
cp android/termux-file-editor  ~/bin/termux-file-editor
cp android/termux-url-opener   ~/bin/termux-url-opener
chmod +x ~/bin/cursor-drop.sh ~/bin/termux-file-editor ~/bin/termux-url-opener

# config (edit Alias / RemoteDir to match your ~/.ssh/config)
cp android/CursorDrop.ini ~/.config/cursor-drop/CursorDrop.ini
```

Set up your SSH hosts in `~/.ssh/config` so the `Alias=` values resolve, e.g.:

```
Host myserver
    HostName 203.0.113.10
    User dev
    IdentityFile ~/.ssh/id_ed25519
```

Verify keys work without a prompt: `ssh -o BatchMode=yes myserver true`.

## Use

1. In any app (Files, gallery, a browser's "download → share"), pick a file and
   **Share → Termux**.
2. CursorDrop uploads it to the **active** server and copies the remote path
   (single-quoted, e.g. `'/home/dev/.cursor-drop-files/20260618-143000-0-photo.png'`)
   to the clipboard. A toast confirms it.
3. Long-press → **Paste** into your Claude Code prompt in Terminus/Termux.

### Choosing the server

The active server defaults to the **first** one in the ini and is remembered in
`~/.config/cursor-drop/active`.

```bash
cursor-drop.sh list        # list servers, '*' marks the active one
cursor-drop.sh pick        # radio dialog to choose the active server
cursor-drop.sh use dev     # set active by name
cursor-drop.sh flush       # delete every file in the active server's RemoteDir
```

Want to be asked **on every share** which server to target (a one-shot choice
that doesn't change the saved active)? Set `CURSOR_DROP_PROMPT=1` — either in
your environment or by uncommenting the `export` line in `~/bin/termux-file-editor`.

### From the command line

```bash
cursor-drop.sh ~/storage/downloads/report.pdf      # upload to active server
cursor-drop.sh file1.png file2.png                 # multiple files
```

## Behavior parity with the desktop app

- **Naming**: remote files are `yyyyMMdd-HHmmss-<index>-<sanitized-name>`; the
  timestamp is taken once per batch.
- **Sanitize**: whitespace runs collapse to `_`, and only `[alnum]._-` survive.
- **`RemoteDir`**: a leading `~` expands to the remote `$HOME` (queried once via
  `ssh … echo $HOME`, then cached under `~/.cache/cursor-drop/`); an absolute
  path is used as-is.
- **Paths exist immediately**: `mkdir -p` + `touch` run before `scp`, so the
  pasted path is valid the moment it's on the clipboard.
- **scp quoting**: the remote path is passed **unquoted** (modern OpenSSH scp
  uses SFTP and takes it literally); the clipboard payload single-quotes each
  path so it pastes as a literal absolute path.
- **Multi-server**: `[Server:<name>]` sections + the legacy `[Remote]` section;
  a server with no `Alias` is dropped; `RemoteDir` defaults to
  `~/.cursor-drop-files` when omitted; there is always at least one server.

## Notes / limits

- Whether a shared file arrives via `termux-file-editor` (as a path) or
  `termux-url-opener` (as a `content://` URI on stdin) depends on the source app
  and Android version — both hooks are installed to cover both routes.
- Logs: `~/.config/cursor-drop/cursor-drop.log`.
- This is a separate, self-contained implementation; it does not build from or
  depend on the Rust crate.
