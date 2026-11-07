//! Everything off the UI thread. One thread loads the model and keeps the
//! index current (reading new photos, watching the folders); another answers
//! searches, newest first; image decodes run on the thread pool. Results come
//! back as events, and the window is poked to pick them up.

use crate::clip::Clip;
use crate::index::{self, Index};
use crate::models;
use crate::query::Understood;
use crate::settings::{Device, Settings};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

pub const WM_APP_EVENT: u32 = 0x8000 + 40;
/// Thumbnails for the field: small, so a hundred of them cost little.
pub const THUMB: u32 = 384;

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// No model is installed (or none is chosen).
    NoModel,
    Loading,
    Reading { done: usize, total: usize },
    Ready,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Status {
    pub phase: Phase,
    pub device: String,
    pub count: usize,
    pub cloud: usize,
}

pub enum Event {
    /// The status or the index changed.
    Status,
    Results { id: u64, hits: Vec<usize>, ms: u128 },
    Thumb { key: u64, px: Vec<u8>, w: u32, h: u32 },
    Full { key: u64, px: Vec<u8>, w: u32, h: u32 },
    Update(crate::update::Release),
    UpdateProgress(f32),
    UpdateFailed,
    /// A model download moved on.
    Download,
}

enum Cmd {
    Search { id: u64, q: Understood },
    Similar { id: u64, of: usize },
}

pub struct Shared {
    pub idx: RwLock<Index>,
    pub clip: RwLock<Option<Arc<Clip>>>,
    pub status: Mutex<Status>,
    cfg: Mutex<Settings>,
    events: Mutex<VecDeque<Event>>,
    hwnd: isize,
    gen: AtomicU64,
    /// Bumped whenever the index is replaced (entry numbers may have moved).
    pub version: AtomicU64,
}

/// A photo's identity that survives the index changing: its path, hashed.
pub fn key(path: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

impl Shared {
    pub fn send(&self, e: Event) {
        if let Ok(mut q) = self.events.lock() {
            q.push_back(e);
        }
        unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_APP_EVENT, WPARAM(0), LPARAM(0)).ok() };
    }

    fn set(&self, f: impl FnOnce(&mut Status)) {
        if let Ok(mut s) = self.status.lock() {
            f(&mut s);
        }
        self.send(Event::Status);
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or(Status { phase: Phase::Loading, device: String::new(), count: 0, cloud: 0 })
    }
}

pub struct Engine {
    pub shared: Arc<Shared>,
    tx: Sender<Cmd>,
}

impl Engine {
    pub fn start(hwnd: HWND, cfg: Settings) -> Engine {
        let shared = Arc::new(Shared {
            idx: RwLock::new(Index::default()),
            clip: RwLock::new(None),
            status: Mutex::new(Status { phase: Phase::Loading, device: String::new(), count: 0, cloud: 0 }),
            cfg: Mutex::new(cfg),
            events: Mutex::new(VecDeque::new()),
            hwnd: hwnd.0 as isize,
            gen: AtomicU64::new(1),
            version: AtomicU64::new(0),
        });
        let s = shared.clone();
        std::thread::Builder::new().name("indexer".into()).spawn(move || indexer(s)).ok();
        let (tx, rx) = channel();
        let s = shared.clone();
        std::thread::Builder::new().name("search".into()).spawn(move || searcher(s, rx)).ok();
        Engine { shared, tx }
    }

    /// Settings changed in a way that needs the model or the index reloaded.
    pub fn reload(&self, cfg: Settings) {
        if let Ok(mut c) = self.shared.cfg.lock() {
            *c = cfg;
        }
        self.shared.gen.fetch_add(1, Ordering::Relaxed);
    }

    /// Settings changed that only matter for the next read (e.g. text reading).
    pub fn configure(&self, cfg: Settings) {
        if let Ok(mut c) = self.shared.cfg.lock() {
            *c = cfg;
        }
    }

    pub fn events(&self) -> Vec<Event> {
        self.shared.events.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default()
    }

    pub fn search(&self, id: u64, q: Understood) {
        self.tx.send(Cmd::Search { id, q }).ok();
    }

    pub fn similar(&self, id: u64, of: usize) {
        self.tx.send(Cmd::Similar { id, of }).ok();
    }

    pub fn thumbs(&self, ids: Vec<usize>) {
        let s = self.shared.clone();
        let paths: Vec<(usize, String)> = {
            let idx = s.idx.read().unwrap();
            ids.into_iter().filter_map(|i| idx.entries.get(i).map(|e| (i, e.path.clone()))).collect()
        };
        rayon::spawn(move || {
            use rayon::prelude::*;
            paths.par_iter().for_each(|(_, p)| {
                let t = index::thumb_path(p);
                let img = if t.exists() { crate::decode::bgra(&t.to_string_lossy(), THUMB) } else { None }.or_else(|| crate::decode::bgra(p, THUMB));
                if let Some((px, w, h)) = img {
                    s.send(Event::Thumb { key: key(p), px, w, h });
                }
            });
        });
    }

    pub fn full(&self, id: usize, long: u32) {
        let s = self.shared.clone();
        let Some(p) = s.idx.read().unwrap().entries.get(id).map(|e| e.path.clone()) else { return };
        std::thread::spawn(move || {
            if let Some((px, w, h)) = crate::decode::bgra(&p, long) {
                s.send(Event::Full { key: key(&p), px, w, h });
            }
        });
    }

    /// A spread of photos for the resting field: mostly recent, some older.
    pub fn crowd(&self, n: usize) -> Vec<(usize, f32, u64)> {
        let idx = self.shared.idx.read().unwrap();
        let mut by_date: Vec<usize> = (0..idx.entries.len()).filter(|&i| idx.entries[i].w > 0).collect();
        by_date.sort_by_key(|&i| std::cmp::Reverse(idx.entries[i].when()));
        let recent = (n * 2 / 3).min(by_date.len());
        let mut out: Vec<usize> = by_date[..recent].to_vec();
        let rest = &by_date[recent..];
        let mut k = 0usize;
        while out.len() < n && k < rest.len() {
            // a fixed stride through the older ones, so it's the same crowd each time
            out.push(rest[(k * 7919) % rest.len()]);
            k += 1;
        }
        out.sort();
        out.dedup();
        out.into_iter().map(|i| (i, aspect(&idx.entries[i]), key(&idx.entries[i].path))).collect()
    }
}

pub fn aspect(e: &index::Entry) -> f32 {
    if e.h > 0 { e.w as f32 / e.h as f32 } else { 1.0 }
}

fn indexer(s: Arc<Shared>) {
    let mut loaded = 0u64;
    loop {
        let g = s.gen.load(Ordering::Relaxed);
        let cfg = s.cfg.lock().map(|c| c.clone()).unwrap_or_default();
        if g != loaded {
            loaded = g;
            *s.clip.write().unwrap() = None;
            let cpu = cfg.device == Device::Cpu || models::needs_cpu(false);
            let Some(m) = models::get(&cfg.model) else {
                *s.idx.write().unwrap() = Index::default();
                s.version.fetch_add(1, Ordering::Relaxed);
                s.set(|st| { st.phase = Phase::NoModel; st.count = 0; });
                wait_for_change(&s, g);
                continue;
            };
            let Some(dir) = m.location(cpu) else {
                *s.idx.write().unwrap() = Index::default();
                s.version.fetch_add(1, Ordering::Relaxed);
                s.set(|st| { st.phase = Phase::NoModel; st.count = 0; });
                wait_for_change(&s, g);
                continue;
            };
            s.set(|st| st.phase = Phase::Loading);
            match Clip::load(&dir, cfg.device == Device::Cpu) {
                Ok(c) => {
                    let idx = Index::load(&m.id, c.dim);
                    let n = idx.entries.len();
                    let dev = c.device.clone();
                    *s.idx.write().unwrap() = idx;
                    s.version.fetch_add(1, Ordering::Relaxed);
                    *s.clip.write().unwrap() = Some(Arc::new(c));
                    s.set(|st| { st.phase = Phase::Ready; st.device = dev; st.count = n; });
                }
                Err(e) => {
                    crate::log(format!("model: {e}"));
                    s.set(|st| st.phase = Phase::Failed(format!("{e}")));
                    wait_for_change(&s, g);
                    continue;
                }
            }
        }
        let Some(clip) = s.clip.read().unwrap().clone() else {
            wait_for_change(&s, g);
            continue;
        };
        // Watch first, so a photo saved during the scan still wakes us.
        let roots = cfg.roots();
        let watcher = crate::watch::Watch::new(&roots);
        let mut files = index::scan(&roots);
        if !cfg.skip_cloud {
            files.iter_mut().for_each(|f| f.cloud = false);
        }
        let old = s.idx.read().unwrap().clone();
        let mut last = Instant::now();
        let res = index::update(&clip, &old, files, cfg.read_text, |done, total, cur| {
            if total > 0 {
                s.set(|st| st.phase = Phase::Reading { done, total });
            }
            // let people search what's ready while a big first read runs
            if last.elapsed().as_secs_f32() > 1.5 && cur.entries.len() > old.entries.len() {
                let n = cur.entries.len();
                *s.idx.write().unwrap() = cur.clone();
                s.version.fetch_add(1, Ordering::Relaxed);
                s.set(|st| st.count = n);
                last = Instant::now();
            }
            s.gen.load(Ordering::Relaxed) == g
        });
        match res {
            Ok(idx) => {
                let (n, cloud) = (idx.entries.len(), idx.cloud_skipped);
                *s.idx.write().unwrap() = idx;
                s.version.fetch_add(1, Ordering::Relaxed);
                s.set(|st| { st.phase = Phase::Ready; st.count = n; st.cloud = cloud; });
            }
            Err(e) => s.set(|st| st.phase = Phase::Failed(e.to_string())),
        }
        // Until the folders change (or an hour passes, in case a folder was
        // added that the watch couldn't see), or the settings do.
        let t0 = Instant::now();
        while s.gen.load(Ordering::Relaxed) == g && t0.elapsed() < Duration::from_secs(3600) {
            if watcher.wait(Duration::from_secs(1)) {
                break;
            }
        }
    }
}

fn wait_for_change(s: &Shared, g: u64) {
    while s.gen.load(Ordering::Relaxed) == g {
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn searcher(s: Arc<Shared>, rx: Receiver<Cmd>) {
    while let Ok(mut cmd) = rx.recv() {
        // only the newest question matters
        while let Ok(c) = rx.try_recv() {
            cmd = c;
        }
        let t = Instant::now();
        let (id, hits) = match cmd {
            Cmd::Search { id, q } => (id, search(&s, &q)),
            Cmd::Similar { id, of } => {
                let idx = s.idx.read().unwrap();
                let hits = if of < idx.entries.len() && idx.dim > 0 {
                    let v = idx.vecs[of * idx.dim..(of + 1) * idx.dim].to_vec();
                    idx.search(&v, &[], crate::scene::MAX_RESULTS + 1).into_iter().map(|h| h.0).filter(|&i| i != of).collect()
                } else {
                    vec![]
                };
                (id, hits)
            }
        };
        if std::env::var_os("MUNINN_DEBUG").is_some() {
            crate::log(format!("search {id}: {} hits in {:?}", hits.len(), t.elapsed()));
        }
        s.send(Event::Results { id, hits, ms: t.elapsed().as_millis() });
    }
}

fn search(s: &Shared, q: &Understood) -> Vec<usize> {
    let k = crate::scene::MAX_RESULTS;
    let idx = s.idx.read().unwrap();
    if q.text.trim().is_empty() {
        // Only a date: the photos from then, newest first.
        let mut v: Vec<usize> = (0..idx.entries.len()).filter(|&i| q.accepts(idx.entries[i].when())).collect();
        v.sort_by_key(|&i| std::cmp::Reverse(idx.entries[i].when()));
        v.truncate(k);
        return v;
    }
    let Some(clip) = s.clip.read().unwrap().clone() else { return vec![] };
    let Ok(v) = clip.embed_text(&q.text) else { return vec![] };
    let words: Vec<String> = q.text.to_lowercase().split_whitespace().map(String::from).collect();
    let mut hits = idx.search_where(&v, &words, k * 3, |e| q.accepts(e.when()));
    // Weak matches are noise: keep those near the best one.
    if let Some(best) = hits.first().map(|h| h.1) {
        hits.retain(|h| h.1 > best - 0.09);
    }
    hits.truncate(k);
    hits.into_iter().map(|h| h.0).collect()
}
