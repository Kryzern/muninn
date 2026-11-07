#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod clip;
mod decode;
mod edit;
mod engine;
mod gfx;
mod gpu;
mod index;
mod models;
mod ocr;
mod platform;
mod query;
mod scene;
mod settings;
mod update;
mod watch;

use anyhow::{anyhow, Result};
use app::App;
use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::ValidateRect;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub fn log(msg: impl AsRef<str>) {
    use std::io::Write;
    let path = index::data_dir().join("muninn.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 512 << 10) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {}", platform::now(), msg.as_ref());
    }
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        ValidateRect(Some(h), None).ok().ok();
        with_app(|app| app.tick());
        return LRESULT(0);
    }
    match with_app(|app| app.handle(msg, wp, lp)).flatten() {
        Some(r) => r,
        None => DefWindowProcW(h, msg, wp, lp),
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("index") => cli_index(&args[1..]),
        Some("search") => cli_search(&args[1..].join(" ")),
        Some("devices") => {
            for a in gpu::adapters() {
                println!("{}: {} ({} MB{})", a.index, a.name, a.vram >> 20, if a.software { ", software" } else { "" });
            }
            Ok(())
        }
        _ => {
            run(args.iter().any(|a| a == "--minimized"));
            Ok(())
        }
    }
}

fn run(minimized: bool) {
    unsafe {
        let _mutex = CreateMutexW(None, true, w!("Local\\Muninn.Window"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(h) = FindWindowW(w!("MuninnWindow"), None) {
                let _ = ShowWindow(h, SW_RESTORE);
                let _ = SetForegroundWindow(h);
            }
            return;
        }
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).ok();
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let cfg = settings::Settings::load();

        let instance = GetModuleHandleW(None).unwrap_or_default();
        // resource 1 is the icon build.rs embeds
        let icon = LoadIconW(Some(instance.into()), PCWSTR(1 as *const u16)).unwrap_or_default();
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: icon,
            hIconSm: icon,
            lpszClassName: w!("MuninnWindow"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let (x, y, ww, wh) = match cfg.window {
            Some([x, y, w, h]) if w > 200 && h > 200 => (x, y, w, h),
            _ => (CW_USEDEFAULT, CW_USEDEFAULT, 1360, 860),
        };
        let hwnd = match CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP, w!("MuninnWindow"), w!("Muninn"), WS_OVERLAPPEDWINDOW, x, y, ww, wh, None, None, Some(instance.into()), None) {
            Ok(h) => h,
            Err(e) => {
                log(format!("CreateWindowEx failed: {e}"));
                return;
            }
        };
        platform::dark_title_bar(hwnd);
        let app = App::new(hwnd, cfg.clone());
        APP.with(|a| *a.borrow_mut() = Some(app));
        // MUNINN_NOACTIVATE: open behind the current window (for automated checks)
        let quiet = std::env::var_os("MUNINN_NOACTIVATE").is_some();
        let show = if minimized { SW_SHOWMINNOACTIVE } else if quiet { SW_SHOWNOACTIVATE } else if cfg.maximized { SW_SHOWMAXIMIZED } else { SW_SHOWNORMAL };
        let _ = ShowWindow(hwnd, show);

        // Input first: drain every message, then draw at most one frame when
        // the compositor is ready for it, and sleep when nothing moves.
        let mut msg = MSG::default();
        'run: loop {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'run;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            match with_app(|app| app.wants_frame()).flatten() {
                Some(_) if with_app(|app| app.frame_wait_ms()).unwrap_or(0) > 0 => {
                    // too soon for the next frame: wait for it, or for input
                    let ms = with_app(|app| app.frame_wait_ms()).unwrap_or(1);
                    MsgWaitForMultipleObjectsEx(None, ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                }
                Some(ready) => {
                    let handles = [ready];
                    let r = MsgWaitForMultipleObjectsEx(Some(&handles), 100, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                    if r == WAIT_OBJECT_0 || r == WAIT_TIMEOUT {
                        with_app(|app| app.tick());
                        // One frame per display refresh: sleep until the compositor's
                        // next one. Without this an HWND swap chain runs as fast as
                        // the GPU can go (thousands of frames a second).
                        if with_app(|app| app.wants_frame().is_some()).unwrap_or(false) {
                            let _ = windows::Win32::Graphics::Dwm::DwmFlush();
                        }
                    }
                }
                None => {
                    WaitMessage().ok();
                }
            }
        }
    }
}

fn active_model() -> Result<(&'static models::Model, PathBuf)> {
    let cfg = settings::Settings::load();
    let cpu = models::needs_cpu(cfg.device == settings::Device::Cpu);
    let m = models::get(&cfg.model)
        .filter(|m| m.installed(cpu))
        .or_else(|| models::catalog().iter().find(|m| m.installed(cpu)))
        .ok_or_else(|| anyhow!("no model installed: open Muninn and pick one in Models"))?;
    let dir = m.location(cpu).ok_or_else(|| anyhow!("model files missing"))?;
    Ok((m, dir))
}

fn cli_index(dirs: &[String]) -> Result<()> {
    let cfg = settings::Settings::load();
    let roots: Vec<PathBuf> = if dirs.is_empty() { cfg.roots() } else { dirs.iter().map(PathBuf::from).collect() };
    let (m, dir) = active_model()?;
    let t = Instant::now();
    let clip = clip::Clip::load(&dir, cfg.device == settings::Device::Cpu)?;
    println!("{} loaded on {} in {:?}", m.name, clip.device, t.elapsed());
    let t = Instant::now();
    let files = index::scan(&roots);
    println!("found {} images in {:?}", files.len(), t.elapsed());
    let t = Instant::now();
    let old = index::Index::load(&m.id, clip.dim);
    let idx = index::update(&clip, &old, files, cfg.read_text, |d, n, _| {
        eprint!("\r{d}/{n}");
        true
    })?;
    println!("\nindexed {} images in {:.1}s", idx.entries.len(), t.elapsed().as_secs_f32());
    Ok(())
}

fn cli_search(q: &str) -> Result<()> {
    let (m, dir) = active_model()?;
    let clip = clip::Clip::load(&dir, false)?;
    let idx = index::Index::load(&m.id, clip.dim);
    let u = query::understand(q, platform::now());
    let text = if u.text.is_empty() { "a photo".to_string() } else { u.text.clone() };
    let _ = clip.embed_text(&text)?; // warm up (GPU kernels compile on first use)
    let t = Instant::now();
    let v = clip.embed_text(&text)?;
    let words: Vec<String> = u.text.to_lowercase().split_whitespace().map(String::from).collect();
    let k = std::env::var("MUNINN_K").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
    let hits = idx.search_where(&v, &words, k, |e| u.accepts(e.when()));
    let chips = if u.chips.is_empty() { String::new() } else { format!(" [{}]", u.chips.join(", ")) };
    println!("{} results in {:?} ({}, {}){chips}", hits.len(), t.elapsed(), m.name, clip.device);
    for (i, s) in hits {
        println!("{s:.3}  {}", idx.entries[i].path);
    }
    Ok(())
}
