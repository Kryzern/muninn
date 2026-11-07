//! Photo discovery, embedding, thumbnails and search.

use crate::clip::{Clip, MEAN, SIZE, STD};
use anyhow::Result;
use image::{imageops::FilterType, DynamicImage, ImageDecoder, ImageReader, RgbImage};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

const EXTS: [&str; 20] = ["jpg", "jpeg", "jfif", "png", "webp", "gif", "bmp", "heic", "heif", "avif", "tif", "tiff", "dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf"];
const SKIP: [&str; 6] = ["appdata", "node_modules", "$recycle.bin", "windows", "program files", "cache"];
const BATCH: usize = 64;
/// Images whose long side is smaller than this are icons, stickers and UI
/// bits, not photos: they're left out.
pub const MIN_LONG: u32 = 400;
/// Bumped when the model or the stored data changes; a mismatch re-indexes everything.
const FORMAT: &str = "3:ocr";
const STOP: [&str; 12] = ["the", "and", "with", "for", "from", "that", "this", "photo", "picture", "image", "of", "a"];

#[derive(Serialize, Deserialize, Clone)]
pub struct Entry {
    pub path: String,
    pub mtime: u64,
    pub size: u64,
    #[serde(default)]
    pub w: u32,
    #[serde(default)]
    pub h: u32,
    /// Text found in the photo, lowercased.
    #[serde(default)]
    pub text: String,
    /// When it was taken (unix seconds) from its metadata; 0 = not read yet,
    /// -1 = it has none, so the file's date stands in.
    #[serde(default)]
    pub taken: i64,
    /// Only a placeholder on this PC (OneDrive Files On-Demand and the like):
    /// reading it would download it.
    #[serde(skip)]
    pub cloud: bool,
}

#[derive(Default, Clone)]
pub struct Index {
    pub entries: Vec<Entry>,
    pub vecs: Vec<f32>,
    /// Cloud-only photos left alone by the last update.
    pub cloud_skipped: usize,
    /// Files that aren't photos (too small, unreadable), by path, with the
    /// mtime and size they had, so they aren't decoded again on every rescan.
    pub skipped: HashMap<String, (u64, u64)>,
    /// Which model made the vectors, and their length.
    pub model: String,
    pub dim: usize,
}

impl Entry {
    /// When the photo was taken, or failing that when the file was last changed.
    pub fn when(&self) -> i64 {
        if self.taken > 0 { self.taken } else { self.mtime as i64 }
    }
}

/// Each model keeps its own index, so switching back to one is instant.
pub fn index_dir(model: &str) -> PathBuf {
    let d = data_dir().join("index").join(model);
    if !d.exists() {
        let _ = std::fs::create_dir_all(&d);
        // 0.3 kept the only index at the top level
        if model == "fast" {
            for f in ["index.json", "vectors.bin"] {
                let _ = std::fs::rename(data_dir().join(f), d.join(f));
            }
        }
    }
    d
}

pub fn data_dir() -> PathBuf {
    // MUNINN_HOME overrides the data folder (portable / testing)
    let d = std::env::var_os("MUNINN_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("Muninn"));
    let _ = std::fs::create_dir_all(d.join("thumbs"));
    d
}

pub fn thumb_path(path: &str) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    data_dir().join("thumbs").join(format!("{:016x}.jpg", h.finish()))
}

pub fn default_roots() -> Vec<PathBuf> {
    let mut r: Vec<PathBuf> = [dirs::picture_dir(), dirs::desktop_dir(), dirs::download_dir()].into_iter().flatten().collect();
    if let Some(h) = dirs::home_dir() {
        r.push(h.join("OneDrive").join("Pictures"));
    }
    r.retain(|p| p.exists());
    r.dedup();
    r
}

impl Index {
    pub fn load(model: &str, dim: usize) -> Self {
        let empty = Self { model: model.into(), dim, ..Default::default() };
        let d = index_dir(model);
        let entries: Vec<Entry> = std::fs::read(d.join("index.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let raw = std::fs::read(d.join("vectors.bin")).unwrap_or_default();
        let vecs: Vec<f32> = raw.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        let fmt = std::fs::read_to_string(d.join("format.txt")).unwrap_or_default();
        if dim == 0 || vecs.len() != entries.len() * dim || (!entries.is_empty() && fmt.trim() != FORMAT) {
            return empty;
        }
        let skipped = std::fs::read(d.join("skipped.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { entries, vecs, cloud_skipped: 0, skipped, model: model.into(), dim }
    }

    pub fn save(&self) -> Result<()> {
        let d = index_dir(&self.model);
        std::fs::write(d.join("index.json.tmp"), serde_json::to_vec(&self.entries)?)?;
        let bytes: Vec<u8> = self.vecs.iter().flat_map(|f| f.to_le_bytes()).collect();
        std::fs::write(d.join("vectors.bin.tmp"), bytes)?;
        std::fs::rename(d.join("index.json.tmp"), d.join("index.json"))?;
        std::fs::rename(d.join("vectors.bin.tmp"), d.join("vectors.bin"))?;
        std::fs::write(d.join("format.txt"), FORMAT)?;
        std::fs::write(d.join("skipped.json"), serde_json::to_vec(&self.skipped)?)?;
        Ok(())
    }

    /// Top `k` matches: (entry index, score).
    pub fn search(&self, q: &[f32], words: &[String], k: usize) -> Vec<(usize, f32)> {
        self.search_where(q, words, k, |_| true)
    }

    /// Like `search`, only over the entries `keep` accepts.
    pub fn search_where(&self, q: &[f32], words: &[String], k: usize, keep: impl Fn(&Entry) -> bool + Sync) -> Vec<(usize, f32)> {
        if self.dim == 0 || q.len() != self.dim {
            return vec![];
        }
        let words: Vec<&str> = words.iter().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric())).filter(|w| w.len() > 2 && !STOP.contains(w)).collect();
        let phrase = words.join(" ");
        let mut s: Vec<(usize, f32)> = self
            .vecs
            .par_chunks_exact(self.dim)
            .enumerate()
            .filter(|(i, _)| keep(&self.entries[*i]))
            .map(|(i, v)| {
                let mut d: f32 = v.iter().zip(q).map(|(a, b)| a * b).sum();
                let e = &self.entries[i];
                let name = e.path.to_lowercase();
                if words.iter().any(|w| name.contains(w)) {
                    d += 0.04; // filename hit
                }
                if !words.is_empty() && !e.text.is_empty() {
                    // text inside the photo
                    let hits = words.iter().filter(|w| e.text.contains(**w)).count();
                    d += 0.10 * hits as f32 / words.len() as f32;
                    if words.len() > 1 && e.text.contains(phrase.as_str()) {
                        d += 0.04;
                    }
                }
                (i, d)
            })
            .collect();
        let k = k.min(s.len());
        if k == 0 {
            return vec![];
        }
        s.select_nth_unstable_by(k - 1, |a, b| b.1.total_cmp(&a.1));
        s.truncate(k);
        s.sort_by(|a, b| b.1.total_cmp(&a.1));
        s
    }
}

pub fn scan(roots: &[PathBuf]) -> Vec<Entry> {
    let mut out = Vec::new();
    for root in roots {
        let walk = walkdir::WalkDir::new(root).into_iter().filter_entry(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            !(e.file_type().is_dir() && e.depth() > 0 && (n.starts_with('.') || SKIP.contains(&n.as_str())))
        });
        for e in walk.flatten() {
            let ok = e.path().extension().map(|x| EXTS.contains(&x.to_string_lossy().to_lowercase().as_str())).unwrap_or(false);
            if !ok || !e.file_type().is_file() {
                continue;
            }
            let Ok(m) = e.metadata() else { continue };
            if m.len() < 8 * 1024 {
                continue; // icons and junk
            }
            let mtime = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            // From the directory listing, so checking never opens the file.
            let cloud = is_cloud_only(std::os::windows::fs::MetadataExt::file_attributes(&m));
            out.push(Entry { path: e.path().to_string_lossy().into_owned(), mtime, size: m.len(), w: 0, h: 0, text: String::new(), taken: 0, cloud });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// A placeholder whose contents live in the cloud. Opening it makes OneDrive
/// (or any cloud files provider) download it, so it is skipped, unless it's
/// set to "Always keep on this device".
fn is_cloud_only(attrs: u32) -> bool {
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x4_0000;
    const PINNED: u32 = 0x8_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    attrs & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0 && attrs & PINNED == 0
}

/// Re-embed only new or changed files; `progress(done, total, so_far)` after
/// each batch, returning false to stop early (what's done so far is kept).
pub fn update(clip: &Clip, old: &Index, files: Vec<Entry>, read_text: bool, mut progress: impl FnMut(usize, usize, &Index) -> bool) -> Result<Index> {
    let known: HashMap<&str, usize> = old.entries.iter().enumerate().map(|(i, e)| (e.path.as_str(), i)).collect();
    let dim = clip.dim;
    let mut idx = Index { model: old.model.clone(), dim, ..Default::default() };
    let mut todo = Vec::new();
    for f in files {
        if old.skipped.get(&f.path) == Some(&(f.mtime, f.size)) {
            idx.skipped.insert(f.path.clone(), (f.mtime, f.size));
            continue;
        }
        match known.get(f.path.as_str()) {
            // indexed before small images were left out
            Some(&i) if old.entries[i].w > 0 && old.entries[i].w.max(old.entries[i].h) < MIN_LONG => {
                idx.skipped.insert(f.path.clone(), (f.mtime, f.size));
            }
            Some(&i) if old.entries[i].mtime == f.mtime && old.entries[i].size == f.size && old.entries[i].w > 0 => {
                idx.vecs.extend_from_slice(&old.vecs[i * dim..(i + 1) * dim]);
                idx.entries.push(old.entries[i].clone());
            }
            // Indexed before it went cloud-only stays searchable (above); new or
            // changed ones wait until they are on this PC.
            _ if f.cloud => idx.cloud_skipped += 1,
            _ => todo.push(f),
        }
    }
    // Photos indexed before dates were read get theirs now: metadata only, no decoding.
    let missing: Vec<usize> = idx.entries.iter().enumerate().filter(|(_, e)| e.taken == 0).map(|(i, _)| i).collect();
    if !missing.is_empty() {
        let dates: Vec<i64> = missing.par_iter().map(|&i| crate::decode::taken(&idx.entries[i].path).unwrap_or(-1)).collect();
        for (i, t) in missing.into_iter().zip(dates) {
            idx.entries[i].taken = t;
        }
    }
    let total = todo.len();
    let _ = progress(0, total, &idx);
    // Decode + thumbnails for the next batches run while the GPU embeds this one. Photos that look
    // like they hold text go to a separate OCR worker, so reading text never stalls indexing.
    let ocr_on = read_text && std::env::var_os("MUNINN_NO_OCR").is_none();
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<(Entry, Vec<f32>)>>(2);
    let (otx, orx) = std::sync::mpsc::channel::<(usize, String)>();
    let (rtx, rrx) = std::sync::mpsc::channel::<(usize, String)>();
    let mut done = 0;
    std::thread::scope(|sc| -> Result<()> {
        let todo = &todo;
        sc.spawn(move || {
            for chunk in todo.chunks(BATCH) {
                let p: Vec<(Entry, Vec<f32>)> = chunk.par_iter().map(|e| match prep(&e.path) {
                    Some((px, w, h)) => (Entry { w, h, text: String::new(), taken: crate::decode::taken(&e.path).unwrap_or(-1), ..e.clone() }, px),
                    None => (Entry { w: 0, ..e.clone() }, vec![]),
                }).collect();
                if tx.send(p).is_err() {
                    break;
                }
            }
        });
        sc.spawn(move || {
            // gather jobs on this plain thread (never on pool threads), then read them on every core
            // once embedding is done and the CPU is free
            let jobs: Vec<(usize, String)> = orx.into_iter().collect();
            jobs.par_iter().for_each_with(rtx, |r, (i, path)| {
                if let Some(d) = crate::decode::decode(path, 1600) {
                    let _ = r.send((*i, crate::ocr::read(&DynamicImage::ImageRgb8(d.img))));
                }
            });
        });
        let apply = |idx: &mut Index, (i, t): (usize, String)| { if let Some(e) = idx.entries.get_mut(i) { e.text = t; } };
        for (n, batch) in rx.into_iter().enumerate() {
            done += batch.len();
            let (ok, bad): (Vec<_>, Vec<_>) = batch.into_iter().partition(|(_, p)| !p.is_empty());
            for (e, _) in bad {
                idx.skipped.insert(e.path, (e.mtime, e.size));
            }
            if !ok.is_empty() {
                let count = ok.len();
                let mut px = Vec::with_capacity(count * 3 * SIZE * SIZE);
                for (_, p) in &ok {
                    px.extend_from_slice(p);
                }
                let v = clip.embed_images(px, count)?;
                for (j, (e, _)) in ok.iter().enumerate() {
                    if ocr_on && clip.textness(&v[j * dim..(j + 1) * dim]) > -0.02 {
                        let _ = otx.send((idx.entries.len() + j, e.path.clone()));
                    }
                }
                idx.vecs.extend_from_slice(&v);
                idx.entries.extend(ok.into_iter().map(|(e, _)| e));
            }
            while let Ok(r) = rrx.try_recv() { apply(&mut idx, r); }
            if n % 20 == 19 {
                idx.save()?;
            }
            if !progress(done, total, &idx) {
                break;
            }
        }
        drop(otx);
        Ok(())
    })?;
    while let Ok(r) = rrx.try_recv() { let (i, t) = r; if let Some(e) = idx.entries.get_mut(i) { e.text = t; } }
    idx.save()?;
    Ok(idx)
}

fn open(path: &str) -> Option<DynamicImage> {
    let mut dec = ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_decoder().ok()?;
    let o = dec.orientation().ok();
    let mut img = DynamicImage::from_decoder(dec).ok()?;
    if let Some(o) = o {
        img.apply_orientation(o);
    }
    Some(img)
}

/// Decode small, write a UI thumbnail, return model input.
fn prep(path: &str) -> Option<(Vec<f32>, u32, u32)> {
    let long = 640;
    let (img, w, h) = match crate::decode::decode(path, long) {
        Some(d) => (d.img, d.w, d.h),
        None => {
            let i = open(path)?;
            let (w, h) = (i.width(), i.height());
            if w < 48 || h < 48 {
                return None;
            }
            (i.thumbnail(long, long).to_rgb8(), w, h)
        }
    };
    if w.max(h) < MIN_LONG {
        return None;
    }
    let (iw, ih) = img.dimensions();
    let s = (360.0 / iw.min(ih) as f32).min(1.0);
    let small: RgbImage = image::imageops::thumbnail(&img, ((iw as f32 * s) as u32).max(1), ((ih as f32 * s) as u32).max(1));
    let tp = thumb_path(path);
    if !tp.exists() {
        let _ = small.save_with_format(&tp, image::ImageFormat::Jpeg);
    }
    let sq = DynamicImage::ImageRgb8(small).resize_to_fill(SIZE as u32, SIZE as u32, FilterType::Triangle).to_rgb8();
    let mut out = vec![0f32; 3 * SIZE * SIZE];
    for (i, p) in sq.pixels().enumerate() {
        for c in 0..3 {
            out[c * SIZE * SIZE + i] = (p[c] as f32 / 255.0 - MEAN[c]) / STD[c];
        }
    }
    Some((out, w, h))
}

#[cfg(test)]
mod tests {
    use super::is_cloud_only;

    #[test]
    fn cloud_placeholders() {
        assert!(!is_cloud_only(0x20)); // archive: a normal local file
        assert!(is_cloud_only(0x40_0020)); // OneDrive "online-only"
        assert!(is_cloud_only(0x1000));
        assert!(!is_cloud_only(0x48_0020)); // "Always keep on this device"
        assert!(!is_cloud_only(0x10_0020)); // unpinned but downloaded: local
    }
}

#[cfg(test)]
mod live {
    #[test]
    #[ignore]
    fn scan_real_folders() {
        let roots = super::default_roots();
        let t = std::time::Instant::now();
        let files = super::scan(&roots);
        let cloud = files.iter().filter(|f| f.cloud).count();
        eprintln!("roots {roots:?}\n{} photos, {cloud} cloud-only, scan {:?}", files.len(), t.elapsed());
    }
}
