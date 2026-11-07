//! Text in photos via the OCR engine built into Windows.

use image::{imageops::FilterType, DynamicImage};
use std::cell::RefCell;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

fn max_side() -> u32 { std::env::var("MUNINN_OCR_SIDE").ok().and_then(|s| s.parse().ok()).unwrap_or(1600) }

thread_local! {
    // one engine per indexing thread
    static ENGINE: RefCell<Option<Option<OcrEngine>>> = const { RefCell::new(None) };
}

fn engine<R>(f: impl FnOnce(&OcrEngine) -> R) -> Option<R> {
    ENGINE.with(|e| {
        let mut e = e.borrow_mut();
        if e.is_none() {
            unsafe { let _ = RoInitialize(RO_INIT_MULTITHREADED); }
            // the user's languages first, then English if its OCR pack is installed
            *e = Some(OcrEngine::TryCreateFromUserProfileLanguages().ok().or_else(|| {
                windows::Globalization::Language::CreateLanguage(&windows::core::HSTRING::from("en-US")).ok().and_then(|l| OcrEngine::TryCreateFromLanguage(&l).ok())
            }));
        }
        e.as_ref().unwrap().as_ref().map(f)
    })
}

/// Recognized text, lowercased, or empty when there is none.
pub fn read(img: &DynamicImage) -> String {
    let (w, h) = (img.width(), img.height());
    let s = (max_side() as f32 / w.max(h) as f32).min(1.0);
    let img = if s < 1.0 { img.resize((w as f32 * s) as u32, (h as f32 * s) as u32, FilterType::Triangle) } else { img.clone() };
    let (w, h) = (img.width(), img.height());
    if w < 40 || h < 40 {
        return String::new();
    }
    let mut px = img.to_rgba8().into_raw();
    for p in px.chunks_exact_mut(4) {
        p.swap(0, 2); // RGBA -> BGRA
    }
    engine(|e| -> windows::core::Result<String> {
        let dw = DataWriter::new()?;
        dw.WriteBytes(&px)?;
        let buf = dw.DetachBuffer()?;
        let bmp = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(&buf, BitmapPixelFormat::Bgra8, w as i32, h as i32, BitmapAlphaMode::Ignore)?;
        let r = e.RecognizeAsync(&bmp)?.get()?;
        Ok(r.Text()?.to_string_lossy().to_lowercase())
    })
    .and_then(|r| r.ok())
    .unwrap_or_default()
}
