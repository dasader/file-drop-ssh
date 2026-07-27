//! CursorDrop — drag files / paste clipboard images, SCP them to the remote
//! host, and copy the resulting remote absolute paths to the clipboard for
//! pasting into a remote session (e.g. Claude Code over SSH in a terminal).

#![windows_subsystem = "windows"]

mod clipboard;
mod config;
mod sys;
mod upload;
mod util;

use core::ffi::c_void;
use core::mem::{size_of, zeroed};
use core::ptr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use config::Server;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateRoundRectRgn, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect,
    GetStockObject, InvalidateRect, RoundRect, SelectObject, SetBkMode, SetTextColor, SetWindowRgn,
    DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, HDC, HFONT, NULL_BRUSH, PAINTSTRUCT,
    PS_SOLID, SRCCOPY,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD,
};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, VK_CONTROL, VK_ESCAPE, VK_V,
};
use windows_sys::Win32::UI::Shell::{DragAcceptFiles, DragFinish, HDROP};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetClientRect, GetCursorPos, GetSystemMetrics, KillTimer, LoadCursorW,
    PostMessageW, PostQuitMessage, RegisterClassExW, SendMessageW,
    SetForegroundWindow, SetTimer, SetWindowPos, TrackPopupMenu,
    CS_HREDRAW, CS_VREDRAW, GetMessageW, DispatchMessageW, TranslateMessage, HWND_TOPMOST,
    MessageBoxW, IDC_ARROW, IDYES, MB_ICONWARNING, MB_YESNO, MF_CHECKED, MF_SEPARATOR,
    MF_STRING, MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_DESTROY, WM_DPICHANGED, WM_DROPFILES,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_NCLBUTTONDOWN, WM_PAINT,
    WM_RBUTTONUP, WM_TIMER, WNDCLASSEXW, WS_EX_ACCEPTFILES, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, HTCAPTION,
};

// Layout in 96-dpi units; everything on screen goes through px().
const PILL_W: i32 = 216;
const PILL_H: i32 = 64;
const RADIUS: i32 = 16;
const PAD: i32 = 16;
const RAIL_W: i32 = 3;
const RAIL_H: i32 = 30;
const TEXT_X: i32 = 34;
const TITLE_H: i32 = 20;
const SUB_H: i32 = 18;

const WM_APP_STATE: u32 = WM_APP + 1;
const TIMER_REVERT: usize = 1;

const ID_PASTE: usize = 101;
const ID_LOG: usize = 102;
const ID_EXIT: usize = 103;
const ID_FLUSH: usize = 104;
/// Server menu items get command IDs `ID_SERVER_BASE + index`.
const ID_SERVER_BASE: usize = 200;

#[derive(Clone, Copy)]
pub enum StateKind {
    Idle,
    Uploading,
    Success,
    Error,
}

struct UiState {
    kind: StateKind,
    title: String,
    sub: String,
    /// 0.0 = no bar; otherwise the fraction of the transfer that is done.
    progress: f32,
}

static STATE: Mutex<UiState> = Mutex::new(UiState {
    kind: StateKind::Idle,
    title: String::new(),
    sub: String::new(),
    progress: 0.0,
});
static HWND_MAIN: AtomicIsize = AtomicIsize::new(0);
static DARK: AtomicBool = AtomicBool::new(true);
/// Monitor dpi the window is currently on; 96 until the window exists.
static DPI: AtomicI32 = AtomicI32::new(96);

/// A 96-dpi design unit in real pixels.
fn px(v: i32) -> i32 {
    v * DPI.load(Ordering::Relaxed) / 96
}

// Configured servers (loaded once at startup) and the active selection.
// The active index is session-only — never written back to the ini.
static SERVERS: OnceLock<Vec<Server>> = OnceLock::new();
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

fn servers() -> &'static [Server] {
    SERVERS.get_or_init(config::load)
}

/// The currently active server, clamped to a valid index.
fn active_server() -> Server {
    let list = servers();
    let idx = ACTIVE.load(Ordering::Relaxed).min(list.len() - 1);
    list[idx].clone()
}

// ---------------------------------------------------------------------------
// State (callable from worker threads)
// ---------------------------------------------------------------------------
pub fn set_state(kind: StateKind, title: &str, sub: &str) {
    {
        let mut s = STATE.lock().unwrap();
        s.kind = kind;
        s.title = title.to_string();
        s.sub = sub.to_string();
        // A running transfer keeps its bar across the per-file label changes;
        // any other state ends it.
        if !matches!(kind, StateKind::Uploading) {
            s.progress = 0.0;
        }
    }
    repaint();
}

/// How far along the current transfer is (drives the bar at the bottom edge).
pub fn set_progress(done: usize, total: usize) {
    if total > 0 {
        STATE.lock().unwrap().progress = done as f32 / total as f32;
        repaint();
    }
}

fn repaint() {
    let h = HWND_MAIN.load(Ordering::SeqCst);
    if h != 0 {
        unsafe { PostMessageW(h as *mut c_void, WM_APP_STATE, 0, 0) };
    }
}

// ---------------------------------------------------------------------------
// Clipboard paste orchestration
// ---------------------------------------------------------------------------
fn do_paste() {
    let files = clipboard::read_clipboard_files();
    if !files.is_empty() {
        upload::handle_files(files, active_server());
        return;
    }
    if clipboard::clipboard_has_bitmap() {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let out = sys::clip_dir().join(format!("clip-{}-{}.png", sys::now_stamp(), n));
        if clipboard::save_clipboard_bitmap_png(&out) {
            sys::log(&format!("Bitmap saved: {}", out.display()));
            upload::handle_files(vec![out], active_server());
        } else {
            set_state(StateKind::Error, "Image not saved", "Could not write the PNG");
        }
        return;
    }
    set_state(StateKind::Error, "Nothing to paste", "No files or image on the clipboard");
}

// ---------------------------------------------------------------------------
// Drawing
//
// One surface colour, one type scale, and a single accent that carries the
// state: an upright rail beside the text, plus a bar along the bottom edge
// while files are moving. Everything is a rectangle, so it stays crisp at any
// dpi — GDI cannot antialias a shape, and a jagged circle is what "cheap" looks
// like.
// ---------------------------------------------------------------------------
type Rgb = (u8, u8, u8);

fn rgb((r, g, b): Rgb) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

struct Pal {
    surface: Rgb,
    border: Rgb,
    title: Rgb,
    sub: Rgb,
    accent: Rgb,
}

fn palette(kind: StateKind, dark: bool) -> Pal {
    use StateKind::*;
    let accent = if dark {
        match kind {
            Idle => (0x55, 0x60, 0x70),
            Uploading => (0xE0, 0xA6, 0x4A),
            Success => (0x58, 0xC9, 0x8B),
            Error => (0xE4, 0x68, 0x5E),
        }
    } else {
        match kind {
            Idle => (0x9A, 0xA2, 0xAF),
            Uploading => (0xC8, 0x8A, 0x1E),
            Success => (0x2F, 0xA0, 0x66),
            Error => (0xD0, 0x4A, 0x40),
        }
    };
    if dark {
        Pal {
            surface: (0x1A, 0x1A, 0x1F),
            border: (0x30, 0x30, 0x3A),
            title: (0xF1, 0xF0, 0xF4),
            sub: (0x8C, 0x89, 0x96),
            accent,
        }
    } else {
        Pal {
            surface: (0xFF, 0xFF, 0xFF),
            border: (0xDE, 0xDC, 0xE2),
            title: (0x16, 0x15, 0x1A),
            sub: (0x74, 0x71, 0x7C),
            accent,
        }
    }
}

fn current_visual() -> (Pal, String, String, f32) {
    let (kind, title, sub, progress) = {
        let s = STATE.lock().unwrap();
        (s.kind, s.title.clone(), s.sub.clone(), s.progress)
    };
    let pal = palette(kind, DARK.load(Ordering::Relaxed));
    // Idle is the one state with nothing to report, so it speaks for itself.
    if title.is_empty() {
        return (pal, "Drop or paste".into(), "Ctrl+V or drag files here".into(), 0.0);
    }
    (pal, title, sub, progress)
}

unsafe fn make_font(height: i32, weight: i32) -> HFONT {
    let face = sys::wide("Segoe UI");
    CreateFontW(
        height, 0, 0, 0, weight, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr(),
    )
}

unsafe fn fill(hdc: HDC, x: i32, y: i32, w: i32, h: i32, color: Rgb) {
    let brush = CreateSolidBrush(rgb(color));
    let rc = RECT { left: x, top: y, right: x + w, bottom: y + h };
    FillRect(hdc, &rc, brush);
    DeleteObject(brush);
}

/// One line, vertically centred in its slot, ellipsized rather than clipped —
/// remote file names are long and the widget is not.
unsafe fn draw_line(
    hdc: HDC,
    text: &str,
    (left, top, right, height): (i32, i32, i32, i32),
    size: i32,
    weight: i32,
    color: Rgb,
) {
    let font = make_font(-size, weight);
    let old = SelectObject(hdc, font);
    SetTextColor(hdc, rgb(color));
    let mut rc = RECT { left, top, right, bottom: top + height };
    let w = sys::wide(text);
    DrawTextW(
        hdc,
        w.as_ptr(),
        -1,
        &mut rc,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
    );
    SelectObject(hdc, old);
    DeleteObject(font);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = zeroed();
    let screen = BeginPaint(hwnd, &mut ps);

    let mut rc: RECT = zeroed();
    GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right, rc.bottom);
    let (pal, title, sub, progress) = current_visual();

    // Draw off-screen and blit once: repainting the surface under the text in
    // place makes the label flash on every progress update.
    let hdc = CreateCompatibleDC(screen);
    let bmp = CreateCompatibleBitmap(screen, w, h);
    let old_bmp = SelectObject(hdc, bmp);

    fill(hdc, 0, 0, w, h, pal.surface);

    // Hairline edge, following the window region.
    let pen = CreatePen(PS_SOLID, px(1).max(1), rgb(pal.border));
    let old_pen = SelectObject(hdc, pen);
    let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
    RoundRect(hdc, 0, 0, w, h, px(RADIUS) * 2, px(RADIUS) * 2);
    SelectObject(hdc, old_pen);
    SelectObject(hdc, old_brush);
    DeleteObject(pen);

    // State rail.
    let rail_h = px(RAIL_H);
    fill(hdc, px(PAD), (h - rail_h) / 2, px(RAIL_W), rail_h, pal.accent);

    SetBkMode(hdc, 1); // TRANSPARENT
    let (left, right) = (px(TEXT_X), w - px(PAD));
    let top = (h - px(TITLE_H + SUB_H)) / 2;
    draw_line(hdc, &title, (left, top, right, px(TITLE_H)), px(15), 600, pal.title);
    let sub_top = top + px(TITLE_H);
    draw_line(hdc, &sub, (left, sub_top, right, px(SUB_H)), px(11), 400, pal.sub);

    // Progress hugs the bottom edge, inset past the corner arcs.
    if progress > 0.0 {
        let track = w - px(PAD) * 2;
        let done = (track as f32 * progress.min(1.0)) as i32;
        fill(hdc, px(PAD), h - px(11), done, px(3), pal.accent);
    }

    BitBlt(screen, 0, 0, w, h, hdc, 0, 0, SRCCOPY);
    SelectObject(hdc, old_bmp);
    DeleteObject(bmp);
    DeleteDC(hdc);
    EndPaint(hwnd, &ps);
}

// ---------------------------------------------------------------------------
// Menu
// ---------------------------------------------------------------------------
unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();

    let item = |flags: u32, id: usize, text: &str| {
        AppendMenuW(menu, flags, id, sys::wide(text).as_ptr());
    };
    let sep = || AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());

    // Server switching is session-only — the choice is never written back to the ini.
    let list = servers();
    if list.len() > 1 {
        let active = ACTIVE.load(Ordering::Relaxed).min(list.len() - 1);
        for (i, s) in list.iter().enumerate() {
            item(MF_STRING | (if i == active { MF_CHECKED } else { 0 }), ID_SERVER_BASE + i, &s.name);
        }
        sep();
    }

    item(MF_STRING, ID_PASTE, "Paste clipboard");
    item(MF_STRING, ID_FLUSH, "Flush remote files");
    sep();
    item(MF_STRING, ID_LOG, "Show log");
    sep();
    item(MF_STRING, ID_EXIT, "Exit");

    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd); // so the menu closes on outside click
    let cmd = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON,
        pt.x,
        pt.y,
        0,
        hwnd,
        ptr::null(),
    );
    DestroyMenu(menu);

    let cmd = cmd as usize;
    if cmd >= ID_SERVER_BASE && cmd < ID_SERVER_BASE + list.len() {
        let idx = cmd - ID_SERVER_BASE;
        ACTIVE.store(idx, Ordering::Relaxed);
        sys::log(&format!("Active server: {}", list[idx].name));
        return;
    }
    match cmd {
        ID_PASTE => do_paste(),
        ID_FLUSH => {
            // Deleting remote files is not undoable — always confirm.
            let s = active_server();
            let text = sys::wide(&format!(
                "Delete all files in {}:{} ?\n\nThis cannot be undone.",
                s.alias, s.remote_dir
            ));
            let title = sys::wide("CursorDrop — flush remote files");
            let answer = MessageBoxW(
                hwnd,
                text.as_ptr(),
                title.as_ptr(),
                MB_YESNO | MB_ICONWARNING,
            );
            if answer == IDYES {
                upload::flush(s);
            }
        }
        ID_LOG => {
            let _ = std::process::Command::new("notepad")
                .arg(sys::log_path())
                .spawn();
        }
        ID_EXIT => {
            DestroyWindow(hwnd);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Drop handling
// ---------------------------------------------------------------------------
unsafe fn handle_drop(wparam: WPARAM) {
    let hdrop = wparam as HDROP;
    let files = clipboard::drop_paths(hdrop);
    DragFinish(hdrop);

    if files.is_empty() {
        return;
    }
    upload::handle_files(files, active_server());
}

// ---------------------------------------------------------------------------
// Window procedure
// ---------------------------------------------------------------------------
unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_LBUTTONDOWN => {
            // Drag the borderless window by its body.
            ReleaseCapture();
            SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0);
            0
        }
        WM_RBUTTONUP => {
            show_menu(hwnd);
            0
        }
        WM_KEYDOWN => {
            if wparam == VK_ESCAPE as usize {
                DestroyWindow(hwnd);
            } else if wparam == VK_V as usize
                && ((GetKeyState(VK_CONTROL as i32) as i32) & 0x8000) != 0
            {
                do_paste();
            }
            0
        }
        WM_DROPFILES => {
            handle_drop(wparam);
            0
        }
        WM_DPICHANGED => {
            let dpi = (wparam & 0xFFFF) as u32;
            apply_dpi(hwnd, dpi, Some(*(lparam as *const RECT)));
            0
        }
        WM_APP_STATE => {
            InvalidateRect(hwnd, ptr::null(), 1);
            KillTimer(hwnd, TIMER_REVERT); // a stale revert would blank a new transfer
            let kind = STATE.lock().unwrap().kind;
            match kind {
                StateKind::Success => SetTimer(hwnd, TIMER_REVERT, 1500, None),
                StateKind::Error => SetTimer(hwnd, TIMER_REVERT, 2500, None),
                _ => 0,
            };
            0
        }
        WM_TIMER => {
            if wparam == TIMER_REVERT {
                KillTimer(hwnd, TIMER_REVERT);
                {
                    let mut s = STATE.lock().unwrap();
                    s.kind = StateKind::Idle;
                    s.title.clear();
                    s.sub.clear();
                    s.progress = 0.0;
                }
                InvalidateRect(hwnd, ptr::null(), 1);
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------
fn detect_dark() -> bool {
    unsafe {
        let mut data: u32 = 1;
        let mut size: u32 = 4;
        let res = RegGetValueW(
            HKEY_CURRENT_USER,
            sys::wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize").as_ptr(),
            sys::wide("AppsUseLightTheme").as_ptr(),
            RRF_RT_REG_DWORD,
            ptr::null_mut(),
            &mut data as *mut u32 as *mut c_void,
            &mut size,
        );
        if res == 0 {
            data == 0
        } else {
            true
        }
    }
}

unsafe fn create_window() -> HWND {
    let hinst = GetModuleHandleW(ptr::null());
    let class_name = sys::wide("CursorDropPill");

    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: ptr::null_mut(),
        hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
        hbrBackground: ptr::null_mut(),
        lpszMenuName: ptr::null(),
        lpszClassName: class_name.as_ptr(),
        hIconSm: ptr::null_mut(),
    };
    RegisterClassExW(&wc);

    // Created at 96-dpi size, then resized to the monitor it actually landed on.
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_ACCEPTFILES,
        class_name.as_ptr(),
        sys::wide("CursorDrop").as_ptr(),
        WS_POPUP | WS_VISIBLE,
        0,
        0,
        PILL_W,
        PILL_H,
        ptr::null_mut(),
        ptr::null_mut(),
        hinst,
        ptr::null(),
    );

    apply_dpi(hwnd, GetDpiForWindow(hwnd), None);
    DragAcceptFiles(hwnd, 1);
    hwnd
}

/// Resize, re-centre (first run) and rebuild the rounded region for `dpi`.
/// Without this the widget is drawn at 96 dpi and stretched by the compositor,
/// which is what makes the text look soft on a scaled display.
unsafe fn apply_dpi(hwnd: HWND, dpi: u32, place: Option<RECT>) {
    DPI.store(if dpi == 0 { 96 } else { dpi as i32 }, Ordering::Relaxed);
    let (w, h) = (px(PILL_W), px(PILL_H));

    let (x, y, w, h) = match place {
        Some(r) => (r.left, r.top, r.right - r.left, r.bottom - r.top),
        None => (
            (GetSystemMetrics(SM_CXSCREEN) - w) / 2,
            (GetSystemMetrics(SM_CYSCREEN) - h) / 2,
            w,
            h,
        ),
    };
    SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);

    let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, px(RADIUS) * 2, px(RADIUS) * 2);
    SetWindowRgn(hwnd, rgn, 1); // the window owns the region from here
    InvalidateRect(hwnd, ptr::null(), 1);
}

fn main() {
    // CLI mode: `CursorDrop.exe <file> [file ...]` uploads the given files
    // (no GUI), copies the remote paths to the clipboard, then exits. Doubles
    // as a test harness. Results are recorded in CursorDrop.log.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        sys::log(&format!("CLI mode: {} arg(s)", args.len()));
        let files: Vec<PathBuf> = args.iter().map(PathBuf::from).collect();
        let ok = upload::run(files, &active_server());
        std::process::exit(if ok { 0 } else { 1 });
    }

    unsafe {
        // Per-monitor dpi: draw at the display's real pixel density instead of
        // letting the compositor scale a 96-dpi bitmap up.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        DARK.store(detect_dark(), Ordering::Relaxed);
        let hwnd = create_window();
        HWND_MAIN.store(hwnd as isize, Ordering::SeqCst);

        // Also loads the servers, so the menu is ready before the first click.
        sys::log(&format!(
            "CursorDrop started at {} dpi ({} theme), {} server(s)",
            DPI.load(Ordering::Relaxed),
            if DARK.load(Ordering::Relaxed) { "dark" } else { "light" },
            servers().len()
        ));

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
