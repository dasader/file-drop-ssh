//! CursorDrop — drag files / paste clipboard images, SCP them to the remote
//! host's `.cursor-drop-files/`, and auto-paste the absolute paths into the
//! editor (Cursor / VS Code over SSH).
//!
//! Rust port of the original AutoHotkey v2 script (core feature set).

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
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use config::Server;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteObject,
    DrawTextW, EndPaint, FillRect, GetStockObject, InvalidateRect, RoundRect, SelectObject,
    SetBkMode, SetTextColor, SetWindowRgn, DT_CENTER, DT_SINGLELINE, DT_VCENTER, HFONT,
    NULL_BRUSH, PAINTSTRUCT, PS_SOLID,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture};
use windows_sys::Win32::UI::Shell::{
    DragAcceptFiles, DragFinish, DragQueryFileW, ExtractIconW, Shell_NotifyIconW, HDROP,
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetClientRect, GetCursorPos, GetSystemMetrics, KillTimer, LoadCursorW,
    PostMessageW, PostQuitMessage, RegisterClassExW, SendMessageW,
    SetForegroundWindow, SetLayeredWindowAttributes, SetTimer, SetWindowPos, TrackPopupMenu,
    CS_HREDRAW, CS_VREDRAW, GetMessageW, DispatchMessageW, TranslateMessage, HWND_TOPMOST,
    IDC_ARROW, LWA_ALPHA, MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOMOVE,
    SWP_NOSIZE, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_DROPFILES,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_NCLBUTTONDOWN, WM_PAINT,
    WM_RBUTTONUP, WM_TIMER, WNDCLASSEXW, WS_EX_ACCEPTFILES, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, HTCAPTION,
};

const PILL_W: i32 = 160;
const PILL_H: i32 = 52;

const WM_APP_STATE: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const TIMER_REVERT: usize = 1;

const ID_PASTE: usize = 101;
const ID_LOG: usize = 102;
const ID_EXIT: usize = 103;
/// Server menu items get command IDs `ID_SERVER_BASE + index`.
const ID_SERVER_BASE: usize = 200;

const VK_CONTROL: i32 = 0x11;
const VK_ESCAPE: usize = 0x1B;
const VK_V: usize = 0x56;

#[derive(Clone, Copy, PartialEq)]
pub enum StateKind {
    Idle,
    Reading,
    Uploading,
    Success,
    Error,
}

struct UiState {
    kind: StateKind,
    detail: String,
}

static STATE: Mutex<UiState> = Mutex::new(UiState {
    kind: StateKind::Idle,
    detail: String::new(),
});
static HWND_MAIN: AtomicIsize = AtomicIsize::new(0);
static DARK: AtomicBool = AtomicBool::new(true);

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
pub fn set_state(kind: StateKind, detail: &str) {
    {
        let mut s = STATE.lock().unwrap();
        s.kind = kind;
        s.detail = detail.to_string();
    }
    let h = HWND_MAIN.load(Ordering::SeqCst);
    if h != 0 {
        unsafe { PostMessageW(h as *mut c_void, WM_APP_STATE, kind as usize, 0) };
    }
}

// ---------------------------------------------------------------------------
// Clipboard paste orchestration
// ---------------------------------------------------------------------------
fn do_paste() {
    set_state(StateKind::Reading, "");
    let files = clipboard::read_clipboard_files();
    if !files.is_empty() {
        upload::handle_files(files, active_server());
        return;
    }
    if clipboard::clipboard_has_bitmap() {
        let out = sys::clip_dir().join(format!("clip-{}.png", sys::now_stamp()));
        if clipboard::save_clipboard_bitmap_png(&out) {
            sys::log(&format!("Bitmap saved: {}", out.display()));
            upload::handle_files(vec![out], active_server());
        } else {
            set_state(StateKind::Error, "Bitmap save failed");
        }
        return;
    }
    set_state(StateKind::Error, "Empty clipboard");
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------
fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

type Rgb = (u8, u8, u8);

/// Lighten (+) or darken (-) each channel, clamped.
fn shade(c: Rgb, d: i32) -> Rgb {
    let f = |v: u8| (v as i32 + d).clamp(0, 255) as u8;
    (f(c.0), f(c.1), f(c.2))
}

struct Pal {
    bg: Rgb,
    text: Rgb,
    sub: Rgb,
    border: Rgb,
}

fn palette(kind: StateKind, dark: bool) -> Pal {
    use StateKind::*;
    if dark {
        match kind {
            Idle => Pal { bg: (0x20, 0x20, 0x22), text: (0xE6, 0xE6, 0xE6), sub: (0x8C, 0x8C, 0x8C), border: (0x4C, 0x7A, 0x62) },
            Reading => Pal { bg: (0x2A, 0x22, 0x35), text: (0xC4, 0xA8, 0xE0), sub: (0x9A, 0x7D, 0xBF), border: (0x7B, 0x5C, 0x9E) },
            Uploading => Pal { bg: (0x2E, 0x28, 0x18), text: (0xE8, 0xC8, 0x6A), sub: (0xBF, 0xA4, 0x4E), border: (0xC9, 0xA8, 0x4E) },
            Success => Pal { bg: (0x1B, 0x33, 0x28), text: (0x7F, 0xD4, 0xA0), sub: (0x5A, 0xA8, 0x7A), border: (0x5A, 0xC0, 0x8A) },
            Error => Pal { bg: (0x35, 0x1E, 0x1E), text: (0xE8, 0x70, 0x70), sub: (0xC0, 0x50, 0x50), border: (0xC8, 0x5C, 0x5C) },
        }
    } else {
        match kind {
            Idle => Pal { bg: (0xFB, 0xFB, 0xFC), text: (0x22, 0x22, 0x22), sub: (0x99, 0x99, 0x99), border: (0x9C, 0xC7, 0xAE) },
            Reading => Pal { bg: (0xF0, 0xE8, 0xF8), text: (0x7B, 0x3F, 0xA0), sub: (0x9A, 0x7D, 0xBF), border: (0xB8, 0x9F, 0xD6) },
            Uploading => Pal { bg: (0xFF, 0xF5, 0xE0), text: (0xB8, 0x8A, 0x20), sub: (0xD4, 0xA8, 0x30), border: (0xD9, 0xC0, 0x7A) },
            Success => Pal { bg: (0xE8, 0xF5, 0xEC), text: (0x2E, 0x8B, 0x4E), sub: (0x5A, 0xA8, 0x7A), border: (0x8F, 0xC9, 0xA8) },
            Error => Pal { bg: (0xFD, 0xE8, 0xE8), text: (0xC0, 0x30, 0x30), sub: (0xD0, 0x50, 0x50), border: (0xD9, 0x9A, 0x9A) },
        }
    }
}

fn current_visual() -> (Pal, String, String) {
    let (kind, detail) = {
        let s = STATE.lock().unwrap();
        (s.kind, s.detail.clone())
    };
    let dark = DARK.load(Ordering::Relaxed);
    let pal = palette(kind, dark);
    let (label, subtext) = match kind {
        StateKind::Idle => ("Drop / Paste".to_string(), "Ctrl+V or drag files".to_string()),
        StateKind::Reading => ("Reading...".to_string(), "Checking clipboard".to_string()),
        StateKind::Uploading => (
            if detail.is_empty() { "Uploading...".to_string() } else { detail },
            "Syncing to remote".to_string(),
        ),
        StateKind::Success => (
            if detail.is_empty() { "Done".to_string() } else { detail },
            "Ctrl+Shift+V to paste".to_string(),
        ),
        StateKind::Error => (
            if detail.is_empty() { "Error".to_string() } else { detail },
            "Check log".to_string(),
        ),
    };
    (pal, label, subtext)
}

unsafe fn make_font(height: i32, weight: i32) -> HFONT {
    let face = sys::wide("Segoe UI");
    CreateFontW(
        height, 0, 0, 0, weight, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr(),
    )
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);

    let mut rc: RECT = zeroed();
    GetClientRect(hwnd, &mut rc);
    let h = rc.bottom;

    let (pal, label, subtext) = current_visual();
    let w = rc.right;

    // Subtle vertical gradient (lighter top -> darker bottom) for depth.
    let top = shade(pal.bg, 14);
    let bot = shade(pal.bg, -10);
    let lerp = |a: u8, b: u8, t: f32| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    let denom = if h > 1 { (h - 1) as f32 } else { 1.0 };
    for y in 0..h {
        let t = y as f32 / denom;
        let col = rgb(lerp(top.0, bot.0, t), lerp(top.1, bot.1, t), lerp(top.2, bot.2, t));
        let brush = CreateSolidBrush(col);
        let row = RECT { left: 0, top: y, right: w, bottom: y + 1 };
        FillRect(hdc, &row, brush);
        DeleteObject(brush);
    }

    // Rounded capsule border in the state accent color.
    let pen = CreatePen(PS_SOLID, 2, rgb(pal.border.0, pal.border.1, pal.border.2));
    let old_pen = SelectObject(hdc, pen);
    let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
    let ell = h - 2;
    RoundRect(hdc, 1, 1, w - 1, h - 1, ell, ell);
    SelectObject(hdc, old_pen);
    SelectObject(hdc, old_brush);
    DeleteObject(pen);

    SetBkMode(hdc, 1); // TRANSPARENT

    let block_top = ((h - 34) / 2).max(2);

    // label
    let lfont = make_font(-18, 600);
    let old = SelectObject(hdc, lfont);
    SetTextColor(hdc, rgb(pal.text.0, pal.text.1, pal.text.2));
    let mut lr = RECT { left: 0, top: block_top, right: w, bottom: block_top + 20 };
    let wl = sys::wide(&label);
    DrawTextW(hdc, wl.as_ptr(), -1, &mut lr, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    SelectObject(hdc, old);
    DeleteObject(lfont);

    // sub label (only when tall enough)
    if !subtext.is_empty() && h >= 48 {
        let sfont = make_font(-12, 400);
        let olds = SelectObject(hdc, sfont);
        SetTextColor(hdc, rgb(pal.sub.0, pal.sub.1, pal.sub.2));
        let st = block_top + 20;
        let mut sr = RECT { left: 0, top: st, right: w, bottom: st + 14 };
        let ws = sys::wide(&subtext);
        DrawTextW(hdc, ws.as_ptr(), -1, &mut sr, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        SelectObject(hdc, olds);
        DeleteObject(sfont);
    }

    EndPaint(hwnd, &ps);
}

// ---------------------------------------------------------------------------
// Tray + menu
// ---------------------------------------------------------------------------
unsafe fn add_tray(hwnd: HWND) {
    let hinst = GetModuleHandleW(ptr::null());
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_APP_TRAY;
    nid.hIcon = ExtractIconW(hinst, sys::wide("shell32.dll").as_ptr(), 46);
    let tip = sys::wide("CursorDrop");
    for (i, c) in tip.iter().enumerate().take(nid.szTip.len()) {
        nid.szTip[i] = *c;
    }
    Shell_NotifyIconW(NIM_ADD, &mut nid);
}

unsafe fn remove_tray(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    Shell_NotifyIconW(NIM_DELETE, &mut nid);
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();

    // Server list (only when more than one is configured). The active server is
    // checkmarked; clicking another switches the active selection for this
    // session. With a single server the menu looks exactly as before.
    let list = servers();
    if list.len() > 1 {
        let active = ACTIVE.load(Ordering::Relaxed).min(list.len() - 1);
        for (i, s) in list.iter().enumerate() {
            let flags = MF_STRING | (if i == active { MF_CHECKED } else { 0 });
            AppendMenuW(menu, flags, ID_SERVER_BASE + i, sys::wide(&s.name).as_ptr());
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
    }

    AppendMenuW(menu, MF_STRING, ID_PASTE, sys::wide("Paste clipboard").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
    AppendMenuW(menu, MF_STRING, ID_LOG, sys::wide("Show log").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
    AppendMenuW(menu, MF_STRING, ID_EXIT, sys::wide("Exit").as_ptr());

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
    let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, ptr::null_mut(), 0);
    let mut files: Vec<PathBuf> = Vec::new();
    for i in 0..count {
        let len = DragQueryFileW(hdrop, i, ptr::null_mut(), 0) as usize + 1;
        let mut buf = vec![0u16; len];
        let got = DragQueryFileW(hdrop, i, buf.as_mut_ptr(), len as u32);
        files.push(PathBuf::from(sys::from_wide(&buf[..got as usize])));
    }
    DragFinish(hdrop);

    if files.is_empty() {
        return;
    }
    set_state(StateKind::Reading, "");
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
            if wparam == VK_ESCAPE {
                DestroyWindow(hwnd);
            } else if wparam == VK_V && ((GetKeyState(VK_CONTROL) as i32) & 0x8000) != 0 {
                do_paste();
            }
            0
        }
        WM_DROPFILES => {
            handle_drop(wparam);
            0
        }
        WM_APP_STATE => {
            InvalidateRect(hwnd, ptr::null(), 1);
            let kind = wparam as u32;
            if kind == StateKind::Success as u32 {
                SetTimer(hwnd, TIMER_REVERT, 1500, None);
            } else if kind == StateKind::Error as u32 {
                SetTimer(hwnd, TIMER_REVERT, 2500, None);
            }
            0
        }
        WM_TIMER => {
            if wparam == TIMER_REVERT {
                KillTimer(hwnd, TIMER_REVERT);
                {
                    let mut s = STATE.lock().unwrap();
                    s.kind = StateKind::Idle;
                    s.detail.clear();
                }
                InvalidateRect(hwnd, ptr::null(), 1);
            }
            0
        }
        WM_APP_TRAY => {
            let m = lparam as u32;
            if m == WM_RBUTTONUP || m == WM_CONTEXTMENU {
                show_menu(hwnd);
            } else if m == WM_LBUTTONDBLCLK {
                do_paste();
            }
            0
        }
        WM_DESTROY => {
            remove_tray(hwnd);
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

    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    let x = (sw - PILL_W) / 2;
    let y = (sh - PILL_H) / 2;

    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_ACCEPTFILES,
        class_name.as_ptr(),
        sys::wide("CursorDrop").as_ptr(),
        WS_POPUP | WS_VISIBLE,
        x,
        y,
        PILL_W,
        PILL_H,
        ptr::null_mut(),
        ptr::null_mut(),
        hinst,
        ptr::null(),
    );

    // Full capsule: rounded short ends (ellipse = height).
    let rgn = CreateRoundRectRgn(0, 0, PILL_W + 1, PILL_H + 1, PILL_H, PILL_H);
    SetWindowRgn(hwnd, rgn, 1);

    SetLayeredWindowAttributes(hwnd, 0, 240, LWA_ALPHA);
    SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
    DragAcceptFiles(hwnd, 1);

    hwnd
}

fn main() {
    sys::clip_dir();

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

    // Load servers up front so the startup log lists them and the menu is ready.
    sys::log(&format!(
        "CursorDrop started ({}x{}), {} server(s)",
        PILL_W,
        PILL_H,
        servers().len()
    ));

    unsafe {
        DARK.store(detect_dark(), Ordering::Relaxed);
        let hwnd = create_window();
        HWND_MAIN.store(hwnd as isize, Ordering::SeqCst);
        add_tray(hwnd);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
