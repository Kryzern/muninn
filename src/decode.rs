//! Image decoding through Windows Imaging Component: fast scaled JPEG decodes, plus any
//! format Windows has a codec for (HEIC, AVIF, camera RAW, TIFF...).

use image::{imageops, RgbImage};
use std::cell::RefCell;
use windows::core::HSTRING;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

thread_local! {
    static WIC: RefCell<Option<IWICImagingFactory>> = const { RefCell::new(None) };
}

fn factory() -> Option<IWICImagingFactory> {
    WIC.with(|f| {
        let mut f = f.borrow_mut();
        if f.is_none() {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                *f = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok();
            }
        }
        f.clone()
    })
}

pub struct Decoded {
    pub img: RgbImage,
    /// Original size after orientation.
    pub w: u32,
    pub h: u32,
}

/// Decode so the long side is about `long` px (never upscaled), oriented upright.
pub fn decode(path: &str, long: u32) -> Option<Decoded> {
    let f = factory()?;
    unsafe {
        let dec = f.CreateDecoderFromFilename(&HSTRING::from(path), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = dec.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        if w < 48 || h < 48 {
            return None;
        }
        let orient = orientation(&frame);
        let s = (long as f64 / w.max(h) as f64).min(1.0);
        let (tw, th) = (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1));
        let scaler = f.CreateBitmapScaler().ok()?;
        scaler.Initialize(&frame, tw, th, WICBitmapInterpolationModeFant).ok()?;
        let conv = f.CreateFormatConverter().ok()?;
        conv.Initialize(&scaler, &GUID_WICPixelFormat24bppRGB, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let stride = tw * 3;
        let mut buf = vec![0u8; (stride * th) as usize];
        conv.CopyPixels(std::ptr::null(), stride, &mut buf).ok()?;
        let img = RgbImage::from_raw(tw, th, buf)?;
        let img = match orient {
            2 => imageops::flip_horizontal(&img),
            3 => imageops::rotate180(&img),
            4 => imageops::flip_vertical(&img),
            5 => imageops::flip_horizontal(&imageops::rotate90(&img)),
            6 => imageops::rotate90(&img),
            7 => imageops::flip_horizontal(&imageops::rotate270(&img)),
            8 => imageops::rotate270(&img),
            _ => img,
        };
        let (w, h) = if (5..=8).contains(&orient) { (h, w) } else { (w, h) };
        Some(Decoded { img, w, h })
    }
}

fn orientation(frame: &IWICBitmapFrameDecode) -> u16 {
    unsafe {
        let Ok(q) = frame.GetMetadataQueryReader() else { return 1 };
        let mut v = PROPVARIANT::default();
        if q.GetMetadataByName(&HSTRING::from("System.Photo.Orientation"), &mut v).is_err() {
            return 1;
        }
        u16::try_from(&v).unwrap_or(1)
    }
}

/// When the photo was taken, from its metadata (unix seconds). Reads no pixels.
pub fn taken(path: &str) -> Option<i64> {
    let f = factory()?;
    unsafe {
        let dec = f.CreateDecoderFromFilename(&HSTRING::from(path), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = dec.GetFrame(0).ok()?;
        let q = frame.GetMetadataQueryReader().ok()?;
        let mut v = PROPVARIANT::default();
        q.GetMetadataByName(&HSTRING::from("System.Photo.DateTaken"), &mut v).ok()?;
        let ft = windows::Win32::System::Com::StructuredStorage::PropVariantToFileTime(&v, windows::Win32::System::Variant::PSTF_UTC).ok()?;
        let t = ((ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64) as i64;
        // 100 ns ticks since 1601 -> seconds since 1970
        let secs = t / 10_000_000 - 11_644_473_600;
        (secs > 0).then_some(secs)
    }
}

/// Premultiplied BGRA, upright, long side about `long` px (never upscaled).
pub fn bgra(path: &str, long: u32) -> Option<(Vec<u8>, u32, u32)> {
    let f = factory()?;
    unsafe {
        let dec = f.CreateDecoderFromFilename(&HSTRING::from(path), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = dec.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 {
            return None;
        }
        let orient = orientation(&frame);
        let s = (long as f64 / w.max(h) as f64).min(1.0);
        let (tw, th) = (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1));
        let scaler = f.CreateBitmapScaler().ok()?;
        scaler.Initialize(&frame, tw, th, WICBitmapInterpolationModeFant).ok()?;
        let conv = f.CreateFormatConverter().ok()?;
        conv.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let mut buf = vec![0u8; (tw * th * 4) as usize];
        conv.CopyPixels(std::ptr::null(), tw * 4, &mut buf).ok()?;
        // the channel order doesn't matter to a rotation
        let img = image::RgbaImage::from_raw(tw, th, buf)?;
        let img = match orient {
            2 => imageops::flip_horizontal(&img),
            3 => imageops::rotate180(&img),
            4 => imageops::flip_vertical(&img),
            5 => imageops::flip_horizontal(&imageops::rotate90(&img)),
            6 => imageops::rotate90(&img),
            7 => imageops::flip_horizontal(&imageops::rotate270(&img)),
            8 => imageops::rotate270(&img),
            _ => img,
        };
        let (iw, ih) = img.dimensions();
        Some((img.into_raw(), iw, ih))
    }
}
