//! The window: the photo field, the glass search bar, the Settings and
//! Models tabs, and the photo viewer. Drawn every frame while something
//! moves, not at all while nothing does.

use crate::edit::Edit;
use crate::engine::{self, Engine, Event, Phase, Status};
use crate::gfx::{Color, Gfx, Glass, Quad, Rect, Texture};
use crate::models::{self, Download};
use crate::platform;
use crate::query::{self, Understood};
use crate::scene::Field;
use crate::settings::{self, Device, Field as FieldMode, Settings};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const FG: Color = Color::gray(0.93, 1.0);
const MID: Color = Color::gray(0.60, 1.0);
const GHOST: Color = Color::gray(0.30, 1.0);
const TIMER_CARET: usize = 1;
const TIMER_SEARCH: usize = 2;
const TIMER_PH: usize = 3;
const WM_MOUSELEAVE: u32 = 0x02A3;
const PLACEHOLDERS: [&str; 6] = ["a dog on the beach", "snow last winter", "receipts", "sunset with friends", "screenshots", "coffee in Paris"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Search,
    Settings,
    Models,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Opt {
    ReadText,
    Cloud,
    Field,
    Glass,
    Animations,
    Device,
    Autostart,
    Updates,
    Reread,
}

#[derive(Clone, PartialEq, Debug)]
enum Act {
    Tab(Mode),
    Close,
    Opt(Opt),
    AddFolder,
    RemoveFolder(usize),
    Download(&'static str),
    Cancel,
    Use(&'static str),
    Remove(&'static str),
    Open,
    Reveal,
    Copy,
    Similar,
    Prev,
    Next,
    Update,
}

struct Viewer {
    id: usize,
    key: u64,
    from: Rect,
    t: f32,
    closing: bool,
    bmp: Option<(ID2D1Bitmap1, u32, u32)>,
    list: Vec<usize>,
    copied: Option<Instant>,
}

pub struct App {
    hwnd: HWND,
    gfx: Option<Gfx>,
    scale: f32,
    w: f32,
    h: f32,
    cfg: Settings,
    engine: Engine,
    status: Status,
    tex: HashMap<u64, Texture>,
    asked: HashSet<u64>,
    /// The index version the photo numbers below belong to.
    ver: u64,
    /// The field moved last frame: keep drawing.
    moving: bool,
    /// A texture arrived or the size changed: draw the photos again.
    photos_dirty: bool,
    /// Looked up once, not every frame: whether there's a GPU to run models
    /// on, which models are on disk (and removable), and the default folders.
    has_gpu: bool,
    installed: Vec<(bool, bool)>,
    default_roots: Vec<String>,
    /// What the photo layer showed last time, rounded to what an eye can see.
    last_sig: Vec<[i32; 6]>,
    /// MUNINN_DEBUG frame stats: (since, frames, logic ms, draw ms, photo passes).
    stats: (Option<Instant>, u32, f32, f32, u32),
    /// Until then, draw at the monitor's full refresh rate: results are
    /// flying into place.
    burst_until: Option<Instant>,
    field: Field,
    crowd_for: usize,
    edit: Edit,
    understood: Understood,
    similar_of: Option<usize>,
    search_id: u64,
    sent: Option<String>,
    typed_at: Option<Instant>,
    meta: String,
    sel: Option<usize>,
    mode: Mode,
    viewer: Option<Viewer>,
    mouse: (f32, f32),
    tracking: bool,
    controls: Vec<(Rect, Act)>,
    hot: Option<Act>,
    download: Option<Download>,
    update: Option<crate::update::Release>,
    updating: Option<f32>,
    update_failed: bool,
    last: Option<Instant>,
    t0: Instant,
    dock: f32,
    panel: f32,
    scroll: f32,
    caret_on: bool,
    /// Placeholder typewriter: line, chars shown, direction (or hold), rounds done.
    ph: (usize, usize, i32, usize),
    active: bool,
    dirty: bool,
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    Rect::new(lerp(a.x, b.x, t), lerp(a.y, b.y, t), lerp(a.w, b.w, t), lerp(a.h, b.h, t))
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

impl App {
    pub fn new(hwnd: HWND, cfg: Settings) -> App {
        let scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
        let engine = Engine::start(hwnd, cfg.clone());
        let mut app = App {
            hwnd,
            gfx: None,
            scale: scale.max(1.0),
            w: 1.0,
            h: 1.0,
            status: engine.shared.status(),
            engine,
            tex: HashMap::new(),
            asked: HashSet::new(),
            ver: u64::MAX,
            moving: true,
            photos_dirty: true,
            last_sig: vec![],
            has_gpu: crate::gpu::adapters().iter().any(|a| !a.software),
            installed: vec![],
            default_roots: crate::index::default_roots().iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            burst_until: None,
            stats: (None, 0, 0.0, 0.0, 0),
            field: Field::new(),
            crowd_for: usize::MAX,
            edit: Edit::default(),
            understood: query::understand("", 0),
            similar_of: None,
            search_id: 0,
            sent: None,
            typed_at: None,
            meta: String::new(),
            sel: None,
            mode: Mode::Search,
            viewer: None,
            mouse: (-1.0, -1.0),
            tracking: false,
            controls: vec![],
            hot: None,
            download: None,
            update: None,
            updating: None,
            update_failed: false,
            last: None,
            t0: Instant::now(),
            dock: 0.0,
            panel: 0.0,
            scroll: 0.0,
            caret_on: true,
            ph: (0, 0, 1, 0),
            // set by WM_ACTIVATE once the window really is in front
            active: false,
            dirty: true,
            cfg,
        };
        app.field.quiet = app.quiet();
        app.refresh_installed();
        if app.cfg.model.is_empty() || models::get(&app.cfg.model).is_none_or(|m| !m.installed(app.cpu())) {
            // first run, or the chosen model went missing: use one that's here
            if let Some(m) = models::catalog().iter().find(|m| m.installed(app.cpu())) {
                app.cfg.model = m.id.clone();
                app.cfg.save();
                app.engine.reload(app.cfg.clone());
            }
        }
        if app.cfg.check_updates {
            let s = app.engine.shared.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(10));
                loop {
                    if let Some(r) = crate::update::check() {
                        s.send(Event::Update(r));
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(6 * 3600));
                }
            });
        }
        unsafe {
            SetTimer(Some(hwnd), TIMER_CARET, 530, None);
            SetTimer(Some(hwnd), TIMER_PH, 60, None);
        }
        app
    }

    fn cpu(&self) -> bool {
        self.cfg.device == Device::Cpu || !self.has_gpu
    }

    /// Which models are on disk; checked when it can have changed, not per frame.
    fn refresh_installed(&mut self) {
        let cpu = self.cpu();
        self.installed = models::catalog().iter().map(|m| (m.installed(cpu), m.dir().exists())).collect();
    }

    fn quiet(&self) -> f32 {
        match self.cfg.field {
            FieldMode::Full => 1.0,
            FieldMode::Quiet => 0.55,
            FieldMode::Off => 0.0,
        }
    }

    fn animate(&self) -> bool {
        self.cfg.animations
    }

    // ---- frame pacing ----------------------------------------------------------

    /// The frame-latency waitable when a frame should be drawn, else None.
    pub fn wants_frame(&mut self) -> Option<HANDLE> {
        if self.gfx.is_none() {
            self.ensure_gfx();
        }
        let gfx = self.gfx.as_ref()?;
        let moving = self.moving || self.easing() || self.download.is_some() || self.updating.is_some() || gfx.stale();
        if !moving && !self.dirty {
            // nothing to draw: the next frame's timing starts fresh
            self.last = None;
        }
        (self.dirty || moving).then(|| gfx.waitable())
    }

    /// Milliseconds until the next frame may start. Short transitions (a photo
    /// opening, results arriving, a panel sliding in) run at the monitor's full
    /// refresh rate so they stay smooth on fast screens; the drift that follows
    /// the pointer, which can go on for as long as the mouse moves, is held to
    /// 60 fps.
    pub fn frame_wait_ms(&self) -> u32 {
        const FRAME: f32 = 1000.0 / 60.0;
        if self.easing() || self.burst_until.is_some_and(|t| Instant::now() < t) {
            return 0;
        }
        match self.last {
            Some(l) if !self.dirty || self.moving => (FRAME - l.elapsed().as_secs_f32() * 1000.0).max(0.0) as u32,
            _ => 0,
        }
    }

    fn easing(&self) -> bool {
        let dock = if self.docked() { 1.0 } else { 0.0 };
        let panel = if self.mode != Mode::Search { 1.0 } else { 0.0 };
        self.dock != dock || self.panel != panel || self.viewer.as_ref().is_some_and(|v| v.closing || v.t < 1.0)
    }

    fn ph_running(&self) -> bool {
        // a few rounds of examples, then it rests on the last one
        self.animate() && self.active && self.ph.3 < 3 && self.edit.text.is_empty() && self.mode == Mode::Search && self.status.phase != Phase::NoModel
    }

    fn ensure_gfx(&mut self) {
        let mut r = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut r).ok() };
        let (pw, ph) = ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32);
        match Gfx::new(self.hwnd, pw, ph, self.scale) {
            Ok(g) => self.gfx = Some(g),
            Err(e) => crate::log(format!("graphics: {e}")),
        }
        self.w = pw as f32 / self.scale;
        self.h = ph as f32 / self.scale;
        self.relayout();
    }

    fn docked(&self) -> bool {
        !self.edit.text.trim().is_empty() || self.similar_of.is_some()
    }

    fn bar_rect(&self, dock: f32) -> Rect {
        let w = (self.w - 32.0).min(760.0);
        let top_docked = if self.w >= 1040.0 { 22.0 } else { 64.0 };
        let y = lerp((self.h * 0.44 - 38.0).max(70.0), top_docked, dock);
        let h = lerp(76.0, 60.0, dock);
        Rect::new((self.w - w) / 2.0, y, w, h)
    }

    fn relayout(&mut self) {
        let chips = if self.understood.chips.is_empty() { 0.0 } else { 46.0 };
        let b = self.bar_rect(1.0);
        self.field.resize(self.w, self.h, b.y + b.h + chips + 26.0);
    }

    // ---- events from the engine ------------------------------------------------

    fn poll(&mut self) {
        for ev in self.engine.events() {
            match ev {
                Event::Status => {
                    let was = self.status.phase.clone();
                    self.status = self.engine.shared.status();
                    self.follow_index();
                    if was != self.status.phase && matches!(self.status.phase, Phase::Ready) && self.sent.is_some() {
                        self.sent = None; // ask again now the model is here
                        self.typed_at = Some(Instant::now());
                    }
                    self.refresh_crowd();
                }
                Event::Results { id, hits, ms } => {
                    if id != self.search_id {
                        continue;
                    }
                    let idx = self.engine.shared.idx.read().unwrap();
                    let hits: Vec<(usize, f32, u64)> = hits.into_iter().filter_map(|i| idx.entries.get(i).map(|e| (i, engine::aspect(e), engine::key(&e.path)))).collect();
                    drop(idx);
                    self.meta = match hits.len() {
                        0 => String::new(),
                        1 => format!("1 photo · {ms} ms"),
                        n => format!("{n} photos · {ms} ms"),
                    };
                    self.relayout();
                    self.field.set_results(&hits, self.docked());
                    self.burst_until = Some(Instant::now() + std::time::Duration::from_millis(900));
                    self.sel = None;
                }
                Event::Thumb { key, px, w, h } => {
                    if let Some(t) = self.gfx.as_ref().and_then(|g| g.texture(&px, w, h)) {
                        self.tex.insert(key, t);
                        self.photos_dirty = true;
                    }
                }
                Event::Full { key, px, w, h } => {
                    let bmp = self.gfx.as_ref().and_then(|g| g.bitmap(&px, w, h));
                    if let (Some(v), Some(b)) = (self.viewer.as_mut(), bmp) {
                        if v.key == key {
                            v.bmp = Some((b, w, h));
                        }
                    }
                }
                Event::Update(r) => self.update = Some(r),
                Event::UpdateProgress(f) => self.updating = Some(f),
                Event::UpdateFailed => {
                    self.updating = None;
                    self.update_failed = true;
                }
                Event::Download => {
                    if let Some(d) = &self.download {
                        match d.state() {
                            models::State::Done => {
                                let id = d.id.clone();
                                self.download = None;
                                self.refresh_installed();
                                // the first model downloaded is the one to use
                                if self.status.phase == Phase::NoModel || self.cfg.model.is_empty() {
                                    self.use_model(&id);
                                }
                            }
                            models::State::Cancelled => self.download = None,
                            _ => {}
                        }
                    }
                }
            }
        }
        self.dirty = true;
    }

    /// The index was replaced: photo numbers may have moved, so re-point
    /// everything on screen by path.
    fn follow_index(&mut self) {
        let v = self.engine.shared.version.load(std::sync::atomic::Ordering::Relaxed);
        if v == self.ver {
            return;
        }
        self.ver = v;
        let idx = self.engine.shared.idx.read().unwrap();
        let map: HashMap<u64, usize> = idx.entries.iter().enumerate().map(|(i, e)| (engine::key(&e.path), i)).collect();
        drop(idx);
        self.field.remap(&map);
        if let Some(vw) = self.viewer.as_mut() {
            match map.get(&vw.key) {
                Some(&i) => vw.id = i,
                None => self.viewer = None,
            }
        }
        self.sel = None;
        self.crowd_for = usize::MAX;
        self.photos_dirty = true;
    }

    fn refresh_crowd(&mut self) {
        let n = match self.cfg.field {
            FieldMode::Full => 36,
            FieldMode::Quiet => 18,
            FieldMode::Off => 0,
        };
        let key = self.status.count * 4 + n;
        if key == self.crowd_for {
            return;
        }
        self.crowd_for = key;
        let crowd = if n == 0 { vec![] } else { self.engine.crowd(n) };
        self.field.set_idle(&crowd);
    }

    fn want_textures(&mut self) {
        let missing: Vec<(usize, u64)> = self.field.items.iter().map(|it| (it.id, it.key)).filter(|(_, k)| !self.tex.contains_key(k) && !self.asked.contains(k)).collect();
        if !missing.is_empty() {
            self.asked.extend(missing.iter().map(|m| m.1));
            self.engine.thumbs(missing.into_iter().map(|m| m.0).collect());
        }
        // keep the cache bounded: drop textures nothing shows
        if self.tex.len() > 220 {
            let live: HashSet<u64> = self.field.items.iter().map(|it| it.key).collect();
            self.tex.retain(|id, _| live.contains(id));
            self.asked.retain(|id| live.contains(id));
        }
    }

    // ---- searching -------------------------------------------------------------

    fn changed_text(&mut self) {
        self.similar_of = None;
        self.field.wake();
        self.typed_at = Some(Instant::now());
        // ask once typing pauses, whether or not frames are being drawn
        unsafe { SetTimer(Some(self.hwnd), TIMER_SEARCH, 80, None) };
        self.caret_on = true;
        self.understood = query::understand(&self.edit.text, platform::now());
        self.relayout();
        if self.edit.text.trim().is_empty() {
            self.sent = None;
            self.meta.clear();
            self.field.set_results(&[], false);
            self.burst_until = Some(Instant::now() + std::time::Duration::from_millis(900));
        }
        self.dirty = true;
    }

    fn maybe_search(&mut self) {
        let Some(t) = self.typed_at else { return };
        if t.elapsed().as_millis() < 70 {
            return;
        }
        self.typed_at = None;
        let q = self.edit.text.trim().to_string();
        if q.is_empty() || self.sent.as_deref() == Some(q.as_str()) {
            return;
        }
        if std::env::var_os("MUNINN_DEBUG").is_some() {
            crate::log(format!("asking {:?} as {:?}", q, self.understood.text));
        }
        self.sent = Some(q);
        self.search_id += 1;
        self.engine.search(self.search_id, self.understood.clone());
    }

    fn find_similar(&mut self, id: usize) {
        let name = self.engine.shared.idx.read().unwrap().entries.get(id).map(|e| platform::file_name(&e.path).to_string()).unwrap_or_default();
        self.close_viewer();
        self.edit.set(&format!("like {name}"));
        self.edit.select_all();
        self.understood = query::understand("", 0);
        self.similar_of = Some(id);
        self.sent = None;
        self.relayout();
        self.search_id += 1;
        self.engine.similar(self.search_id, id);
    }

    // ---- viewer ----------------------------------------------------------------

    fn open_viewer(&mut self, id: usize) {
        let from = self.field.find(id).map(|it| it.rect).unwrap_or(Rect::new(self.w / 2.0, self.h / 2.0, 1.0, 1.0));
        let list = { let r = self.field.results(); if r.contains(&id) { r } else { vec![id] } };
        let key = self.engine.shared.idx.read().unwrap().entries.get(id).map(|e| engine::key(&e.path)).unwrap_or(0);
        self.viewer = Some(Viewer { id, key, from, t: 0.0, closing: false, bmp: None, list, copied: None });
        let long = (self.w.max(self.h) * self.scale).min(3000.0) as u32;
        self.engine.full(id, long);
        self.dirty = true;
    }

    fn close_viewer(&mut self) {
        if let Some(v) = self.viewer.as_mut() {
            v.closing = true;
            if let Some(it) = self.field.find(v.id) {
                v.from = it.rect;
            }
        }
    }

    fn viewer_step(&mut self, d: i32) {
        let Some(v) = self.viewer.as_mut() else { return };
        let Some(pos) = v.list.iter().position(|&x| x == v.id) else { return };
        let next = pos as i32 + d;
        if next < 0 || next as usize >= v.list.len() {
            return;
        }
        let id = v.list[next as usize];
        v.id = id;
        v.key = self.engine.shared.idx.read().unwrap().entries.get(id).map(|e| engine::key(&e.path)).unwrap_or(0);
        v.bmp = None;
        v.t = 1.0;
        if let Some(it) = self.field.find(id) {
            v.from = it.rect;
        }
        let long = (self.w.max(self.h) * self.scale).min(3000.0) as u32;
        self.engine.full(id, long);
    }

    fn viewer_path(&self) -> Option<String> {
        let v = self.viewer.as_ref()?;
        self.engine.shared.idx.read().unwrap().entries.get(v.id).map(|e| e.path.clone())
    }

    fn image_rect(&self, aspect: f32) -> Rect {
        let panel = if self.w >= 900.0 { 400.0 } else { 0.0 };
        let area = Rect::new(32.0, 32.0, self.w - panel - 64.0, self.h - 104.0);
        let w = area.w.min(area.h * aspect);
        let h = w / aspect;
        Rect::new(area.x + (area.w - w) / 2.0, area.y + (area.h - h) / 2.0, w, h)
    }

    fn info_rect(&self) -> Rect {
        if self.w >= 900.0 {
            Rect::new(self.w - 380.0 - 20.0, 20.0, 380.0, self.h - 40.0)
        } else {
            Rect::new(16.0, self.h - 230.0, self.w - 32.0, 214.0)
        }
    }

    // ---- models and settings -------------------------------------------------------

    fn use_model(&mut self, id: &str) {
        self.cfg.model = id.to_string();
        self.cfg.save();
        self.tex.clear();
        self.asked.clear();
        self.field = Field::new();
        self.field.quiet = self.quiet();
        self.crowd_for = usize::MAX;
        self.sent = None;
        self.typed_at = Some(Instant::now());
        self.relayout();
        self.engine.reload(self.cfg.clone());
    }

    fn act(&mut self, a: Act) {
        match a {
            Act::Tab(m) => {
                self.mode = if self.mode == m { Mode::Search } else { m };
                self.scroll = 0.0;
                self.refresh_installed();
            }
            Act::Close => {
                if self.viewer.is_some() {
                    self.close_viewer();
                } else {
                    self.mode = Mode::Search;
                }
            }
            Act::Opt(o) => self.toggle(o),
            Act::AddFolder => {
                if let Some(p) = platform::pick_folder(self.hwnd) {
                    if self.cfg.folders.is_empty() {
                        self.cfg.folders = self.default_roots.clone();
                    }
                    if !self.cfg.folders.iter().any(|f| f.eq_ignore_ascii_case(&p)) {
                        self.cfg.folders.push(p);
                    }
                    self.cfg.save();
                    self.engine.reload(self.cfg.clone());
                }
            }
            Act::RemoveFolder(i) => {
                if self.cfg.folders.is_empty() {
                    self.cfg.folders = self.default_roots.clone();
                }
                if i < self.cfg.folders.len() {
                    self.cfg.folders.remove(i);
                }
                if self.cfg.folders.is_empty() {
                    // nothing left would mean "the defaults": keep it empty on purpose
                    self.cfg.folders.push(String::from("\u{0}"));
                }
                self.cfg.save();
                self.engine.reload(self.cfg.clone());
            }
            Act::Download(id) => {
                if self.download.is_none() {
                    if let Some(m) = models::get(id) {
                        let s = self.engine.shared.clone();
                        self.download = Some(Download::start(m, self.cpu(), move || s.send(Event::Download)));
                    }
                }
            }
            Act::Cancel => {
                if let Some(d) = &self.download {
                    d.cancel();
                }
            }
            Act::Use(id) => self.use_model(id),
            Act::Remove(id) => {
                if let Some(m) = models::get(id) {
                    if self.cfg.model != id {
                        m.remove();
                    }
                    self.refresh_installed();
                }
            }
            Act::Open => {
                if let Some(p) = self.viewer_path() {
                    platform::open(&p);
                }
            }
            Act::Reveal => {
                if let Some(p) = self.viewer_path() {
                    platform::reveal(&p);
                }
            }
            Act::Copy => {
                if let Some(p) = self.viewer_path() {
                    if platform::copy_file(self.hwnd, &p) {
                        if let Some(v) = self.viewer.as_mut() {
                            v.copied = Some(Instant::now());
                        }
                    }
                }
            }
            Act::Similar => {
                if let Some(id) = self.viewer.as_ref().map(|v| v.id) {
                    self.find_similar(id);
                }
            }
            Act::Prev => self.viewer_step(-1),
            Act::Next => self.viewer_step(1),
            Act::Update => self.start_update(),
        }
        self.dirty = true;
    }

    fn toggle(&mut self, o: Opt) {
        let c = &mut self.cfg;
        let mut reload = false;
        match o {
            Opt::ReadText => c.read_text = !c.read_text,
            Opt::Cloud => {
                c.skip_cloud = !c.skip_cloud;
                reload = true;
            }
            Opt::Field => {
                c.field = match c.field {
                    FieldMode::Full => FieldMode::Quiet,
                    FieldMode::Quiet => FieldMode::Off,
                    FieldMode::Off => FieldMode::Full,
                }
            }
            Opt::Glass => c.glass = !c.glass,
            Opt::Animations => c.animations = !c.animations,
            Opt::Device => {
                c.device = if c.device == Device::Auto { Device::Cpu } else { Device::Auto };
                reload = true;
            }
            Opt::Autostart => {
                c.start_with_windows = !c.start_with_windows;
                settings::set_autostart(c.start_with_windows);
            }
            Opt::Updates => c.check_updates = !c.check_updates,
            Opt::Reread => {
                if let Some(m) = models::get(&c.model) {
                    let _ = std::fs::remove_dir_all(crate::index::index_dir(&m.id));
                }
                reload = true;
            }
        }
        self.cfg.save();
        self.field.quiet = self.quiet();
        self.crowd_for = usize::MAX;
        self.refresh_crowd();
        if reload {
            self.tex.clear();
            self.asked.clear();
            self.engine.reload(self.cfg.clone());
        } else {
            self.engine.configure(self.cfg.clone());
        }
    }

    fn start_update(&mut self) {
        let Some(r) = self.update.clone() else { return };
        if !crate::update::self_updates() || r.setup.is_none() {
            platform::open_url(&r.page);
            return;
        }
        if self.updating.is_some() {
            return;
        }
        self.updating = Some(0.0);
        self.update_failed = false;
        let s = self.engine.shared.clone();
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let mut last = 0;
            let setup = crate::update::download(&r, |f| {
                let p = (f * 100.0) as i32;
                if p != last {
                    last = p;
                    s.send(Event::UpdateProgress(f));
                }
            });
            match setup {
                Some(p) if crate::update::install(&p) => unsafe {
                    PostMessageW(Some(HWND(hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0)).ok();
                },
                _ => s.send(Event::UpdateFailed),
            }
        });
    }

    // ---- input -----------------------------------------------------------------------

    fn key(&mut self, vk: VIRTUAL_KEY) -> bool {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        if self.viewer.as_ref().is_some_and(|v| !v.closing) {
            match vk {
                VK_ESCAPE => self.close_viewer(),
                VK_LEFT => self.viewer_step(-1),
                VK_RIGHT => self.viewer_step(1),
                VK_RETURN => self.act(Act::Open),
                _ if ctrl && vk.0 == b'C' as u16 => self.act(Act::Copy),
                _ if !ctrl && vk.0 == b'S' as u16 => self.act(Act::Similar),
                _ => return false,
            }
            self.dirty = true;
            return true;
        }
        if self.mode != Mode::Search {
            match vk {
                VK_ESCAPE => self.mode = Mode::Search,
                VK_DOWN => self.scroll += 48.0,
                VK_UP => self.scroll -= 48.0,
                _ => return false,
            }
            self.dirty = true;
            return true;
        }
        let results = self.field.results();
        match vk {
            VK_ESCAPE => {
                if !self.edit.text.is_empty() {
                    self.edit.clear();
                    self.changed_text();
                }
            }
            VK_RETURN => {
                let pick = self.sel.and_then(|s| results.get(s).copied()).or(results.first().copied());
                if let Some(id) = pick {
                    self.open_viewer(id);
                }
            }
            VK_DOWN | VK_RIGHT | VK_TAB if !results.is_empty() && (vk != VK_RIGHT || self.sel.is_some()) => {
                self.sel = Some(self.sel.map_or(0, |s| (s + 1).min(results.len() - 1)));
            }
            VK_UP | VK_LEFT if self.sel.is_some() => {
                self.sel = self.sel.and_then(|s| s.checked_sub(1));
            }
            VK_BACK => {
                self.edit.backspace(ctrl);
                self.changed_text();
            }
            VK_DELETE => {
                self.edit.delete(ctrl);
                self.changed_text();
            }
            VK_LEFT => self.edit.left(ctrl),
            VK_RIGHT => self.edit.right(ctrl),
            VK_HOME => self.edit.home(),
            VK_END => self.edit.end(),
            _ if ctrl && vk.0 == b'A' as u16 => self.edit.select_all(),
            _ if ctrl && vk.0 == b'V' as u16 => {
                if let Some(t) = clipboard_text(self.hwnd) {
                    self.edit.insert(t.lines().next().unwrap_or(""));
                    self.changed_text();
                }
            }
            _ if ctrl && vk.0 == 0xBC => self.act(Act::Tab(Mode::Settings)), // Ctrl+,
            _ => return false,
        }
        self.caret_on = true;
        self.dirty = true;
        true
    }

    fn char_in(&mut self, c: char) {
        if c.is_control() || self.mode != Mode::Search || self.viewer.is_some() {
            return;
        }
        let mut buf = [0u8; 4];
        self.edit.insert(c.encode_utf8(&mut buf));
        self.changed_text();
    }

    fn click(&mut self, x: f32, y: f32) {
        if let Some(a) = self.controls.iter().rev().find(|(r, _)| r.contains(x, y)).map(|c| c.1.clone()) {
            self.act(a);
            return;
        }
        if self.viewer.is_some() {
            // clicking outside the photo and the panel closes it
            let inside = self.viewer.as_ref().and_then(|v| v.bmp.as_ref().map(|b| self.image_rect(b.1 as f32 / b.2 as f32))).is_some_and(|r| r.contains(x, y)) || self.info_rect().contains(x, y);
            if !inside {
                self.close_viewer();
            }
            return;
        }
        if self.mode != Mode::Search {
            return;
        }
        if let Some(id) = self.field.hit(x, y).map(|it| it.id) {
            self.open_viewer(id);
        }
    }

    pub fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let pt = || ((lp.0 & 0xFFFF) as i16 as f32, ((lp.0 >> 16) & 0xFFFF) as i16 as f32);
        match msg {
            engine::WM_APP_EVENT => self.poll(),
            WM_SIZE => {
                let (pw, ph) = ((lp.0 & 0xFFFF) as u32, ((lp.0 >> 16) & 0xFFFF) as u32);
                if pw > 0 && ph > 0 {
                    if let Some(g) = self.gfx.as_mut() {
                        g.resize(pw, ph, self.scale).ok();
                    }
                    self.w = pw as f32 / self.scale;
                    self.h = ph as f32 / self.scale;
                    self.relayout();
                    self.dirty = true;
                    self.photos_dirty = true;
                    self.tick();
                }
                return Some(LRESULT(0));
            }
            WM_DPICHANGED => {
                self.scale = ((wp.0 & 0xFFFF) as f32 / 96.0).max(1.0);
                let r = unsafe { &*(lp.0 as *const RECT) };
                unsafe { SetWindowPos(self.hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE).ok() };
                return Some(LRESULT(0));
            }
            WM_GETMINMAXINFO => {
                let mm = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
                mm.ptMinTrackSize = POINT { x: (640.0 * self.scale) as i32, y: (480.0 * self.scale) as i32 };
                return Some(LRESULT(0));
            }
            WM_ACTIVATE => {
                self.active = (wp.0 & 0xFFFF) != 0;
                if self.active {
                    self.field.wake();
                }
                self.dirty = true;
            }
            WM_TIMER if wp.0 == TIMER_SEARCH => {
                unsafe { KillTimer(Some(self.hwnd), TIMER_SEARCH).ok() };
                self.maybe_search();
                self.dirty = true;
            }
            WM_TIMER if wp.0 == TIMER_CARET => {
                // blink only while the window is in front
                if self.active || !self.caret_on {
                    self.caret_on = self.active && !self.caret_on;
                    self.dirty = true;
                }
            }
            WM_TIMER if wp.0 == TIMER_PH => {
                if self.typewriter() {
                    self.dirty = true;
                }
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                if self.key(VIRTUAL_KEY(wp.0 as u16)) {
                    return Some(LRESULT(0));
                }
            }
            WM_CHAR => {
                if let Some(c) = char::from_u32(wp.0 as u32) {
                    self.char_in(c);
                }
                return Some(LRESULT(0));
            }
            WM_MOUSEMOVE => {
                let (x, y) = pt();
                let (x, y) = (x / self.scale, y / self.scale);
                self.mouse = (x, y);
                self.field.pointer(x, y);
                if !self.tracking {
                    let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
                    unsafe { TrackMouseEvent(&mut t).ok() };
                    self.tracking = true;
                }
                let hot = self.controls.iter().rev().find(|(r, _)| r.contains(x, y)).map(|c| c.1.clone());
                if hot != self.hot {
                    self.hot = hot;
                }
                self.dirty = true;
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.mouse = (-1.0, -1.0);
                self.field.pointer_left();
                self.hot = None;
                self.dirty = true;
            }
            WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
                let (x, y) = self.mouse;
                let hand = self.hot.is_some() || (self.viewer.is_none() && self.mode == Mode::Search && self.field.hit(x, y).is_some());
                let bar = self.mode == Mode::Search && self.viewer.is_none() && self.bar_rect(self.dock).contains(x, y);
                let c = if hand { IDC_HAND } else if bar { IDC_IBEAM } else { IDC_ARROW };
                unsafe { SetCursor(LoadCursorW(None, c).ok()) };
                return Some(LRESULT(1));
            }
            WM_LBUTTONUP => {
                let (x, y) = pt();
                self.click(x / self.scale, y / self.scale);
                return Some(LRESULT(0));
            }
            WM_MOUSEWHEEL => {
                let d = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                if self.mode != Mode::Search {
                    self.scroll = (self.scroll - d * 60.0).max(0.0);
                } else if self.viewer.is_some() {
                    self.viewer_step(if d < 0.0 { 1 } else { -1 });
                }
                self.dirty = true;
                return Some(LRESULT(0));
            }
            WM_CLOSE => {
                self.save_window();
                unsafe { DestroyWindow(self.hwnd).ok() };
                return Some(LRESULT(0));
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                return Some(LRESULT(0));
            }
            WM_ERASEBKGND => return Some(LRESULT(1)),
            _ => {}
        }
        None
    }

    fn save_window(&mut self) {
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        unsafe { GetWindowPlacement(self.hwnd, &mut wp).ok() };
        let r = wp.rcNormalPosition;
        self.cfg.window = Some([r.left, r.top, r.right - r.left, r.bottom - r.top]);
        self.cfg.maximized = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32;
        self.cfg.save();
    }

    // ---- drawing ----------------------------------------------------------------------

    pub fn tick(&mut self) {
        if self.gfx.is_none() {
            self.ensure_gfx();
        }
        let now = Instant::now();
        let dt = self.last.map_or(16.0, |l| (now - l).as_secs_f32() * 1000.0).clamp(1.0, 64.0);
        self.last = Some(now);
        self.dirty = false;
        self.maybe_search();
        let animate = self.animate();
        let k = |tau: f32| if animate { 1.0 - (-dt / tau).exp() } else { 1.0 };
        let (kd, kp) = (k(260.0), k(200.0));
        // Eased, then snapped home once the rest is too small to see, so a
        // transition doesn't spend a second of frames on its last half-percent.
        let snap = |v: &mut f32, target: f32, k: f32| {
            *v += (target - *v) * k;
            if (target - *v).abs() < 0.005 {
                *v = target;
            }
        };
        let (dock_to, panel_to) = (if self.docked() { 1.0 } else { 0.0 }, if self.mode != Mode::Search { 1.0 } else { 0.0 });
        snap(&mut self.dock, dock_to, kd);
        snap(&mut self.panel, panel_to, kp);
        let kv = k(170.0);
        if let Some(v) = self.viewer.as_mut() {
            let target = if v.closing { 0.0 } else { 1.0 };
            v.t += (target - v.t) * kv;
            if (target - v.t).abs() < 0.005 {
                v.t = target;
            }
            if v.closing && v.t <= 0.0 {
                self.viewer = None;
            }
        }
        let vt = self.viewer.as_ref().map_or(0.0, |v| ease_out(v.t));
        // The viewer pushes the photos back; a panel only dims them (a uniform
        // in the glass pass), so opening one redraws no photos at all.
        self.field.depth = 1.0 - 0.75 * vt;
        self.moving = self.field.step(self.t0.elapsed().as_secs_f64() * 1000.0, dt, self.animate());
        self.want_textures();

        let mut gfx = match self.gfx.take() {
            Some(g) => g,
            None => return,
        };
        let quads: Vec<Quad> = self.field.draw_order().into_iter().filter_map(|it| self.tex.get(&it.key).map(|t| Quad { tex: t, rect: it.rect, blur: it.blur(), gray: it.gray(), dim: it.dim() })).collect();
        // Redraw the photos only when one of them changed by a visible amount
        // (a quarter pixel, a sliver of blur or brightness).
        let sig: Vec<[i32; 6]> = quads
            .iter()
            .map(|q| [(q.rect.x * 4.0) as i32, (q.rect.y * 4.0) as i32, (q.rect.w * 4.0) as i32, (q.blur * 20.0) as i32, (q.dim * 100.0) as i32, (q.gray * 20.0) as i32])
            .collect();
        let photos = std::mem::take(&mut self.photos_dirty) || sig != self.last_sig;
        self.last_sig = sig;
        let bar = self.bar_rect(ease_out(self.dock));
        let bar_alpha = (1.0 - self.panel) * (1.0 - vt);
        let mut glass = vec![];
        if self.cfg.glass && bar_alpha > 0.01 {
            glass.push(Glass { rect: bar, radius: lerp(26.0, 20.0, self.dock), alpha: bar_alpha, lens: 26.0 });
        }
        if self.panel > 0.01 {
            glass.push(Glass { rect: self.panel_rect(), radius: 26.0, alpha: self.panel.min(1.0), lens: 5.0 });
        }
        if vt > 0.01 && self.cfg.glass {
            glass.push(Glass { rect: self.info_rect(), radius: 24.0, alpha: vt, lens: 7.0 });
        }
        if self.status.phase == Phase::NoModel && self.mode == Mode::Search && self.viewer.is_none() && self.cfg.glass {
            glass.push(Glass { rect: self.nomodel_rect(bar), radius: 22.0, alpha: bar_alpha, lens: 10.0 });
        }
        let light = self.field.light();
        let scene_dim = 1.0 - 0.55 * self.panel.max(vt);
        let mut controls = vec![];
        let t_draw = Instant::now();
        let logic_ms = (t_draw - now).as_secs_f32() * 1000.0;
        let r = gfx.frame(&quads, &glass, light, scene_dim, photos, |g| self.draw(g, bar, bar_alpha, vt, &mut controls));
        drop(quads);
        if let Err(e) = r {
            crate::log(format!("frame: {e}"));
            // the device was lost (driver update, GPU reset): start over
            self.tex.clear();
            self.asked.clear();
            self.gfx = None;
            self.dirty = true;
            return;
        }
        self.gfx = Some(gfx);
        self.controls = controls;
        if std::env::var_os("MUNINN_DEBUG").is_some() {
            let st = &mut self.stats;
            st.1 += 1;
            st.2 += logic_ms;
            st.3 += t_draw.elapsed().as_secs_f32() * 1000.0;
            st.4 += photos as u32;
            let since = *st.0.get_or_insert(now);
            if since.elapsed().as_secs_f32() >= 1.0 {
                crate::log(format!("frames {} · logic {:.2} ms · draw {:.2} ms per frame · photo passes {}", st.1, st.2 / st.1 as f32, st.3 / st.1 as f32, st.4));
                *st = (Some(Instant::now()), 0, 0.0, 0.0, 0);
            }
        }
    }

    /// One step of the example searches typing themselves out. Returns
    /// whether the placeholder changed.
    fn typewriter(&mut self) -> bool {
        if !self.ph_running() {
            return false;
        }
        let (mut li, ci, mut dir, mut rounds) = self.ph;
        let n = PLACEHOLDERS[li % PLACEHOLDERS.len()].chars().count() as i32;
        // below -1, dir counts the ticks left to hold a finished line
        if dir < -1 {
            self.ph = (li, ci, dir + 1, rounds);
            return false;
        }
        let mut ci = ci as i32;
        if dir == 1 {
            ci += 1;
            if ci >= n {
                ci = n;
                dir = -36; // hold about two seconds, then erase
            }
        } else {
            ci -= 1;
            if ci <= 0 {
                ci = 0;
                dir = 1;
                li += 1;
                if li % PLACEHOLDERS.len() == 0 {
                    rounds += 1;
                }
            }
        }
        self.ph = (li, ci.max(0) as usize, dir, rounds);
        true
    }

    fn panel_rect(&self) -> Rect {
        let w = (self.w - 48.0).min(720.0);
        let rise = (1.0 - ease_out(self.panel)) * 24.0;
        Rect::new((self.w - w) / 2.0, 84.0 + rise, w, self.h - 124.0)
    }

    fn nomodel_rect(&self, bar: Rect) -> Rect {
        Rect::new(bar.x, bar.y + bar.h + 16.0, bar.w, 150.0)
    }

    fn button(&self, g: &Gfx, r: Rect, label: &str, primary: bool, act: Act, controls: &mut Vec<(Rect, Act)>, alpha: f32) {
        let hot = self.hot.as_ref() == Some(&act);
        if primary {
            g.fill_round(r, 11.0, Color::gray(if hot { 1.0 } else { 0.93 }, alpha));
            g.text_center(label, &g.fonts.body_medium, r, Color::gray(0.05, alpha));
        } else {
            g.fill_round(r, 11.0, Color::gray(1.0, if hot { 0.10 } else { 0.06 } * alpha));
            g.stroke_round(r, 11.0, Color::gray(1.0, 0.08 * alpha));
            g.text_center(label, &g.fonts.body_medium, r, FG.alpha(alpha));
        }
        controls.push((r, act));
    }

    /// A round icon button, right edge at `x`. Lit while hovered or while its
    /// tab is open.
    fn icon_button(&self, g: &Gfx, x: f32, y: f32, glyph: &str, act: Act, controls: &mut Vec<(Rect, Act)>, a: f32) -> f32 {
        let r = Rect::new(x - 36.0, y, 36.0, 36.0);
        let open = matches!((&act, self.mode), (Act::Tab(m), cur) if *m == cur);
        let hot = self.hot.as_ref() == Some(&act);
        if open || hot {
            g.fill_round(r, 18.0, Color::gray(1.0, if open { 0.12 } else { 0.07 } * a));
        }
        if open {
            g.stroke_round(r, 18.0, Color::gray(1.0, 0.14 * a));
        }
        g.text_center(glyph, &g.fonts.icon, r, if open || hot { FG.alpha(a) } else { MID.alpha(a) });
        controls.push((r, act));
        r.w
    }

    fn text_button(&self, g: &Gfx, x: f32, y: f32, label: &str, act: Act, controls: &mut Vec<(Rect, Act)>, right: bool) -> f32 {
        let w = g.measure(label, &g.fonts.body) + 20.0;
        let r = if right { Rect::new(x - w, y, w, 32.0) } else { Rect::new(x, y, w, 32.0) };
        let on = self.hot.as_ref() == Some(&act) || matches!((&act, self.mode), (Act::Tab(a), b) if *a == b);
        if on {
            g.fill_round(r, 10.0, Color::gray(1.0, 0.07));
        }
        g.text_center(label, &g.fonts.body, r, if on { FG } else { MID });
        controls.push((r, act));
        r.w
    }

    fn draw(&self, g: &Gfx, bar: Rect, bar_alpha: f32, vt: f32, controls: &mut Vec<(Rect, Act)>) {
        // progress line while reading photos
        if let Phase::Reading { done, total } = self.status.phase {
            if total > 0 {
                g.fill(Rect::new(0.0, 0.0, self.w * done as f32 / total as f32, 2.0), Color::gray(0.85, 0.9));
            }
        }
        if let Some(f) = self.updating {
            g.fill(Rect::new(0.0, 0.0, self.w * f, 2.0), Color::gray(0.85, 0.9));
        }
        let top_alpha = 1.0 - vt;
        if top_alpha > 0.01 {
            self.draw_top(g, controls, top_alpha);
        }
        if bar_alpha > 0.01 {
            self.draw_bar(g, bar, bar_alpha, controls);
        }
        if self.panel > 0.01 {
            match self.mode {
                Mode::Settings => self.draw_settings(g, controls),
                Mode::Models => self.draw_models(g, controls),
                Mode::Search => {}
            }
        }
        if self.viewer.is_none() && self.mode == Mode::Search {
            self.draw_overlays(g);
        }
        if vt > 0.001 {
            self.draw_viewer(g, vt, controls);
        }
    }

    fn draw_top(&self, g: &Gfx, controls: &mut Vec<(Rect, Act)>, a: f32) {
        // wordmark: the raven, then the name
        g.raven(22.0, 24.0, 22.0, FG.alpha(a));
        g.text("muninn", &g.fonts.word, Rect::new(50.0, 22.0, 120.0, 24.0), FG.alpha(a));

        let mut x = self.w - 18.0;
        x -= self.icon_button(g, x, 16.0, "\u{E8F1}", Act::Tab(Mode::Models), controls, a) + 6.0;
        x -= self.icon_button(g, x, 16.0, "\u{E713}", Act::Tab(Mode::Settings), controls, a) + 14.0;
        let status = match &self.status.phase {
            Phase::Reading { done, total } => format!("Reading photos  {} / {}", platform::thousands(*done), platform::thousands(*total)),
            Phase::Loading => "Starting the model".to_string(),
            Phase::Failed(_) => "Something went wrong".to_string(),
            Phase::NoModel => String::new(),
            Phase::Ready => {
                let mut s = format!("{} photos", platform::thousands(self.status.count));
                if self.status.cloud > 0 {
                    s += &format!("  ·  {} in the cloud", platform::thousands(self.status.cloud));
                }
                s
            }
        };
        if let Some(r) = &self.update {
            let label = match (self.updating, self.update_failed) {
                (Some(f), _) => format!("Downloading {}  {:.0}%", r.version, f * 100.0),
                (None, true) => format!("Couldn't download {} · Try again", r.version),
                (None, false) => format!("Muninn {} is out · Update", r.version),
            };
            x -= self.text_button(g, x, 18.0, &label, Act::Update, controls, true) + 10.0;
        }
        let sw = g.measure(&status, &g.fonts.small);
        if self.w > 900.0 || self.update.is_none() {
            g.text(&status, &g.fonts.small, Rect::new(x - sw - 2.0, 18.0, sw + 4.0, 32.0), MID.alpha(a));
        }
    }

    fn draw_bar(&self, g: &Gfx, bar: Rect, a: f32, controls: &mut Vec<(Rect, Act)>) {
        if !self.cfg.glass {
            g.fill_round(bar, 22.0, Color::gray(0.10, a));
            g.stroke_round(bar, 22.0, Color::gray(1.0, 0.08 * a));
        }
        let pad = lerp(26.0, 20.0, self.dock);
        g.text("\u{E721}", &g.fonts.icon, Rect::new(bar.x + pad - 4.0, bar.y, 24.0, bar.h), MID.alpha(a));
        let tx = bar.x + pad + 34.0;
        let fmt = if self.dock > 0.5 { &g.fonts.query_small } else { &g.fonts.query };
        let meta_w = if self.meta.is_empty() { 0.0 } else { g.measure(&self.meta, &g.fonts.small) + 24.0 };
        let tr = Rect::new(tx, bar.y, bar.w - (tx - bar.x) - pad - meta_w, bar.h);
        if self.edit.text.is_empty() {
            let ph = if self.status.phase == Phase::NoModel {
                "Choose a model below to start".to_string()
            } else if self.animate() {
                PLACEHOLDERS[self.ph.0 % PLACEHOLDERS.len()].chars().take(self.ph.1).collect()
            } else {
                "Describe a photo".to_string()
            };
            g.text(&ph, fmt, tr, GHOST.alpha(a));
        } else {
            if self.edit.all && self.similar_of.is_none() {
                let w = g.measure(&self.edit.text, fmt);
                g.fill_round(Rect::new(tx - 2.0, bar.y + bar.h / 2.0 - 16.0, w + 4.0, 32.0), 6.0, Color::gray(1.0, 0.14 * a));
            }
            g.text(&self.edit.text, fmt, tr, FG.alpha(a));
        }
        if self.caret_on && self.mode == Mode::Search && self.viewer.is_none() && self.status.phase != Phase::NoModel {
            let cx = tx + g.caret_x(&self.edit.text, fmt, self.edit.caret16());
            let ch = if self.dock > 0.5 { 24.0 } else { 30.0 };
            g.fill(Rect::new(cx, bar.y + (bar.h - ch) / 2.0, 1.6, ch), FG.alpha(a));
        }
        if !self.meta.is_empty() {
            g.text_right(&self.meta, &g.fonts.small, Rect::new(bar.x, bar.y, bar.w - pad, bar.h), MID.alpha(a));
        }
        // what the date words were understood as
        let mut x = bar.x + 4.0;
        let y = bar.y + bar.h + 14.0;
        for c in &self.understood.chips {
            let w = g.measure(c, &g.fonts.body_medium) + 46.0;
            let r = Rect::new(x, y, w, 32.0);
            g.fill_round(r, 16.0, Color::gray(1.0, 0.05 * a));
            g.stroke_round(r, 16.0, Color::gray(1.0, 0.07 * a));
            g.circle(x + 17.0, y + 16.0, 11.5, Color::gray(1.0, 0.07 * a), None);
            g.text("\u{E787}", &g.fonts.icon, Rect::new(x + 5.0, y, 24.0, 32.0), MID.alpha(a));
            g.text(c, &g.fonts.body_medium, Rect::new(x + 34.0, y, w - 40.0, 32.0), FG.alpha(a));
            x += w + 8.0;
        }
        if self.status.phase == Phase::NoModel && self.mode == Mode::Search {
            let r = self.nomodel_rect(bar);
            if !self.cfg.glass {
                g.fill_round(r, 22.0, Color::gray(0.10, a));
            }
            g.text("Muninn needs a search model", &g.fonts.heading, Rect::new(r.x + 24.0, r.y + 18.0, r.w - 48.0, 28.0), FG.alpha(a));
            g.paragraph(
                "It's what understands what's in your photos, and it runs entirely on this PC. Pick one in Models: the Fast one is about 300 MB and a good start.",
                &g.fonts.body,
                Rect::new(r.x + 24.0, r.y + 50.0, r.w - 220.0, 80.0),
                MID.alpha(a),
            );
            self.button(g, Rect::new(r.x + r.w - 184.0, r.y + r.h - 64.0, 160.0, 42.0), "Choose a model", true, Act::Tab(Mode::Models), controls, a);
        }
        if let Phase::Failed(msg) = &self.status.phase {
            let r = self.nomodel_rect(bar);
            g.paragraph(&format!("Muninn couldn't start its model: {msg}. Try the CPU in Settings, or download the model again."), &g.fonts.body, Rect::new(r.x + 8.0, r.y, r.w - 16.0, 80.0), MID.alpha(a));
        }
    }

    /// Captions on hover, the keyboard selection, and "nothing found".
    fn draw_overlays(&self, g: &Gfx) {
        let results = self.field.results();
        if let Some(id) = self.sel.and_then(|s| results.get(s).copied()) {
            if let Some(it) = self.field.find(id) {
                g.stroke_round(it.rect.inset(-4.0, -4.0), 15.0, Color::gray(1.0, 0.85));
            }
        }
        let (mx, my) = self.mouse;
        let hovered = self.hot.is_none().then(|| self.field.hit(mx, my)).flatten().filter(|it| it.sharp());
        let caption = hovered.map(|it| it.id).or(self.sel.and_then(|s| results.get(s).copied()));
        if let Some(it) = caption.and_then(|id| self.field.find(id)) {
            let name = self.engine.shared.idx.read().unwrap().entries.get(it.id).map(|e| platform::file_name(&e.path).to_string()).unwrap_or_default();
            let w = (g.measure(&name, &g.fonts.small) + 20.0).min(it.rect.w - 16.0);
            let r = Rect::new(it.rect.x + 8.0, it.rect.y + it.rect.h - 36.0, w, 28.0);
            g.fill_round(r, 8.0, Color::rgba(0.05, 0.05, 0.055, 0.72));
            g.text(&name, &g.fonts.small, r.inset(10.0, 0.0), FG);
        }
        if self.docked() && self.meta.is_empty() && self.sent.is_some() && self.typed_at.is_none() && self.status.phase == Phase::Ready {
            g.text_center("Nothing matches that yet. Try fewer words.", &g.fonts.body, Rect::new(0.0, self.h * 0.46, self.w, 30.0), MID);
        }
    }

    fn panel_header(&self, g: &Gfx, p: Rect, title: &str, controls: &mut Vec<(Rect, Act)>, a: f32) {
        g.text(title, &g.fonts.title, Rect::new(p.x + 28.0, p.y + 22.0, p.w - 100.0, 36.0), FG.alpha(a));
        let c = Rect::new(p.x + p.w - 56.0, p.y + 22.0, 34.0, 34.0);
        if self.hot == Some(Act::Close) {
            g.fill_round(c, 17.0, Color::gray(1.0, 0.08));
        }
        g.text("\u{E711}", &g.fonts.icon, c, MID.alpha(a));
        controls.push((c, Act::Close));
    }

    fn draw_settings(&self, g: &Gfx, controls: &mut Vec<(Rect, Act)>) {
        let a = self.panel;
        let p = self.panel_rect();
        if !self.cfg.glass {
            g.fill_round(p, 26.0, Color::gray(0.09, a));
        }
        self.panel_header(g, p, "Settings", controls, a);
        let body = Rect::new(p.x + 12.0, p.y + 76.0, p.w - 24.0, p.h - 88.0);
        g.push_clip(body);
        let mut y = body.y - self.scroll.min(1200.0);
        let row_h = 52.0;
        let section = |g: &Gfx, y: &mut f32, t: &str| {
            g.text(&t.to_uppercase(), &g.fonts.eyebrow, Rect::new(body.x + 16.0, *y + 8.0, body.w, 24.0), MID.alpha(a));
            *y += 36.0;
        };
        section(g, &mut y, "Folders");
        let folders: Vec<String> = if self.cfg.folders.is_empty() {
            self.default_roots.clone()
        } else {
            self.cfg.folders.iter().filter(|f| !f.starts_with('\u{0}')).cloned().collect()
        };
        for (i, f) in folders.iter().enumerate() {
            let r = Rect::new(body.x, y, body.w, 44.0);
            g.text("\u{E8B7}", &g.fonts.icon, Rect::new(r.x + 10.0, r.y, 24.0, r.h), MID.alpha(a));
            g.text(f, &g.fonts.body, Rect::new(r.x + 44.0, r.y, r.w - 160.0, r.h), FG.alpha(a));
            let rm = Rect::new(r.x + r.w - 92.0, r.y + 6.0, 84.0, 32.0);
            if self.hot == Some(Act::RemoveFolder(i)) {
                g.fill_round(rm, 10.0, Color::gray(1.0, 0.07));
            }
            g.text_center("Remove", &g.fonts.body, rm, MID.alpha(a));
            controls.push((rm, Act::RemoveFolder(i)));
            y += 44.0;
        }
        if folders.is_empty() {
            g.text("No folders. Add one to search its photos.", &g.fonts.body, Rect::new(body.x + 16.0, y, body.w, 40.0), MID.alpha(a));
            y += 40.0;
        }
        self.button(g, Rect::new(body.x + 8.0, y + 6.0, 150.0, 38.0), "Add a folder", false, Act::AddFolder, controls, a);
        y += 62.0;
        section(g, &mut y, "Options");
        let dev = match self.cfg.device {
            Device::Auto => self.status.device.clone().chars().take(34).collect::<String>().trim().to_string(),
            Device::Cpu => "Processor".to_string(),
        };
        let dev = if self.cfg.device == Device::Auto { format!("Graphics card{}", if dev.is_empty() || dev.starts_with("CPU") { String::new() } else { format!(" · {dev}") }) } else { dev };
        let on = |b: bool| if b { "On".to_string() } else { "Off".to_string() };
        let rows: Vec<(Opt, &str, String)> = vec![
            (Opt::ReadText, "Read text in photos", on(self.cfg.read_text)),
            (Opt::Cloud, "Photos only in OneDrive", if self.cfg.skip_cloud { "Leave in the cloud".into() } else { "Download and read".into() }),
            (Opt::Field, "Background photos", match self.cfg.field { FieldMode::Full => "Lots".into(), FieldMode::Quiet => "A few".into(), FieldMode::Off => "None".into() }),
            (Opt::Glass, "Glass", on(self.cfg.glass)),
            (Opt::Animations, "Animations", on(self.cfg.animations)),
            (Opt::Device, "Run the model on", dev),
            (Opt::Autostart, "Open at sign-in", on(self.cfg.start_with_windows)),
            (Opt::Updates, "Check for updates", on(self.cfg.check_updates)),
            (Opt::Reread, "Read every photo again", "Start".into()),
        ];
        for (o, label, value) in rows {
            let r = Rect::new(body.x, y, body.w, row_h);
            if self.hot == Some(Act::Opt(o)) {
                g.fill_round(r, 12.0, Color::gray(1.0, 0.05));
            }
            g.text(label, &g.fonts.body, Rect::new(r.x + 16.0, r.y, r.w * 0.55, r.h), FG.alpha(a));
            g.text_right(&value, &g.fonts.body, Rect::new(r.x, r.y, r.w - 16.0, r.h), MID.alpha(a));
            controls.push((r, Act::Opt(o)));
            y += row_h;
        }
        y += 16.0;
        g.paragraph(
            "Click a setting to change it. Muninn only reads the folders above, and nothing leaves this PC except a check for new versions, which you can turn off here.",
            &g.fonts.small,
            Rect::new(body.x + 16.0, y, body.w - 32.0, 60.0),
            MID.alpha(a),
        );
        g.pop_clip();
    }

    fn draw_models(&self, g: &Gfx, controls: &mut Vec<(Rect, Act)>) {
        let a = self.panel;
        let p = self.panel_rect();
        if !self.cfg.glass {
            g.fill_round(p, 26.0, Color::gray(0.09, a));
        }
        self.panel_header(g, p, "Models", controls, a);
        let body = Rect::new(p.x + 12.0, p.y + 76.0, p.w - 24.0, p.h - 88.0);
        g.push_clip(body);
        let mut y = body.y - self.scroll.min(600.0);
        let cpu = self.cpu();
        y += g.paragraph(
            "A model is what understands your photos. Each one downloads once and then runs entirely on this PC. Switching reads your photos again with the new model, and the old index is kept, so switching back is instant.",
            &g.fonts.body,
            Rect::new(body.x + 16.0, y, body.w - 32.0, 80.0),
            MID.alpha(a),
        ) + 18.0;
        for m in models::catalog() {
            let id: &'static str = m.id.as_str();
            let (installed, removable) = self.installed.get(models::catalog().iter().position(|x| x.id == m.id).unwrap_or(0)).copied().unwrap_or((false, false));
            let using = installed && self.cfg.model == m.id;
            let card = Rect::new(body.x, y, body.w, 128.0);
            g.fill_round(card, 16.0, Color::gray(1.0, if using { 0.07 } else { 0.035 } * a));
            g.stroke_round(card, 16.0, Color::gray(1.0, if using { 0.16 } else { 0.06 } * a));
            g.text(&m.name, &g.fonts.heading, Rect::new(card.x + 20.0, card.y + 16.0, 200.0, 26.0), FG.alpha(a));
            let nw = g.measure(&m.name, &g.fonts.heading);
            g.text(&format!("{}  ·  {}", m.arch, models::size_label(m.download_size(cpu))), &g.fonts.small, Rect::new(card.x + 32.0 + nw, card.y + 16.0, 260.0, 26.0), MID.alpha(a));
            g.paragraph(&m.blurb, &g.fonts.body, Rect::new(card.x + 20.0, card.y + 48.0, card.w - 230.0, 70.0), MID.alpha(a));
            let btn = Rect::new(card.x + card.w - 190.0, card.y + card.h - 58.0, 170.0, 40.0);
            match &self.download {
                Some(d) if d.id == m.id => {
                    let f = d.fraction();
                    match d.state() {
                        models::State::Failed(msg) => {
                            g.paragraph(&msg, &g.fonts.small, Rect::new(btn.x - 40.0, card.y + 16.0, 210.0, 40.0), MID.alpha(a));
                            self.button(g, btn, "Try again", true, Act::Download(id), controls, a);
                        }
                        _ => {
                            let track = Rect::new(btn.x, btn.y - 4.0, btn.w, 4.0);
                            g.fill_round(track, 2.0, Color::gray(1.0, 0.10 * a));
                            g.fill_round(Rect::new(track.x, track.y, track.w * f, 4.0), 2.0, Color::gray(0.9, a));
                            g.text(&format!("{:.0}%  ·  {} of {}", f * 100.0, models::size_label((f as f64 * d.total as f64) as u64), models::size_label(d.total)), &g.fonts.small, Rect::new(btn.x - 60.0, track.y - 30.0, btn.w + 60.0, 24.0), MID.alpha(a));
                            self.button(g, Rect::new(btn.x + 70.0, btn.y + 8.0, 100.0, 34.0), "Cancel", false, Act::Cancel, controls, a);
                        }
                    }
                }
                _ if using => {
                    g.text_right("In use", &g.fonts.body_medium, Rect::new(btn.x, btn.y, btn.w - 8.0, btn.h), FG.alpha(a));
                }
                _ if installed => {
                    self.button(g, Rect::new(btn.x + 70.0, btn.y, 100.0, btn.h), "Use", true, Act::Use(id), controls, a);
                    if removable {
                        let rm = Rect::new(btn.x - 30.0, btn.y, 90.0, btn.h);
                        if self.hot == Some(Act::Remove(id)) {
                            g.fill_round(rm, 10.0, Color::gray(1.0, 0.07));
                        }
                        g.text_center("Remove", &g.fonts.body, rm, MID.alpha(a));
                        controls.push((rm, Act::Remove(id)));
                    }
                }
                _ => {
                    let busy = self.download.is_some();
                    self.button(g, btn, &format!("Download  {}", models::size_label(m.download_size(cpu))), !busy, if busy { Act::Close } else { Act::Download(id) }, controls, if busy { a * 0.4 } else { a });
                    if busy {
                        controls.pop();
                    }
                }
            }
            y += card.h + 12.0;
        }
        if cpu {
            g.paragraph("This PC will run the model on its processor, so the downloads include the larger files that need.", &g.fonts.small, Rect::new(body.x + 16.0, y + 6.0, body.w - 32.0, 40.0), MID.alpha(a));
        }
        g.pop_clip();
    }

    fn draw_viewer(&self, g: &Gfx, vt: f32, controls: &mut Vec<(Rect, Act)>) {
        let Some(v) = &self.viewer else { return };
        let idx = self.engine.shared.idx.read().unwrap();
        let Some(e) = idx.entries.get(v.id) else { return };
        let aspect = v.bmp.as_ref().map(|b| b.1 as f32 / b.2 as f32).unwrap_or(engine::aspect(e));
        let target = self.image_rect(aspect);
        let r = lerp_rect(v.from, target, ease_out(v.t));
        match &v.bmp {
            Some((b, _, _)) => {
                g.push_clip_round(r, lerp(10.0, 16.0, vt));
                g.image(b, r, 1.0, v.t > 0.995 && !v.closing);
                g.pop_layer();
            }
            None => g.fill_round(r, 16.0, Color::gray(0.12, vt)),
        }
        if v.closing {
            return;
        }
        let info = self.info_rect();
        let a = vt;
        if !self.cfg.glass {
            g.fill_round(info, 24.0, Color::gray(0.09, a));
        }
        let x = info.x + 24.0;
        let w = info.w - 48.0;
        let mut y = info.y + 28.0;
        let close = Rect::new(info.x + info.w - 54.0, info.y + 16.0, 36.0, 36.0);
        if self.hot == Some(Act::Close) {
            g.fill_round(close, 18.0, Color::gray(1.0, 0.08));
        }
        g.text("\u{E711}", &g.fonts.icon, close, MID.alpha(a));
        controls.push((close, Act::Close));
        g.text(platform::file_name(&e.path), &g.fonts.title, Rect::new(x, y, w - 40.0, 34.0), FG.alpha(a));
        y += 46.0;
        let facts = [
            ("\u{E787}", if e.taken > 0 { platform::long_date(e.taken) } else { format!("{} (file date)", platform::long_date(e.when())) }),
            ("\u{E8B7}", platform::folder(&e.path).to_string()),
            ("\u{E91B}", format!("{} × {}  ·  {}", e.w, e.h, if e.size >= 1_000_000 { format!("{:.1} MB", e.size as f64 / 1e6) } else { format!("{} KB", e.size.div_ceil(1000)) })),
        ];
        for (icon, t) in facts {
            g.text(icon, &g.fonts.icon, Rect::new(x - 4.0, y, 24.0, 22.0), MID.alpha(a));
            let h = g.paragraph(&t, &g.fonts.body, Rect::new(x + 30.0, y + 1.0, w - 30.0, 60.0), MID.alpha(a));
            y += h.max(22.0) + 10.0;
        }
        if !e.text.is_empty() && self.w >= 900.0 {
            y += 8.0;
            g.text("TEXT IN THE PHOTO", &g.fonts.eyebrow, Rect::new(x, y, w, 20.0), MID.alpha(a));
            y += 24.0;
            let t: String = e.text.chars().take(220).collect();
            y += g.paragraph(&t, &g.fonts.small, Rect::new(x, y, w, 90.0), FG.alpha(a * 0.8)).min(90.0) + 12.0;
        }
        let by = if self.w >= 900.0 { (info.y + info.h - 150.0).max(y + 10.0) } else { info.y + info.h - 104.0 };
        self.button(g, Rect::new(x, by, w, 46.0), "Open", true, Act::Open, controls, a);
        let half = (w - 8.0) / 2.0;
        let copied = v.copied.is_some_and(|t| t.elapsed().as_millis() < 1500);
        self.button(g, Rect::new(x, by + 54.0, half, 40.0), if copied { "Copied" } else { "Copy" }, false, Act::Copy, controls, a);
        self.button(g, Rect::new(x + half + 8.0, by + 54.0, half, 40.0), "Show in folder", false, Act::Reveal, controls, a);
        if self.w >= 900.0 {
            self.button(g, Rect::new(x, by + 102.0, w, 40.0), "Find similar", false, Act::Similar, controls, a);
        }
        // previous / next under the photo
        if v.list.len() > 1 {
            let cx = target.x + target.w / 2.0;
            let yb = self.h - 60.0;
            for (dx, glyph, act) in [(-52.0, "\u{E76B}", Act::Prev), (8.0, "\u{E76C}", Act::Next)] {
                let r = Rect::new(cx + dx, yb, 44.0, 44.0);
                g.fill_round(r, 22.0, Color::gray(1.0, if self.hot.as_ref() == Some(&act) { 0.12 } else { 0.07 } * a));
                g.text(glyph, &g.fonts.icon, r, FG.alpha(a));
                controls.push((r, act));
            }
        }
    }
}

fn clipboard_text(hwnd: HWND) -> Option<String> {
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::*;
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let r = (|| {
            let h = GetClipboardData(13).ok()?; // CF_UNICODETEXT
            let p = GlobalLock(HGLOBAL(h.0)) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(HGLOBAL(h.0));
            Some(s)
        })();
        let _ = CloseClipboard();
        r
    }
}
