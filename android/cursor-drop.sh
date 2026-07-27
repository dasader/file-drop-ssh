#!/data/data/com.termux/files/usr/bin/bash
#
# CursorDrop for Android (Termux) — the same flow as the Windows pill widget,
# driven by the Android share sheet instead of drag-and-drop.
#
#   share a file -> Termux  ==>  scp to the active remote server
#                                + copy the remote absolute path to the clipboard
#
# Paste that path straight into a remote Claude Code session running over SSH in
# Terminus / Termux. Multiple servers are read from CursorDrop.ini exactly like
# the desktop app ([Server:<name>] sections + the legacy [Remote] section); the
# "active" one is switchable and remembered between runs.
#
# Subcommands:
#   cursor-drop.sh <file> [file...]   upload file(s) to the active server (default)
#   cursor-drop.sh upload <file>...    same, explicit
#   cursor-drop.sh list                list servers (marks the active one)
#   cursor-drop.sh use <name>          set the active server by name
#   cursor-drop.sh pick                pop a radio dialog to choose the active server
#   cursor-drop.sh flush               delete every file in the active server's RemoteDir
#
# Env:
#   CURSOR_DROP_INI     override ini path (default ~/.config/cursor-drop/CursorDrop.ini)
#   CURSOR_DROP_PROMPT  =1 -> when >1 server, ask which to use for THIS upload
#                            (a one-shot choice; does not change the saved active)

set -u

# ---------------------------------------------------------------------------
# Paths / constants
# ---------------------------------------------------------------------------
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/cursor-drop"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/cursor-drop"
INI="${CURSOR_DROP_INI:-$CONFIG_DIR/CursorDrop.ini}"
ACTIVE_FILE="$CONFIG_DIR/active"
LOG="$CONFIG_DIR/cursor-drop.log"

SSH_TIMEOUT=30
SSH_OPTS=(-o "ConnectTimeout=$SSH_TIMEOUT" -o BatchMode=yes)

DEFAULT_REMOTE_DIR="~/.cursor-drop-files"

DEFAULT_INI='; CursorDrop config — list servers as [Server:<name>] sections.
;   Alias     = SSH host alias from your ~/.ssh/config (the host Claude Code runs on)
;   RemoteDir = upload target on the remote. '"'"'~'"'"' expands to the remote $HOME;
;               an absolute path (starting with '"'"'/'"'"') is used as-is.
; The first server listed is the default active one. Switch with `cursor-drop.sh pick`.
; (The legacy [Remote] section is still recognized.)
[Server:prod]
Alias=myserver
RemoteDir=~/.cursor-drop-files

;[Server:dev]
;Alias=devbox
;RemoteDir=~/uploads
'

# Parsed servers (parallel arrays, same order as the ini).
SRV_NAMES=()
SRV_ALIASES=()
SRV_DIRS=()

# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------
log() { printf '%s %s\n' "$(date +%Y%m%d-%H%M%S)" "$*" >>"$LOG" 2>/dev/null; }

# User-facing status: Android toast if available, else stderr.
notify() {
    if command -v termux-toast >/dev/null 2>&1; then
        termux-toast -g top "$*" 2>/dev/null
    fi
    printf '%s\n' "$*" >&2
}

die() { notify "CursorDrop: $*"; log "ERROR: $*"; exit 1; }

# shell_quote: wrap in double quotes, escaping embedded quotes — for the *remote*
# portion of ssh commands (mirrors util::shell_quote).
shell_quote() { printf '"%s"' "${1//\"/\\\"}"; }

# sanitize_filename: collapse whitespace runs to a single '_', then keep only
# alphanumerics, '.', '_', '-' (mirrors util::sanitize_filename; [:alnum:] keeps
# Unicode letters under a UTF-8 locale).
sanitize_filename() {
    printf '%s' "$1" | sed -E 's/[[:space:]]+/_/g' | sed -E 's/[^[:alnum:]._-]//g'
}

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------
ensure_ini() {
    mkdir -p "$CONFIG_DIR" "$CACHE_DIR" 2>/dev/null
    if [ ! -f "$INI" ]; then
        printf '%s' "$DEFAULT_INI" >"$INI" || die "cannot write $INI"
        log "Wrote default CursorDrop.ini"
    fi
}

# parse_ini: fill SRV_* arrays from $INI. Mirrors config::parse exactly —
# [Server:<name>] / [Remote] headers start a server; Alias/RemoteDir (case-
# insensitive keys) apply to the current one; a server is kept only if it has a
# non-empty Alias; RemoteDir defaults when omitted; unknown sections are ignored.
parse_ini() {
    SRV_NAMES=(); SRV_ALIASES=(); SRV_DIRS=()
    local cur_name="" cur_alias="" cur_dir="" have_cur=0

    flush() {
        if [ "$have_cur" = 1 ] && [ -n "$cur_alias" ]; then
            SRV_NAMES+=("$cur_name")
            SRV_ALIASES+=("$cur_alias")
            SRV_DIRS+=("${cur_dir:-$DEFAULT_REMOTE_DIR}")
        fi
        have_cur=0; cur_name=""; cur_alias=""; cur_dir=""
    }

    local line key val section
    while IFS= read -r line || [ -n "$line" ]; do
        # trim leading/trailing whitespace
        line="${line#"${line%%[![:space:]]*}"}"
        line="${line%"${line##*[![:space:]]}"}"
        [ -z "$line" ] && continue
        case "$line" in \#*|';'*) continue ;; esac

        if [[ "$line" == \[*\] ]]; then
            section="${line#[}"; section="${section%]}"
            section="${section#"${section%%[![:space:]]*}"}"
            section="${section%"${section##*[![:space:]]}"}"
            flush
            if [[ "$section" == Server:* ]]; then
                cur_name="${section#Server:}"
                cur_name="${cur_name#"${cur_name%%[![:space:]]*}"}"
                cur_name="${cur_name%"${cur_name##*[![:space:]]}"}"
                [ -n "$cur_name" ] && have_cur=1
            elif [[ "${section,,}" == "remote" ]]; then
                cur_name="Remote"; have_cur=1
            fi
            continue
        fi

        [ "$have_cur" = 1 ] || continue
        [[ "$line" == *=* ]] || continue
        key="${line%%=*}"; val="${line#*=}"
        key="${key%"${key##*[![:space:]]}"}"; key="${key,,}"
        val="${val#"${val%%[![:space:]]*}"}"; val="${val%"${val##*[![:space:]]}"}"
        [ -z "$val" ] && continue
        case "$key" in
            alias) cur_alias="$val" ;;
            remotedir) cur_dir="$val" ;;
        esac
    done <"$INI"
    flush

    # load() guarantees at least one server.
    if [ "${#SRV_NAMES[@]}" -eq 0 ]; then
        SRV_NAMES=("prod"); SRV_ALIASES=("myserver"); SRV_DIRS=("$DEFAULT_REMOTE_DIR")
    fi
}

# Index of the active server (defaults to 0 = first). The active NAME is
# persisted in $ACTIVE_FILE so it survives between shares (the desktop app keeps
# it session-only, but a phone has no long-running process to hold it).
active_index() {
    local want="" i
    [ -f "$ACTIVE_FILE" ] && want="$(cat "$ACTIVE_FILE" 2>/dev/null)"
    if [ -n "$want" ]; then
        for i in "${!SRV_NAMES[@]}"; do
            [ "${SRV_NAMES[$i]}" = "$want" ] && { printf '%s' "$i"; return; }
        done
    fi
    printf '0'
}

set_active() {
    local name="$1" i
    for i in "${!SRV_NAMES[@]}"; do
        if [ "${SRV_NAMES[$i]}" = "$name" ]; then
            printf '%s' "$name" >"$ACTIVE_FILE"
            log "Active server -> $name"
            return 0
        fi
    done
    return 1
}

# ---------------------------------------------------------------------------
# Remote path resolution (mirrors upload::resolve_remote_dir / remote_home)
# ---------------------------------------------------------------------------
remote_home() {
    local alias="$1" cache="$CACHE_DIR/home-$1" home
    if [ -f "$cache" ]; then cat "$cache"; return 0; fi
    home="$(ssh "${SSH_OPTS[@]}" "$alias" 'echo $HOME' </dev/null 2>>"$LOG")"
    home="${home//$'\n'/}"; home="${home//$'\r'/}"
    [ -z "$home" ] && { log "remote_home failed for $alias"; return 1; }
    printf '%s' "$home" >"$cache"
    log "Remote \$HOME for $alias: $home"
    printf '%s' "$home"
}

resolve_remote_dir() {
    local alias="$1" dir="$2" home rest
    if [[ "$dir" == /* ]]; then
        printf '%s' "${dir%/}"; return 0
    fi
    home="$(remote_home "$alias")" || return 1
    rest="${dir#\~}"; rest="${rest#/}"
    home="${home%/}"
    if [ -n "$rest" ]; then printf '%s/%s' "$home" "$rest"; else printf '%s' "$home"; fi
}

# ---------------------------------------------------------------------------
# Upload (mirrors upload::run)
# ---------------------------------------------------------------------------
do_upload() {
    local idx="$1"; shift
    local name="${SRV_NAMES[$idx]}" alias="${SRV_ALIASES[$idx]}" dir="${SRV_DIRS[$idx]}"

    # keep only files that exist
    local files=() f
    for f in "$@"; do [ -f "$f" ] && files+=("$f"); done
    [ "${#files[@]}" -eq 0 ] && die "No files"

    log "Upload target: server '$name' alias=$alias"
    notify "CursorDrop -> $name: uploading ${#files[@]} file(s)…"

    local remote_dir
    remote_dir="$(resolve_remote_dir "$alias" "$dir")" || die "Remote unreachable ($alias)"

    local ts; ts="$(date +%Y%m%d-%H%M%S)"
    local remote_files=() i=0 base
    for f in "${files[@]}"; do
        base="$(basename -- "$f")"
        remote_files+=("$remote_dir/$ts-$i-$(sanitize_filename "$base")")
        i=$((i + 1))
    done

    # Clipboard: single-quote each remote path so it pastes as literal path(s).
    local payload=""
    for f in "${remote_files[@]}"; do payload+="'$f' "; done
    payload="${payload% }"
    if command -v termux-clipboard-set >/dev/null 2>&1; then
        printf '%s' "$payload" | termux-clipboard-set
    else
        log "termux-clipboard-set missing (install Termux:API); path: $payload"
    fi
    log "Clipboard set: $payload"

    # mkdir + touch so the paths exist immediately (before scp finishes).
    local remote_cmd; remote_cmd="mkdir -p $(shell_quote "$remote_dir") && touch"
    for f in "${remote_files[@]}"; do remote_cmd+=" $(shell_quote "$f")"; done
    if ! ssh "${SSH_OPTS[@]}" "$alias" "$remote_cmd" </dev/null >>"$LOG" 2>&1; then
        die "Remote prep failed ($alias)"
    fi

    # scp each. Remote path is NOT quoted (modern scp uses SFTP, takes it
    # literally); sanitized names never contain spaces.
    local total="${#files[@]}" fails=0
    for i in "${!files[@]}"; do
        if scp "${SSH_OPTS[@]}" -- "${files[$i]}" "$alias:${remote_files[$i]}" </dev/null >>"$LOG" 2>&1; then
            log "OK: ${files[$i]} -> ${remote_files[$i]}"
        else
            log "FAIL scp: ${files[$i]}"
            fails=$((fails + 1))
        fi
    done

    if [ "$fails" -gt 0 ]; then
        die "$fails upload(s) failed"
    fi
    local s=""; [ "$total" -gt 1 ] && s="s"
    notify "CursorDrop: $total file$s ready ($name) — path copied"
    log "Done: $total file(s) to $name"
}

# ---------------------------------------------------------------------------
# Flush (mirrors upload::flush) — wipe the server's remote dir, top level only
# ---------------------------------------------------------------------------
cmd_flush() {
    local idx="$1"
    local name="${SRV_NAMES[$idx]}" alias="${SRV_ALIASES[$idx]}" dir="${SRV_DIRS[$idx]}"

    local remote_dir home out
    remote_dir="$(resolve_remote_dir "$alias" "$dir")" || die "Remote unreachable ($alias)"
    home="$(remote_home "$alias" 2>/dev/null)"
    # Never let a stray config turn this into `rm -f /*` or wipe $HOME.
    if [ "${#remote_dir}" -lt 2 ] || [ "$remote_dir" = "$home" ]; then
        die "Unsafe RemoteDir: $remote_dir"
    fi

    # Deleting is not undoable — confirm when a dialog is available (a bare CLI
    # run has no share-sheet misfire to guard against).
    if command -v termux-dialog >/dev/null 2>&1; then
        out="$(termux-dialog confirm -t "CursorDrop" \
            -i "Delete all files in $alias:$remote_dir?" 2>/dev/null)"
        case "$out" in *'"yes"'*) ;; *) notify "Flush cancelled"; return ;; esac
    fi

    # Quotes cover the dir; the glob stays outside so the remote shell expands it.
    if ssh "${SSH_OPTS[@]}" "$alias" "rm -f $(shell_quote "$remote_dir")/*" \
        </dev/null >>"$LOG" 2>&1; then
        notify "CursorDrop: flushed $name"
        log "Flushed $alias:$remote_dir"
    else
        die "Flush failed ($alias)"
    fi
}

# ---------------------------------------------------------------------------
# Server selection UI
# ---------------------------------------------------------------------------
cmd_list() {
    local active i mark
    active="$(active_index)"
    for i in "${!SRV_NAMES[@]}"; do
        mark="  "; [ "$i" = "$active" ] && mark="* "
        printf '%s%s\t(%s)\t%s\n' "$mark" "${SRV_NAMES[$i]}" "${SRV_ALIASES[$i]}" "${SRV_DIRS[$i]}"
    done
}

# Pop a radio dialog (Termux:API) and return the chosen index on stdout, or
# nothing if cancelled / no dialog available.
pick_dialog() {
    command -v termux-dialog >/dev/null 2>&1 || return 1
    local values out chosen i
    values="$(IFS=,; printf '%s' "${SRV_NAMES[*]}")"
    out="$(termux-dialog radio -t "CursorDrop: target server" -v "$values" 2>/dev/null)"
    # termux-dialog returns JSON: {"code":..,"text":"name","index":N}
    chosen="$(printf '%s' "$out" | sed -n 's/.*"text"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
    [ -z "$chosen" ] && return 1
    for i in "${!SRV_NAMES[@]}"; do
        [ "${SRV_NAMES[$i]}" = "$chosen" ] && { printf '%s' "$i"; return 0; }
    done
    return 1
}

cmd_pick() {
    if [ "${#SRV_NAMES[@]}" -le 1 ]; then notify "Only one server: ${SRV_NAMES[0]}"; return; fi
    local idx
    if idx="$(pick_dialog)"; then
        set_active "${SRV_NAMES[$idx]}"
        notify "Active server -> ${SRV_NAMES[$idx]}"
    else
        notify "No change"
    fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
ensure_ini
parse_ini

cmd="${1:-}"
case "$cmd" in
    list)   cmd_list; exit 0 ;;
    pick)   cmd_pick; exit 0 ;;
    flush)  cmd_flush "$(active_index)"; exit 0 ;;
    use)
        [ -n "${2:-}" ] || die "usage: cursor-drop.sh use <name>"
        set_active "$2" && notify "Active server -> $2" || die "no such server: $2"
        exit 0 ;;
    upload) shift ;;        # explicit upload; fall through with remaining args
    "" )    die "no files given" ;;
    *)      ;;              # default: treat all args as files
esac

[ "$#" -ge 1 ] || die "no files given"

idx="$(active_index)"
# One-shot per-upload target choice when asked for and >1 server.
if [ "${CURSOR_DROP_PROMPT:-0}" = "1" ] && [ "${#SRV_NAMES[@]}" -gt 1 ]; then
    if alt="$(pick_dialog)"; then idx="$alt"; fi
fi

do_upload "$idx" "$@"
