//! The search models Muninn can use. None ships with the installer: each is
//! downloaded from the Models tab, checked against its SHA-256, and kept in
//! %LOCALAPPDATA%\Muninn\models\<id>. PCs without a usable GPU also fetch the
//! fp32 files the CPU needs.

use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const BASE: &str = "https://github.com/Kryzern/muninn/releases/download/models-v1";

#[derive(Deserialize, Debug)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub arch: String,
    /// Length of its embeddings (checked against the model when it loads).
    #[allow(dead_code)]
    pub dim: usize,
    pub blurb: String,
    pub files: Vec<File>,
}

#[derive(Deserialize, Debug)]
pub struct File {
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub cpu: bool,
}

pub fn catalog() -> &'static [Model] {
    static C: OnceLock<Vec<Model>> = OnceLock::new();
    C.get_or_init(|| serde_json::from_str(include_str!("models.json")).unwrap_or_default())
}

pub fn get(id: &str) -> Option<&'static Model> {
    catalog().iter().find(|m| m.id == id)
}

/// Does this PC need the CPU files (no DirectX 12 GPU, or the CPU was chosen)?
pub fn needs_cpu(force_cpu: bool) -> bool {
    force_cpu || !crate::gpu::adapters().iter().any(|a| !a.software)
}

impl Model {
    pub fn wanted(&self, cpu: bool) -> impl Iterator<Item = &File> {
        self.files.iter().filter(move |f| !f.cpu || cpu)
    }

    pub fn download_size(&self, cpu: bool) -> u64 {
        self.wanted(cpu).map(|f| f.size).sum()
    }

    pub fn dir(&self) -> PathBuf {
        crate::index::data_dir().join("models").join(&self.id)
    }

    /// Where its files are, if they're all there. A source checkout also finds
    /// the Fast model in its own models folder.
    pub fn location(&self, cpu: bool) -> Option<PathBuf> {
        let complete = |d: &PathBuf| self.wanted(cpu).all(|f| std::fs::metadata(d.join(&f.name)).is_ok_and(|m| m.len() == f.size));
        let mut dirs = vec![self.dir()];
        if self.id == "fast" {
            if let Ok(exe) = std::env::current_exe() {
                if let Some(p) = exe.parent() {
                    dirs.push(p.join("models"));
                    dirs.push(p.join("../../models"));
                }
            }
        }
        dirs.into_iter().find(complete)
    }

    pub fn installed(&self, cpu: bool) -> bool {
        self.location(cpu).is_some()
    }

    pub fn remove(&self) {
        let _ = std::fs::remove_dir_all(self.dir());
        let _ = std::fs::remove_dir_all(crate::index::data_dir().join("index").join(&self.id));
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Running,
    Done,
    Failed(String),
    Cancelled,
}

/// A download in progress, polled by the UI.
pub struct Download {
    pub id: String,
    pub total: u64,
    pub got: Arc<AtomicU64>,
    pub state: Arc<Mutex<State>>,
    cancel: Arc<AtomicBool>,
}

impl Download {
    pub fn start(m: &'static Model, cpu: bool, on_change: impl Fn() + Send + 'static) -> Download {
        let d = Download {
            id: m.id.clone(),
            total: m.download_size(cpu),
            got: Arc::new(AtomicU64::new(0)),
            state: Arc::new(Mutex::new(State::Running)),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let (got, state, cancel) = (d.got.clone(), d.state.clone(), d.cancel.clone());
        std::thread::Builder::new()
            .name("download".into())
            .spawn(move || {
                let r = fetch_all(m, cpu, &got, &cancel, &on_change);
                if let Ok(mut s) = state.lock() {
                    *s = match r {
                        Ok(()) => State::Done,
                        Err(_) if cancel.load(Ordering::Relaxed) => State::Cancelled,
                        Err(e) => State::Failed(e),
                    };
                }
                on_change();
            })
            .ok();
        d
    }

    pub fn state(&self) -> State {
        self.state.lock().map(|s| s.clone()).unwrap_or(State::Running)
    }

    pub fn fraction(&self) -> f32 {
        if self.total == 0 { 0.0 } else { (self.got.load(Ordering::Relaxed) as f64 / self.total as f64) as f32 }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn fetch_all(m: &Model, cpu: bool, got: &AtomicU64, cancel: &AtomicBool, on_change: &dyn Fn()) -> Result<(), String> {
    let dir = m.dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
    let base = std::env::var("MUNINN_MODELS_URL").unwrap_or_else(|_| BASE.to_string());
    let mut done = 0u64;
    for f in m.wanted(cpu) {
        let path = dir.join(&f.name);
        if std::fs::metadata(&path).is_ok_and(|x| x.len() == f.size) {
            done += f.size;
            got.store(done, Ordering::Relaxed);
            continue;
        }
        let part = path.with_extension("part");
        let url = format!("{}/{}-{}", base.trim_end_matches('/'), m.id, f.name);
        let mut last = std::time::Instant::now();
        let n = crate::update::fetch(
            &url,
            &part,
            |g, _| {
                got.store(done + g, Ordering::Relaxed);
                if last.elapsed().as_millis() > 100 {
                    last = std::time::Instant::now();
                    on_change();
                }
            },
            || !cancel.load(Ordering::Relaxed),
        );
        if cancel.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&part);
            return Err("cancelled".into());
        }
        if n != Some(f.size) {
            let _ = std::fs::remove_file(&part);
            return Err("The download stopped partway. Check your connection and try again.".into());
        }
        if sha256(&part).as_deref() != Some(f.sha256.as_str()) {
            let _ = std::fs::remove_file(&part);
            return Err(format!("{} didn't download correctly. Try again.", f.name));
        }
        std::fs::rename(&part, &path).map_err(|e| format!("Couldn't save {}: {e}", f.name))?;
        done += f.size;
    }
    Ok(())
}

fn sha256(p: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(p).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn size_label(bytes: u64) -> String {
    let mb = bytes as f64 / 1e6;
    if mb >= 1000.0 { format!("{:.1} GB", mb / 1000.0) } else { format!("{:.0} MB", mb) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn catalog_parses() {
        let c = super::catalog();
        assert_eq!(c.len(), 3);
        assert!(c.iter().all(|m| m.files.iter().any(|f| f.name == "tokenizer.json")));
        assert_eq!(super::get("best").unwrap().dim, 768);
    }
}
