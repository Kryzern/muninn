//! Small Windows helpers: opening files, the clipboard, the folder picker,
//! the title bar.

use windows::core::{w, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn open(path: &str) {
    unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
}

pub fn open_url(url: &str) {
    open(url);
}

/// Open Explorer with the file selected.
pub fn reveal(path: &str) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{path}\"")).spawn();
}

/// Put the file itself on the clipboard, the way Explorer's Copy does, so it
/// pastes into a chat, an email or a folder.
pub fn copy_file(hwnd: HWND, path: &str) -> bool {
    #[repr(C)]
    struct DropFiles {
        p_files: u32,
        pt: POINT,
        f_nc: BOOL,
        f_wide: BOOL,
    }
    let wide: Vec<u16> = path.encode_utf16().chain([0, 0]).collect();
    let head = std::mem::size_of::<DropFiles>();
    let bytes = head + wide.len() * 2;
    unsafe {
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes) else { return false };
        let p = GlobalLock(mem) as *mut u8;
        if p.is_null() {
            return false;
        }
        let df = DropFiles { p_files: head as u32, pt: POINT::default(), f_nc: BOOL(0), f_wide: BOOL(1) };
        std::ptr::copy_nonoverlapping(&df as *const DropFiles as *const u8, p, head);
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, p.add(head), wide.len() * 2);
        let _ = GlobalUnlock(mem);
        if OpenClipboard(Some(hwnd)).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        // CF_HDROP
        let ok = SetClipboardData(15, Some(HANDLE(mem.0))).is_ok();
        let _ = CloseClipboard();
        ok
    }
}

/// Ask for a folder. None when cancelled.
pub fn pick_folder(hwnd: HWND) -> Option<String> {
    unsafe {
        let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = d.GetOptions().ok()?;
        d.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        d.SetTitle(w!("Choose a folder of photos")).ok();
        d.Show(Some(hwnd)).ok()?;
        let item = d.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// Dark title bar in the window's own background colour.
pub fn dark_title_bar(hwnd: HWND) {
    unsafe {
        let on = BOOL(1);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const _ as *const _, 4);
        let color: u32 = 0x000c_0b0b; // 0x00BBGGRR
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &color as *const _ as *const _, 4);
        let text: u32 = 0x00ed_eded;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TEXT_COLOR, &text as *const _ as *const _, 4);
    }
}

pub fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

pub fn folder(path: &str) -> &str {
    path.rfind(['\\', '/']).map(|i| &path[..i]).unwrap_or("")
}

/// "Sat 21 June 2026" for unix seconds.
pub fn long_date(t: i64) -> String {
    let d = t.div_euclid(86_400);
    let (y, m, day) = crate::query::civil(d);
    const WD: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MN: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    format!("{} {} {} {}", WD[d.rem_euclid(7) as usize], day, MN[m as usize - 1], y)
}

pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}
