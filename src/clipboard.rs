//! Clipboard access: dropped/copied file paths, image -> PNG, and setting text.

use core::ptr;
use std::path::{Path, PathBuf};

use windows_sys::core::GUID;
use windows_sys::Win32::Graphics::Gdi::HBITMAP;
use windows_sys::Win32::Graphics::GdiPlus::{
    GdipCreateBitmapFromHBITMAP, GdipDisposeImage, GdipSaveImageToFile, GdiplusShutdown,
    GdiplusStartup, GdiplusStartupInput, GpBitmap, GpImage,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::UI::Shell::{DragQueryFileW, HDROP};

use crate::sys::{from_wide, wide};

const CF_BITMAP: u32 = 2;
const CF_DIB: u32 = 8;
const CF_DIBV5: u32 = 17;
const CF_HDROP: u32 = 15;
const CF_UNICODETEXT: u32 = 13;

/// Existing file paths held by an `HDROP` — from a drag-drop or from CF_HDROP
/// on the clipboard. Drag-drop callers must `DragFinish` the handle afterwards;
/// a CF_HDROP handle belongs to the clipboard and must not be freed.
pub unsafe fn drop_paths(hdrop: HDROP) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, ptr::null_mut(), 0);
    for i in 0..count {
        let len = DragQueryFileW(hdrop, i, ptr::null_mut(), 0) as usize + 1;
        let mut buf = vec![0u16; len];
        let got = DragQueryFileW(hdrop, i, buf.as_mut_ptr(), len as u32);
        let p = PathBuf::from(from_wide(&buf[..got as usize]));
        if p.exists() {
            files.push(p);
        }
    }
    files
}

/// File paths currently on the clipboard (CF_HDROP), filtered to existing ones.
pub fn read_clipboard_files() -> Vec<PathBuf> {
    unsafe {
        if OpenClipboard(ptr::null_mut()) == 0 {
            return Vec::new();
        }
        let hdrop = GetClipboardData(CF_HDROP);
        let files = if hdrop.is_null() {
            Vec::new()
        } else {
            drop_paths(hdrop as HDROP)
        };
        CloseClipboard();
        files
    }
}

/// Whether the clipboard holds a bitmap image (DDB or DIB).
pub fn clipboard_has_bitmap() -> bool {
    unsafe {
        IsClipboardFormatAvailable(CF_BITMAP) != 0
            || IsClipboardFormatAvailable(CF_DIB) != 0
            || IsClipboardFormatAvailable(CF_DIBV5) != 0
    }
}

/// Put UTF-16 text on the clipboard. False if it could not be delivered.
pub fn set_clipboard_text(s: &str) -> bool {
    let w = wide(s);
    unsafe {
        // Fill the block BEFORE opening the clipboard: EmptyClipboard() on a
        // path that then fails would throw away what the user already had.
        // (A failed hand-off leaks the block; the OS reclaims it at exit.)
        let hmem = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2);
        if hmem.is_null() {
            return false;
        }
        let dst = GlobalLock(hmem) as *mut u16;
        if dst.is_null() {
            return false;
        }
        ptr::copy_nonoverlapping(w.as_ptr(), dst, w.len());
        GlobalUnlock(hmem);

        // Clipboard managers / Win+V / RDP hold the clipboard for a few ms at a
        // time; one refusal must not cost the user the whole transfer.
        let mut opened = false;
        for _ in 0..5 {
            opened = OpenClipboard(ptr::null_mut()) != 0;
            if opened {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        if !opened {
            return false;
        }
        EmptyClipboard();
        let ok = !SetClipboardData(CF_UNICODETEXT, hmem).is_null();
        CloseClipboard();
        ok
    }
}

fn png_encoder_clsid() -> GUID {
    // {557CF406-1A04-11D3-9A73-0000F81EF32E}
    GUID {
        data1: 0x557C_F406,
        data2: 0x1A04,
        data3: 0x11D3,
        data4: [0x9A, 0x73, 0x00, 0x00, 0xF8, 0x1E, 0xF3, 0x2E],
    }
}

/// Save the clipboard bitmap to `out` as PNG via GDI+. Returns success.
pub fn save_clipboard_bitmap_png(out: &Path) -> bool {
    unsafe {
        let mut token: usize = 0;
        let mut si: GdiplusStartupInput = core::mem::zeroed();
        si.GdiplusVersion = 1;
        if GdiplusStartup(&mut token, &si, ptr::null_mut()) != 0 {
            return false;
        }

        let mut ok = false;
        if OpenClipboard(ptr::null_mut()) != 0 {
            let hbitmap = GetClipboardData(CF_BITMAP);
            if !hbitmap.is_null() {
                let mut pbitmap: *mut GpBitmap = ptr::null_mut();
                let st = GdipCreateBitmapFromHBITMAP(
                    hbitmap as HBITMAP,
                    ptr::null_mut(),
                    &mut pbitmap,
                );
                if st == 0 && !pbitmap.is_null() {
                    let clsid = png_encoder_clsid();
                    let wpath = wide(&out.to_string_lossy());
                    let img = pbitmap as *mut GpImage;
                    let st2 = GdipSaveImageToFile(img, wpath.as_ptr(), &clsid, ptr::null());
                    ok = st2 == 0;
                    GdipDisposeImage(img);
                }
            }
            CloseClipboard();
        }
        GdiplusShutdown(token);
        ok
    }
}
