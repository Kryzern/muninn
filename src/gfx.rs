//! Rendering, the way Heimdall does it: Direct3D 11 for the photo field and
//! the glass, Direct2D/DirectWrite for text on top, one flip-model swap chain
//! paced by its frame-latency waitable so the UI thread never blocks on vsync.

use windows::core::{s, Interface, Result, HSTRING, PCSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::Fxc::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows_numerics::Matrix3x2;

const SHADER: &str = include_str!("render.hlsl");
const FONTS: [&[u8]; 3] = [include_bytes!("../ui/fonts/satoshi-400.otf"), include_bytes!("../ui/fonts/satoshi-500.otf"), include_bytes!("../ui/fonts/satoshi-700.otf")];
pub const BG: Color = Color::rgba(11.0 / 255.0, 11.0 / 255.0, 12.0 / 255.0, 1.0);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Color {
        Color { r, g, b, a }
    }
    pub const fn gray(v: f32, a: f32) -> Color {
        Color { r: v, g: v, b: v, a }
    }
    pub fn alpha(self, a: f32) -> Color {
        Color { a: self.a * a, ..self }
    }
    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.r, g: self.g, b: self.b, a: self.a }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    fn d2d(self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
    pub fn inset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w - 2.0 * dx, self.h - 2.0 * dy)
    }
}

/// Must match `Photo` in render.hlsl.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PhotoParams {
    rect: [f32; 4],
    size: [f32; 2],
    bias: f32,
    gray: f32,
    dim: f32,
    rad: f32,
    soft: f32,
    _p0: f32,
    texel: [f32; 2],
    _p1: [f32; 2],
    bg: [f32; 4],
}

/// Must match `Glass` in render.hlsl.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct GlassParams {
    size: [f32; 2],
    scale: f32,
    scene_dim: f32,
    light: [f32; 2],
    lens: f32,
    blur_lod: f32,
    shape: [[f32; 4]; 4],
    radius: [f32; 4],
    alpha: [f32; 4],
    tint: [f32; 4],
    lens_k: [f32; 4],
}

/// One photo in the field, in DIPs.
pub struct Quad<'a> {
    pub tex: &'a Texture,
    pub rect: Rect,
    pub blur: f32,
    pub gray: f32,
    pub dim: f32,
}

/// A piece of glass, in DIPs.
#[derive(Clone, Copy)]
pub struct Glass {
    pub rect: Rect,
    pub radius: f32,
    pub alpha: f32,
    /// How far the rim bends what's behind it, in DIPs.
    pub lens: f32,
}

pub struct Texture {
    srv: ID3D11ShaderResourceView,
    pub w: u32,
    pub h: u32,
}

pub struct Fonts {
    pub query: IDWriteTextFormat,
    pub query_small: IDWriteTextFormat,
    pub word: IDWriteTextFormat,
    pub body: IDWriteTextFormat,
    pub body_medium: IDWriteTextFormat,
    pub small: IDWriteTextFormat,
    pub title: IDWriteTextFormat,
    pub heading: IDWriteTextFormat,
    pub eyebrow: IDWriteTextFormat,
    pub icon: IDWriteTextFormat,
}

pub struct Gfx {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    waitable: HANDLE,
    pub dc: ID2D1DeviceContext,
    pub dwrite: IDWriteFactory,
    pub fonts: Fonts,
    brush: ID2D1SolidColorBrush,
    target: Option<(ID2D1Bitmap1, ID3D11RenderTargetView, ID3D11Texture2D)>,
    scene: Option<(ID3D11RenderTargetView, ID3D11ShaderResourceView, ID3D11Texture2D)>,
    /// Text layouts by (format, text, size, wrapped): building one is most of
    /// what drawing a label costs, and the same labels repeat every frame.
    layouts: std::cell::RefCell<std::collections::HashMap<(usize, String, i32, i32, bool), IDWriteTextLayout>>,
    scissor: ID3D11RasterizerState,
    /// The photo layer still holds the last frame's photos.
    scene_valid: bool,
    /// When the photo layer's mip chain (what the glass frosts from) was last
    /// rebuilt: at most 60 times a second, however fast frames come.
    mips_at: Option<std::time::Instant>,
    mips_stale: bool,
    raven: Option<(ID2D1PathGeometry, ID2D1PathGeometry)>,
    vs_quad: ID3D11VertexShader,
    ps_photo: ID3D11PixelShader,
    vs_full: ID3D11VertexShader,
    ps_glass: ID3D11PixelShader,
    cb_photo: ID3D11Buffer,
    cb_glass: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    pub size: (u32, u32),
    pub scale: f32,
}

impl Drop for Gfx {
    fn drop(&mut self) {
        if !self.waitable.is_invalid() {
            unsafe { CloseHandle(self.waitable).ok() };
        }
    }
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut code = None;
    let mut errors = None;
    let r = unsafe {
        D3DCompile(SHADER.as_ptr() as *const _, SHADER.len(), s!("render.hlsl"), None, None, entry, target, D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &mut code, Some(&mut errors))
    };
    if let Err(e) = r {
        if let Some(err) = errors {
            let msg = unsafe { std::slice::from_raw_parts(err.GetBufferPointer() as *const u8, err.GetBufferSize()) };
            crate::log(format!("shader: {}", String::from_utf8_lossy(msg)));
        }
        return Err(e);
    }
    Ok(code.unwrap())
}

fn bytes(b: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) }
}

/// Satoshi from inside the exe, or Segoe UI if that fails for any reason.
fn satoshi(f: &IDWriteFactory) -> Option<IDWriteFontCollection> {
    unsafe {
        let f5: IDWriteFactory5 = f.cast().ok()?;
        let loader = f5.CreateInMemoryFontFileLoader().ok()?;
        f5.RegisterFontFileLoader(&loader).ok()?;
        let builder = f5.CreateFontSetBuilder().ok()?;
        for data in FONTS {
            let file = loader.CreateInMemoryFontFileReference(&f5, data.as_ptr() as *const _, data.len() as u32, None).ok()?;
            builder.AddFontFile(&file).ok()?;
        }
        let set = builder.CreateFontSet().ok()?;
        f5.CreateFontCollectionFromFontSet(&set).ok()?.cast().ok()
    }
}

fn format(dw: &IDWriteFactory, coll: &Option<IDWriteFontCollection>, family: &str, weight: DWRITE_FONT_WEIGHT, size: f32) -> Result<IDWriteTextFormat> {
    let (family, coll) = match coll {
        Some(c) if family == "Satoshi" => ("Satoshi", Some(c)),
        _ => (if family == "Satoshi" { "Segoe UI Variable Text" } else { family }, None),
    };
    unsafe {
        let fmt = dw.CreateTextFormat(&HSTRING::from(family), coll, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, &HSTRING::from("en-us"))?;
        fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let sign = dw.CreateEllipsisTrimmingSign(&fmt)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        fmt.SetTrimming(&trim, &sign)?;
        Ok(fmt)
    }
}

impl Gfx {
    pub fn new(hwnd: HWND, w: u32, h: u32, scale: f32) -> Result<Gfx> {
        unsafe {
            let mut device = None;
            let mut ctx = None;
            let levels = [D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0];
            let mut r = Err(windows::core::Error::empty());
            for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
                r = D3D11CreateDevice(None, driver, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, Some(&levels), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx));
                if r.is_ok() {
                    break;
                }
            }
            r?;
            let device: ID3D11Device = device.unwrap();
            let ctx: ID3D11DeviceContext = ctx.unwrap();
            let dxgi: IDXGIDevice = device.cast()?;
            let factory: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w.max(1),
                Height: h.max(1),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_NONE,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
                ..Default::default()
            };
            let swap = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)?;
            factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER).ok();
            let swap2: IDXGISwapChain2 = swap.cast()?;
            swap2.SetMaximumFrameLatency(1)?;
            let waitable = swap2.GetFrameLatencyWaitableObject();

            let d2d: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dc = d2d.CreateDevice(&dxgi)?.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = dc.CreateSolidColorBrush(&Color::gray(1.0, 1.0).d2d(), None)?;

            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let coll = satoshi(&dwrite);
            if coll.is_none() {
                crate::log("Satoshi failed to load, using Segoe UI");
            }
            let f = |w, s| format(&dwrite, &coll, "Satoshi", w, s);
            let fonts = Fonts {
                query: f(DWRITE_FONT_WEIGHT_MEDIUM, 26.0)?,
                query_small: f(DWRITE_FONT_WEIGHT_MEDIUM, 20.0)?,
                word: f(DWRITE_FONT_WEIGHT_BOLD, 15.0)?,
                body: f(DWRITE_FONT_WEIGHT_NORMAL, 14.0)?,
                body_medium: f(DWRITE_FONT_WEIGHT_MEDIUM, 14.0)?,
                small: f(DWRITE_FONT_WEIGHT_NORMAL, 12.5)?,
                title: f(DWRITE_FONT_WEIGHT_BOLD, 24.0)?,
                heading: f(DWRITE_FONT_WEIGHT_MEDIUM, 16.0)?,
                eyebrow: f(DWRITE_FONT_WEIGHT_MEDIUM, 11.0)?,
                icon: format(&dwrite, &None, "Segoe Fluent Icons", DWRITE_FONT_WEIGHT_NORMAL, 16.0)
                    .or_else(|_| format(&dwrite, &None, "Segoe MDL2 Assets", DWRITE_FONT_WEIGHT_NORMAL, 16.0))?,
            };
            fonts.icon.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;

            let mut vs_quad = None;
            let mut ps_photo = None;
            let mut vs_full = None;
            let mut ps_glass = None;
            device.CreateVertexShader(bytes(&compile(s!("vs_quad"), s!("vs_4_0"))?), None, Some(&mut vs_quad))?;
            device.CreatePixelShader(bytes(&compile(s!("ps_photo"), s!("ps_4_0"))?), None, Some(&mut ps_photo))?;
            device.CreateVertexShader(bytes(&compile(s!("vs_full"), s!("vs_4_0"))?), None, Some(&mut vs_full))?;
            device.CreatePixelShader(bytes(&compile(s!("ps_glass"), s!("ps_4_0"))?), None, Some(&mut ps_glass))?;
            let buffer = |n: usize| -> Result<ID3D11Buffer> {
                let d = D3D11_BUFFER_DESC {
                    ByteWidth: n.next_multiple_of(16) as u32,
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    ..Default::default()
                };
                let mut b = None;
                device.CreateBuffer(&d, None, Some(&mut b))?;
                Ok(b.unwrap())
            };
            let cb_photo = buffer(std::mem::size_of::<PhotoParams>())?;
            let cb_glass = buffer(std::mem::size_of::<GlassParams>())?;
            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut sampler = None;
            device.CreateSamplerState(&sd, Some(&mut sampler))?;
            let mut bd = D3D11_BLEND_DESC::default();
            bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let mut blend = None;
            device.CreateBlendState(&bd, Some(&mut blend))?;
            let rd = D3D11_RASTERIZER_DESC { FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, ScissorEnable: true.into(), DepthClipEnable: true.into(), ..Default::default() };
            let mut scissor = None;
            device.CreateRasterizerState(&rd, Some(&mut scissor))?;
            let scissor = scissor.unwrap();

            Ok(Gfx {
                device,
                ctx,
                swap,
                waitable,
                dc,
                dwrite,
                fonts,
                brush,
                target: None,
                scene: None,
                layouts: Default::default(),
                scissor,
                scene_valid: false,
                mips_at: None,
                mips_stale: false,
                raven: raven(&d2d),
                vs_quad: vs_quad.unwrap(),
                ps_photo: ps_photo.unwrap(),
                vs_full: vs_full.unwrap(),
                ps_glass: ps_glass.unwrap(),
                cb_photo,
                cb_glass,
                sampler: sampler.unwrap(),
                blend: blend.unwrap(),
                size: (w.max(1), h.max(1)),
                scale,
            })
        }
    }

    /// The glass is frosting from a frame-old copy of the photos: draw once more.
    pub fn stale(&self) -> bool {
        self.mips_stale
    }

    /// Signalled when the compositor is ready for the next frame.
    pub fn waitable(&self) -> HANDLE {
        self.waitable
    }

    pub fn resize(&mut self, w: u32, h: u32, scale: f32) -> Result<()> {
        let (w, h) = (w.max(1), h.max(1));
        self.scale = scale;
        if (w, h) == self.size {
            return Ok(());
        }
        self.target = None;
        self.scene = None;
        self.scene_valid = false;
        unsafe {
            self.dc.SetTarget(None);
            self.ctx.OMSetRenderTargets(None, None);
            self.ctx.ClearState();
            self.ctx.Flush();
            self.swap.ResizeBuffers(2, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT)?;
        }
        self.size = (w, h);
        Ok(())
    }

    /// Upload a top-down BGRA image with a full mip chain (the depth blur samples it).
    pub fn texture(&self, bgra: &[u8], w: u32, h: u32) -> Option<Texture> {
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 0,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32,
            };
            let mut tex = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut tex)).ok()?;
            let tex = tex?;
            self.ctx.UpdateSubresource(&tex, 0, None, bgra.as_ptr() as *const _, w * 4, 0);
            let mut srv = None;
            self.device.CreateShaderResourceView(&tex, None, Some(&mut srv)).ok()?;
            let srv = srv?;
            self.ctx.GenerateMips(&srv);
            Some(Texture { srv, w, h })
        }
    }

    pub fn bitmap(&self, bgra: &[u8], w: u32, h: u32) -> Option<ID2D1Bitmap1> {
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        unsafe { self.dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(bgra.as_ptr() as *const _), w * 4, &props).ok() }
    }

    fn ensure_targets(&mut self) -> Result<()> {
        unsafe {
            if self.target.is_none() {
                let tex: ID3D11Texture2D = self.swap.GetBuffer(0)?;
                let mut rtv = None;
                self.device.CreateRenderTargetView(&tex, None, Some(&mut rtv))?;
                let surface: IDXGISurface = tex.cast()?;
                let props = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                    colorContext: std::mem::ManuallyDrop::new(None),
                };
                let bmp = self.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
                self.target = Some((bmp, rtv.unwrap(), tex));
            }
            if self.scene.is_none() {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: self.size.0,
                    Height: self.size.1,
                    MipLevels: 0,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32,
                };
                let mut tex = None;
                self.device.CreateTexture2D(&desc, None, Some(&mut tex))?;
                let tex = tex.unwrap();
                let rd = D3D11_RENDER_TARGET_VIEW_DESC {
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 } },
                };
                let mut rtv = None;
                self.device.CreateRenderTargetView(&tex, Some(&rd), Some(&mut rtv))?;
                let mut srv = None;
                self.device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
                self.scene = Some((rtv.unwrap(), srv.unwrap(), tex));
            }
        }
        Ok(())
    }

    fn upload<T: Copy>(&self, cb: &ID3D11Buffer, v: &T) -> Result<()> {
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m))?;
            std::ptr::copy_nonoverlapping(v, m.pData as *mut T, 1);
            self.ctx.Unmap(cb, 0);
        }
        Ok(())
    }

    /// One frame: the photo field, the glass over it, then `content` with Direct2D (DIPs).
    /// `photos` false reuses the photo layer from the last frame: nothing in it
    /// moved, so only the glass and the text are drawn again.
    pub fn frame(&mut self, quads: &[Quad], glass: &[Glass], light: (f32, f32), scene_dim: f32, photos: bool, content: impl FnOnce(&Gfx)) -> Result<()> {
        self.ensure_targets()?;
        let (bmp, rtv, back) = self.target.clone().unwrap();
        let (scene_rtv, scene_srv, scene_tex) = self.scene.clone().unwrap();
        let s = self.scale;
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        unsafe {
            let vp = D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: w, Height: h, MinDepth: 0.0, MaxDepth: 1.0 };
            self.ctx.RSSetViewports(Some(&[vp]));
            self.ctx.IASetInputLayout(None);
            self.ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));

            // 1. photos
            if photos || !self.scene_valid {
            self.ctx.OMSetRenderTargets(Some(&[Some(scene_rtv.clone())]), None);
            self.ctx.ClearRenderTargetView(&scene_rtv, &[BG.r, BG.g, BG.b, 1.0]);
            self.ctx.OMSetBlendState(&self.blend, None, 0xffff_ffff);
            self.ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            self.ctx.VSSetShader(&self.vs_quad, None);
            self.ctx.PSSetShader(&self.ps_photo, None);
            self.ctx.VSSetConstantBuffers(0, Some(&[Some(self.cb_photo.clone())]));
            self.ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb_photo.clone())]));
            for q in quads {
                if q.rect.w < 1.0 || q.dim < 0.004 || q.rect.x > w / s || q.rect.y > h / s || q.rect.x + q.rect.w < 0.0 || q.rect.y + q.rect.h < 0.0 {
                    continue;
                }
                let p = PhotoParams {
                    rect: [q.rect.x * s, q.rect.y * s, q.rect.w * s, q.rect.h * s],
                    size: [w, h],
                    bias: q.blur,
                    gray: q.gray,
                    dim: q.dim,
                    rad: (12.0 * s * (q.rect.w / 180.0).clamp(0.55, 1.2)).min(q.rect.w * s * 0.2),
                    soft: 0.75 + q.blur.exp2() * 0.5 * s,
                    texel: [1.0 / q.tex.w as f32, 1.0 / q.tex.h as f32],
                    bg: [BG.r, BG.g, BG.b, 1.0],
                    ..Default::default()
                };
                self.upload(&self.cb_photo, &p)?;
                self.ctx.PSSetShaderResources(0, Some(&[Some(q.tex.srv.clone())]));
                self.ctx.Draw(4, 0);
            }
            self.ctx.OMSetRenderTargets(None, None);
            self.ctx.PSSetShaderResources(0, Some(&[None]));
            self.scene_valid = true;
            self.mips_stale = true;
            }
            // The frost is a heavy blur of these mips; a frame-old copy of it
            // is invisible, so it's rebuilt at 60 Hz at most.
            if self.mips_stale && self.mips_at.is_none_or(|t| t.elapsed().as_secs_f32() > 1.0 / 61.0 || !photos) {
                self.ctx.GenerateMips(&scene_srv);
                self.mips_at = Some(std::time::Instant::now());
                self.mips_stale = false;
            }

            // 2. composite the photos to the screen, with glass where there is some
            let mut g = GlassParams {
                size: [w, h],
                scale: s,
                scene_dim,
                light: [light.0, light.1],
                lens: 26.0,
                blur_lod: 4.2 + s.log2(),
                tint: [0.10, 0.10, 0.11, 0.30],
                ..Default::default()
            };
            for (i, gl) in glass.iter().take(4).enumerate() {
                g.shape[i] = [gl.rect.x * s, gl.rect.y * s, gl.rect.w * s, gl.rect.h * s];
                g.radius[i] = gl.radius * s;
                g.alpha[i] = gl.alpha;
                g.lens_k[i] = gl.lens;
            }
            self.upload(&self.cb_glass, &g)?;
            self.ctx.OMSetBlendState(None, None, 0xffff_ffff);
            self.ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            self.ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.ctx.VSSetShader(&self.vs_full, None);
            self.ctx.PSSetShader(&self.ps_glass, None);
            self.ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb_glass.clone())]));
            self.ctx.PSSetShaderResources(0, Some(&[Some(scene_srv.clone())]));
            self.ctx.RSSetState(&self.scissor);
            let full = RECT { left: 0, top: 0, right: self.size.0 as i32, bottom: self.size.1 as i32 };
            if scene_dim >= 0.999 {
                // The photos go to the screen as a plain copy; the glass shader
                // only runs over the glass itself (plus room for its shadow).
                self.ctx.CopySubresourceRegion(&back, 0, 0, 0, 0, &scene_tex, 0, None);
                for gl in glass.iter().take(4).filter(|g| g.alpha > 0.001) {
                    let m = 8.0 * s;
                    let r = RECT {
                        left: ((gl.rect.x * s - m).floor() as i32).max(0),
                        top: ((gl.rect.y * s - m).floor() as i32).max(0),
                        right: (((gl.rect.x + gl.rect.w) * s + m).ceil() as i32).min(full.right),
                        bottom: (((gl.rect.y + gl.rect.h) * s + m).ceil() as i32).min(full.bottom),
                    };
                    if r.right > r.left && r.bottom > r.top {
                        self.ctx.RSSetScissorRects(Some(&[r]));
                        self.ctx.Draw(3, 0);
                    }
                }
            } else {
                // A panel or the viewer dims everything: one full pass.
                self.ctx.RSSetScissorRects(Some(&[full]));
                self.ctx.Draw(3, 0);
            }
            self.ctx.RSSetState(None);
            self.ctx.PSSetShaderResources(0, Some(&[None]));
            self.ctx.OMSetRenderTargets(None, None);

            // 3. text and controls
            self.dc.SetTarget(&bmp);
            self.dc.SetDpi(96.0 * s, 96.0 * s);
            self.dc.BeginDraw();
            self.dc.SetTransform(&Matrix3x2::identity());
            content(self);
            let r = self.dc.EndDraw(None, None);
            self.dc.SetTarget(None);
            r?;
            // Never blocks; the message loop paces frames to the compositor
            // (see main.rs), so the driver has nothing to spin on.
            self.swap.Present(0, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    // ---- Direct2D helpers (DIPs) ----------------------------------------------

    fn color(&self, c: Color) {
        unsafe { self.brush.SetColor(&c.d2d()) };
    }

    pub fn fill(&self, r: Rect, c: Color) {
        self.color(c);
        unsafe { self.dc.FillRectangle(&r.d2d(), &self.brush) };
    }

    pub fn fill_round(&self, r: Rect, radius: f32, c: Color) {
        self.color(c);
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.dc.FillRoundedRectangle(&rr, &self.brush) };
    }

    pub fn stroke_round(&self, r: Rect, radius: f32, c: Color) {
        self.color(c);
        let w = 1.0 / self.scale;
        let rr = D2D1_ROUNDED_RECT { rect: r.inset(w / 2.0, w / 2.0).d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.dc.DrawRoundedRectangle(&rr, &self.brush, w, None) };
    }

    pub fn circle(&self, cx: f32, cy: f32, r: f32, c: Color, stroke: Option<f32>) {
        self.color(c);
        let e = D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
        unsafe {
            match stroke {
                Some(w) => self.dc.DrawEllipse(&e, &self.brush, w, None),
                None => self.dc.FillEllipse(&e, &self.brush),
            }
        }
    }

    pub fn text(&self, s: &str, fmt: &IDWriteTextFormat, r: Rect, c: Color) {
        if s.is_empty() || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let Some(l) = self.cached(s, fmt, r.w, r.h, false) else { return };
        self.color(c);
        unsafe { self.dc.DrawTextLayout(windows_numerics::Vector2 { X: r.x, Y: r.y }, &l, &self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP) };
    }

    fn cached(&self, s: &str, fmt: &IDWriteTextFormat, w: f32, h: f32, wrap: bool) -> Option<IDWriteTextLayout> {
        let key = (fmt.as_raw() as usize, s.to_string(), (w * 4.0) as i32, (h * 4.0) as i32, wrap);
        let mut cache = self.layouts.borrow_mut();
        if let Some(l) = cache.get(&key) {
            return Some(l.clone());
        }
        if cache.len() > 800 {
            cache.clear();
        }
        let l = self.layout(s, fmt, w, h)?;
        if wrap {
            unsafe {
                l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP).ok();
                l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR).ok();
            }
        }
        cache.insert(key, l.clone());
        Some(l)
    }

    /// Text drawn right-aligned inside `r`.
    pub fn text_right(&self, s: &str, fmt: &IDWriteTextFormat, r: Rect, c: Color) {
        let tw = self.measure(s, fmt);
        self.text(s, fmt, Rect::new(r.x + r.w - tw - 1.0, r.y, tw + 2.0, r.h), c);
    }

    pub fn text_center(&self, s: &str, fmt: &IDWriteTextFormat, r: Rect, c: Color) {
        let tw = self.measure(s, fmt);
        self.text(s, fmt, Rect::new(r.x + (r.w - tw) / 2.0 - 1.0, r.y, tw + 2.0, r.h), c);
    }

    /// Wrapped text inside `r`, top-aligned. Returns the height it used.
    pub fn paragraph(&self, s: &str, fmt: &IDWriteTextFormat, r: Rect, c: Color) -> f32 {
        let Some(l) = self.cached(s, fmt, r.w, 4000.0, true) else { return 0.0 };
        unsafe {
            let mut m = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut m).ok();
            self.color(c);
            self.dc.DrawTextLayout(windows_numerics::Vector2 { X: r.x, Y: r.y }, &l, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            m.height
        }
    }

    pub fn layout(&self, s: &str, fmt: &IDWriteTextFormat, w: f32, h: f32) -> Option<IDWriteTextLayout> {
        let t: Vec<u16> = s.encode_utf16().collect();
        unsafe { self.dwrite.CreateTextLayout(&t, fmt, w, h).ok() }
    }

    pub fn measure(&self, s: &str, fmt: &IDWriteTextFormat) -> f32 {
        let Some(l) = self.cached(s, fmt, 10_000.0, 100.0, false) else { return 0.0 };
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { l.GetMetrics(&mut m).ok() };
        m.widthIncludingTrailingWhitespace
    }

    /// x of the caret after `pos16` UTF-16 units of `s`.
    pub fn caret_x(&self, s: &str, fmt: &IDWriteTextFormat, pos16: u32) -> f32 {
        let Some(l) = self.cached(s, fmt, 10_000.0, 100.0, false) else { return 0.0 };
        let (mut x, mut y) = (0.0f32, 0.0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe { l.HitTestTextPosition(pos16, false, &mut x, &mut y, &mut m).ok() };
        x
    }

    /// `sharp` picks the expensive resampling: for a photo at rest, not one
    /// mid-animation where nobody can tell.
    pub fn image(&self, b: &ID2D1Bitmap1, r: Rect, opacity: f32, sharp: bool) {
        let mode = if sharp { D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC } else { D2D1_INTERPOLATION_MODE_LINEAR };
        unsafe { self.dc.DrawBitmap(b, Some(&r.d2d()), opacity, mode, None, None) };
    }

    pub fn push_clip_round(&self, r: Rect, radius: f32) {
        unsafe {
            let Ok(f) = self.dc.GetFactory() else { return };
            let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
            let Ok(g) = f.CreateRoundedRectangleGeometry(&rr) else { return };
            let p = D2D1_LAYER_PARAMETERS1 {
                contentBounds: D2D_RECT_F { left: -1e6, top: -1e6, right: 1e6, bottom: 1e6 },
                geometricMask: std::mem::ManuallyDrop::new(Some(g.into())),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS1_NONE,
            };
            self.dc.PushLayer(&p, None);
        }
    }

    pub fn pop_layer(&self) {
        unsafe { self.dc.PopLayer() };
    }

    /// A plain rectangular clip: no offscreen layer, so it costs next to nothing.
    pub fn push_clip(&self, r: Rect) {
        unsafe { self.dc.PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
    }

    pub fn pop_clip(&self) {
        unsafe { self.dc.PopAxisAlignedClip() };
    }

    /// Muninn's raven (docs/logo.svg), `size` DIPs wide, top-left at (x, y).
    pub fn raven(&self, x: f32, y: f32, size: f32, c: Color) {
        let Some((beak, body)) = &self.raven else { return };
        // the bird spans about x 112-406, y 134-392 of the 512 logo
        let k = size / 300.0;
        let m = Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: x - 112.0 * k, M32: y - 134.0 * k };
        unsafe {
            self.dc.SetTransform(&m);
            self.color(c);
            self.dc.FillGeometry(beak, &self.brush, None);
            self.dc.FillGeometry(body, &self.brush, None);
            let head = D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: 278.0, Y: 192.0 }, radiusX: 58.0, radiusY: 58.0 };
            self.dc.FillEllipse(&head, &self.brush);
            self.color(BG);
            let eye = D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: 278.0, Y: 178.0 }, radiusX: 10.0, radiusY: 10.0 };
            self.dc.FillEllipse(&eye, &self.brush);
            self.dc.SetTransform(&Matrix3x2::identity());
        }
    }
}

/// The two filled paths of the raven, in the logo's coordinates (already
/// shifted by the logo's translate(-12 -6)).
fn raven(f: &ID2D1Factory1) -> Option<(ID2D1PathGeometry, ID2D1PathGeometry)> {
    let p = |x: f32, y: f32| windows_numerics::Vector2 { X: x - 12.0, Y: y - 6.0 };
    unsafe {
        let beak = f.CreatePathGeometry().ok()?;
        let s = beak.Open().ok()?;
        s.BeginFigure(p(326.0, 166.0), D2D1_FIGURE_BEGIN_FILLED);
        s.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: p(394.0, 176.0), point2: p(418.0, 214.0) });
        s.AddLine(p(334.0, 232.0));
        s.EndFigure(D2D1_FIGURE_END_CLOSED);
        s.Close().ok()?;
        let body = f.CreatePathGeometry().ok()?;
        let s = body.Open().ok()?;
        s.BeginFigure(p(238.0, 212.0), D2D1_FIGURE_BEGIN_FILLED);
        s.AddBezier(&D2D1_BEZIER_SEGMENT { point1: p(192.0, 266.0), point2: p(156.0, 330.0), point3: p(124.0, 398.0) });
        s.AddLine(p(222.0, 378.0));
        s.AddLine(p(276.0, 386.0));
        s.AddBezier(&D2D1_BEZIER_SEGMENT { point1: p(338.0, 356.0), point2: p(362.0, 300.0), point3: p(348.0, 222.0) });
        s.EndFigure(D2D1_FIGURE_END_CLOSED);
        s.Close().ok()?;
        Some((beak.into(), body.into()))
    }
}
