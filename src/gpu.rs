//! Picks the display adapter DirectML should run on.

use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE};

pub struct Adapter {
    pub index: i32,
    pub name: String,
    pub software: bool,
    pub vram: usize,
}

pub fn adapters() -> Vec<Adapter> {
    let mut out = Vec::new();
    unsafe {
        let Ok(f) = CreateDXGIFactory1::<IDXGIFactory1>() else { return out };
        let mut i = 0;
        while let Ok(a) = f.EnumAdapters1(i) {
            if let Ok(d) = a.GetDesc1() {
                let n = d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len());
                out.push(Adapter {
                    index: i as i32,
                    name: String::from_utf16_lossy(&d.Description[..n]),
                    software: d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0,
                    vram: d.DedicatedVideoMemory,
                });
            }
            i += 1;
        }
    }
    out
}

/// Real GPUs first, the one with the most dedicated memory leading (the discrete GPU on
/// hybrid laptops), then the software adapter as a last resort.
pub fn ranked() -> Vec<Adapter> {
    let mut a = adapters();
    a.sort_by_key(|x| (x.software, std::cmp::Reverse(x.vram)));
    if std::env::var_os("MUNINN_WARP").is_some() {
        a.retain(|x| x.software);
    }
    a
}
