//! D3D11 / D2D1 设备与 DXGI 交换链的生命周期。
//!
//! ```text
//!   ID3D11Device  ──►  IDXGIDevice  ──►  IDXGIFactory2
//!         │                                    │
//!         │                          CreateSwapChainForComposition
//!         │                                    ▼
//!         └──► ID2D1Factory1 ──► ID2D1Device ──► ID2D1DeviceContext
//!                                                       │
//!                                              SetTarget(交换链缓冲)
//! ```
//!
//! 交换链用**翻转模型**（`FLIP_SEQUENTIAL`）：画面直接由 GPU 呈现，
//! 不存在"拷贝到重定向表面"的额外一趟。
//!
//! **尺寸变化只重建缓冲，不重建设备**（反闪烁铁律 5）：
//! `D3D11CreateDevice` + `D2D1CreateDevice` 加起来要上百毫秒，
//! 拖边框时若走这条路，画面会明显一顿一顿。
//!
//! 本模块内的 `unsafe` 块只包住 COM 调用；指针都指向栈上变量，
//! 接口生命周期由 `Renderer` 持有，没有跨线程共享。

use std::mem::ManuallyDrop;

use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIAdapter, IDXGIDevice, IDXGIFactory2, IDXGIOutput,
    IDXGISurface, IDXGISwapChain1,
};
use windows::core::{BOOL, Interface};

use crate::error::{AppError, Result, Win32Ext};

use super::composer::Composer;

/// 交换链缓冲数量。两个就够 —— 我们不做三缓冲的延迟换帧优化。
const BUFFER_COUNT: u32 = 2;

/// 要尝试的 D3D 功能级别。
///
/// **故意不包含 `11_1`**：`D3D11CreateDevice` 只要在数组里看到它，
/// 而机器没装 D3D 11.1 运行时就整体失败（返回 E_INVALIDARG），
/// 而不是自动跳过。D2D 互操作 11_0 完全够用，从源头避开这个坑。
const FEATURE_LEVELS: &[D3D_FEATURE_LEVEL] = &[
    D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_10_0,
];

/// 渲染器：持有设备、交换链与当前绑定为绘制目标的位图。
pub struct Renderer {
    context: ID2D1DeviceContext,
    swap_chain: IDXGISwapChain1,
    /// 当前绑定到交换链缓冲的位图。`ResizeBuffers` 之后必须重新取。
    target: Option<ID2D1Bitmap1>,
    width: u32,
    height: u32,
    dpi: f32,
}

impl Renderer {
    /// 建立一整套设备与交换链，并把画面挂到 `hwnd` 上。
    pub fn new(hwnd: HWND, width: u32, height: u32, dpi: f32) -> Result<(Self, Composer)> {
        let (d3d, dxgi) = create_d3d_device()?;

        let (context, swap_chain) = {
            // SAFETY: COM 调用；所有出参都是本函数栈上的变量，
            // 传入的接口来自同一条设备链。
            unsafe {
                let factory: ID2D1Factory1 =
                    D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                        .ctx("D2D1CreateFactory")?;
                let d2d_device = factory
                    .CreateDevice(&dxgi)
                    .ctx("ID2D1Factory1::CreateDevice")?;
                let context = d2d_device
                    .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
                    .ctx("ID2D1Device::CreateDeviceContext")?;

                let swap_chain = create_swap_chain(&d3d, &dxgi, width, height)?;
                (context, swap_chain)
            }
        };

        let composer = Composer::attach(&dxgi, hwnd, &swap_chain)?;

        let mut renderer = Self {
            context,
            swap_chain,
            target: None,
            width: width.max(1),
            height: height.max(1),
            dpi: 0.0,
        };
        renderer.set_dpi(dpi);
        renderer.bind_target()?;

        Ok((renderer, composer))
    }

    /// 缓冲尺寸 / DPI 变化。只重建缓冲，设备与交换链本身复用。
    pub fn resize(&mut self, width: u32, height: u32, dpi: f32) -> Result<()> {
        let (width, height) = (width.max(1), height.max(1));
        if width == self.width && height == self.height && (dpi - self.dpi).abs() < 0.01 {
            return Ok(());
        }

        // SAFETY: COM 调用。重建缓冲前必须解绑绘制目标，
        // 否则缓冲区仍被引用，ResizeBuffers 会失败。
        unsafe {
            self.context.SetTarget(None::<&ID2D1Image>);
            self.target = None;

            self.swap_chain
                .ResizeBuffers(
                    BUFFER_COUNT,
                    width,
                    height,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    Default::default(),
                )
                .ctx("IDXGISwapChain::ResizeBuffers")?;
        }

        self.width = width;
        self.height = height;
        self.set_dpi(dpi);
        self.bind_target()?;
        Ok(())
    }

    /// 开始一帧：绑定目标、清成全透明。
    ///
    /// 清成全透明而不是某个背景色，是因为窗口用 `WS_EX_NOREDIRECTIONBITMAP`，
    /// 透明区域会露出后面的亚克力模糊层。
    pub fn begin(&mut self) -> Result<()> {
        if self.target.is_none() {
            self.bind_target()?;
        }

        // 清成完全透明的黑色。
        // 注意必须用**预乘**语义：alpha=0 时 RGB 也必须是 0，
        // 否则半透明区域会泛白。
        let transparent = D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };

        // SAFETY: COM 调用；transparent 活到调用结束。
        unsafe {
            self.context.BeginDraw();
            self.context
                .SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            self.context
                .SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            self.context.Clear(Some(&transparent));
        }
        Ok(())
    }

    /// 结束一帧并提交。`sync_interval` 为 1 时等垂直同步（不撕裂），
    /// 为 0 时立即提交（拖动尺寸期间用，避免每帧阻塞一个刷新周期）。
    pub fn end(&mut self, sync_interval: u32) -> Result<()> {
        // SAFETY: COM 调用。Present 返回 HRESULT，用 ok() 转成 Result。
        unsafe {
            self.context
                .EndDraw(None, None)
                .ctx("ID2D1RenderTarget::EndDraw")?;

            // 解绑：让 D2D 放开对交换链缓冲的引用，Present 才拿得到缓冲。
            self.context.SetTarget(None::<&ID2D1Image>);

            // **必须同时清空缓存位图**。
            // 只 SetTarget(None) 而不清 target 会造成状态不一致：
            // 下一帧 begin() 看到 target.is_some() 就跳过重新绑定，
            // 于是在"没有绘制目标"的上下文上 BeginDraw，EndDraw 直接返回
            // D2DERR_WRONG_STATE(0x88990001)。
            // 表现是：第一帧正常，之后任何重绘都失败，窗口永远停在首帧。
            self.target = None;

            // 同步间隔：1 = 跟随垂直同步，避免撕裂；0 = 立即提交。
            self.swap_chain
                .Present(sync_interval, DXGI_PRESENT(0))
                .ok()
                .ctx("IDXGISwapChain::Present")?;
        }
        Ok(())
    }

    /// 绘制用的设备上下文。
    pub fn context(&self) -> &ID2D1DeviceContext {
        &self.context
    }

    /// 逻辑像素尺寸（DIP）。D2D 已按 DPI 换算，布局代码直接用这个值。
    pub fn dip_size(&self) -> (f32, f32) {
        let scale = 96.0 / self.dpi.max(1.0);
        (self.width as f32 * scale, self.height as f32 * scale)
    }

    /// 交换链的物理像素尺寸。用于和窗口客户区做一致性核对。
    pub fn pixel_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn set_dpi(&mut self, dpi: f32) {
        self.dpi = dpi.max(96.0);
        // 把 D2D 上下文的 DPI 设成窗口 DPI 之后，所有绘制坐标都是逻辑像素（DIP），
        // 与配置里的尺寸单位一致，布局代码里不需要到处乘缩放系数。
        // SAFETY: COM 调用，参数是值类型。
        unsafe { self.context.SetDpi(self.dpi, self.dpi) };
    }

    /// 取交换链缓冲并绑定为绘制目标。
    fn bind_target(&mut self) -> Result<()> {
        // SAFETY: 交换链刚创建或刚 ResizeBuffers，缓冲必定可用；
        // 位图与 surface 的生命周期由 self 持有。
        unsafe {
            let surface: IDXGISurface = self.swap_chain.GetBuffer(0).ctx("GetBuffer(0)")?;

            // 位图的 DPI 必须与上下文一致，否则 DIP → 像素的映射会错位，
            // 表现为整个画面被放大或缩小。
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: self.dpi,
                dpiY: self.dpi,
                // CANNOT_DRAW：这个位图只作为绘制目标，不能当输入纹理用
                bitmapOptions: D2D1_BITMAP_OPTIONS(
                    D2D1_BITMAP_OPTIONS_TARGET.0 | D2D1_BITMAP_OPTIONS_CANNOT_DRAW.0,
                ),
                colorContext: ManuallyDrop::new(None),
            };

            let bitmap = self
                .context
                .CreateBitmapFromDxgiSurface(&surface, Some(&props))
                .ctx("CreateBitmapFromDxgiSurface")?;

            self.context.SetTarget(&bitmap);
            self.target = Some(bitmap);
        }
        Ok(())
    }
}

/// 创建 D3D11 设备，并取出它的 DXGI 接口。
fn create_d3d_device() -> Result<(ID3D11Device, IDXGIDevice)> {
    let mut device: Option<ID3D11Device> = None;

    // SAFETY: 出参指针指向本函数栈上的变量；feature level 切片长度由 API 读取。
    unsafe {
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            // 不用软件光栅化后端，第三个参数给空模块句柄
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(FEATURE_LEVELS),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }
    .ctx("D3D11CreateDevice")?;

    let device = device.ok_or(AppError::Win32 {
        context: "D3D11CreateDevice 成功但未返回设备",
        code: 0,
    })?;

    // COM QueryInterface，device 有效。`cast` 本身是安全方法，不需要 unsafe 块。
    let dxgi: IDXGIDevice = device.cast().ctx("ID3D11Device -> IDXGIDevice")?;
    Ok((device, dxgi))
}

/// 创建用于合成的翻转模型交换链。
fn create_swap_chain(
    d3d: &ID3D11Device,
    dxgi: &IDXGIDevice,
    width: u32,
    height: u32,
) -> Result<IDXGISwapChain1> {
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: width.max(1),
        Height: height.max(1),
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: BOOL(0),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: BUFFER_COUNT,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        // 预乘 alpha：窗口的透明区域才能正确露出后面的亚克力层
        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
        Flags: 0,
    };

    // SAFETY: COM 调用；desc 活到调用结束；不限制输出目标。
    unsafe {
        let adapter = dxgi.GetAdapter().ctx("IDXGIDevice::GetAdapter")?;
        // GetParent 定义在 IDXGIObject 上，IDXGIAdapter 通过 Deref 继承
        let factory: IDXGIFactory2 = adapter.GetParent().ctx("IDXGIAdapter -> IDXGIFactory2")?;

        factory
            .CreateSwapChainForComposition(d3d, &desc, None::<&IDXGIOutput>)
            .ctx("IDXGIFactory2::CreateSwapChainForComposition")
    }
}
