//! 窗口背景材质：亚克力毛玻璃、系统 Mica、圆角。
//!
//! ## 为什么手写 FFI
//!
//! `SetWindowCompositionAttribute` 是 **user32.dll 里未公开的 API**，
//! `windows` crate 不提供绑定，所以这里自己声明。
//! 之所以值得用：它的 `ACCENT_ENABLE_ACRYLICBLURBEHIND` 接受一个
//! `GradientColor` 参数，能指定**任意色调**；而 Win11 官方的
//! `DWMWA_SYSTEMBACKDROP_TYPE` 只能跟随系统配色。需求里的"多色彩配色"必须靠前者。
//!
//! ## 分层关系（很重要）
//!
//! ```text
//!   DWM 背景材质（模糊，可选）   ← 本模块负责
//!   ────────────────────────
//!   DComp 视觉树：我们绘制的面板  ← render 层负责，颜色来自配色方案
//! ```
//!
//! 本模块只提供**模糊**，不给系统传我们的颜色 —— 因为颜色由我们自己绘制的面板提供，
//! 若两边都着色会出现"双重上色"：面板设成蓝色、系统再叠一层蓝，
//! 最终比配置的颜色深得多，用户改配色时所见非所得。
//! 所以传给系统的着色 alpha 固定为 1（既保留模糊，又几乎不着色）。
//!
//! 已知副作用：`ACCENT_ENABLE_ACRYLICBLURBEHIND` 在部分 Win10 版本上
//! 会让窗口拖动发涩。我们用系统拖动（`HTCAPTION`）而非手动 `SetWindowPos`，
//! 已经规避了主要的卡顿来源。

use std::sync::OnceLock;

use windows::Win32::Foundation::{FARPROC, HMODULE, HWND};
use windows::Win32::Graphics::Dwm::{
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DwmSetWindowAttribute,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
// BOOL 在 windows-core 里（windows crate 通过 `pub use windows_core as core` 转出），
// 不在 Win32::Foundation 下。
use windows::core::{BOOL, PCSTR, w};

use crate::config::theme::{Backdrop, Color};

// ---- 未公开 API 的声明 ----

/// `WINDOWCOMPOSITIONATTRIB`：本模块只用到 `WCA_ACCENT_POLICY`。
const WCA_ACCENT_POLICY: u32 = 19;

/// `ACCENT_STATE`
const ACCENT_DISABLED: u32 = 0;
const ACCENT_ENABLE_BLURBEHIND: u32 = 3;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

/// `ACCENT_POLICY`
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    /// **ABGR** 布局，不是 ARGB。见 `Color::to_abgr`。
    gradient_color: u32,
    animation_id: u32,
}

/// `WINDOWCOMPOSITIONATTRIBDATA`
#[repr(C)]
struct WindowCompositionAttributeData {
    attribute: u32,
    data: *mut core::ffi::c_void,
    size_of_data: usize,
}

/// `SetWindowCompositionAttribute` 的函数签名。
type SetWcaFn = unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> BOOL;

/// 运行期解析 `SetWindowCompositionAttribute` 的地址，只解析一次。
///
/// ## 为什么必须动态解析
///
/// 最直觉的写法是 `#[link(name = "user32")] extern { fn SetWindowCompositionAttribute(..); }`，
/// 但这会**链接失败**：
/// ```
/// error LNK2019: unresolved external symbol __imp_SetWindowCompositionAttribute
/// ```
/// 原因是 `user32.lib` 只导出**有文档**的符号表，这个未公开函数虽然在
/// `user32.dll` 里真实存在，却不在导入库里。必须运行期用 `GetProcAddress` 取地址。
///
/// 这也是为什么"复制别人的 C++ 代码"在这里行不通：C++ 那边同样要
/// `GetProcAddress`，很少有人把这一步一并抄过来。
fn resolve_set_wca() -> Option<SetWcaFn> {
    static CACHE: OnceLock<Option<SetWcaFn>> = OnceLock::new();

    *CACHE.get_or_init(|| {
        const PROC_NAME: &[u8] = b"SetWindowCompositionAttribute\0";

        // SAFETY: 两个名字都是 'static 字节串；取得地址后按匹配的签名转换。
        // 系统若没有该导出，返回 None，上层按"不支持"处理。
        unsafe {
            let module: HMODULE = GetModuleHandleW(w!("user32.dll")).ok()?;
            let proc: FARPROC = GetProcAddress(module, PCSTR(PROC_NAME.as_ptr()));
            proc.map(|p| {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, SetWcaFn>(p)
            })
        }
    })
}

// ---- DWM 官方材质的取值 ----

/// `DWM_SYSTEMBACKDROP_TYPE`
const DWMSBT_MAINWINDOW: i32 = 2; // Mica
const DWMSBT_TRANSIENTWINDOW: i32 = 3; // 系统亚克力

/// `DWM_WINDOW_CORNER_PREFERENCE`
const DWMWCP_DONOTROUND: i32 = 1;
const DWMWCP_ROUND: i32 = 2;

/// 应用背景材质，返回**实际生效**的方式。
///
/// `round_corners` 为假时要求系统**不要**给窗口加圆角 —— 用于"关闭一切窗口外效果"
/// 的平面模式：此时我们自己也不画圆角，窗口就是一个完整的矩形，
/// 边角处不会留下任何透明区域。
///
/// 不返回 `Result`：材质是纯装饰，某台机器上不支持也不该让程序起不来，
/// 降级即可。返回值供日志和设置界面显示"你选的没生效，已降级为 X"。
pub fn apply(hwnd: HWND, requested: Backdrop, tint: Color, round_corners: bool) -> Backdrop {
    // 圆角与深色标题栏与材质无关，先设上；失败不影响功能
    set_round_corners(hwnd, round_corners);
    set_dark_titlebar(hwnd);

    // 只取模糊、不上色：颜色由我们绘制的面板负责
    let blur_only = tint.with_alpha(1).to_abgr();

    match requested {
        Backdrop::None => {
            set_accent(hwnd, ACCENT_DISABLED, 0);
            Backdrop::None
        }
        Backdrop::Blur => {
            if set_accent(hwnd, ACCENT_ENABLE_BLURBEHIND, 0) {
                Backdrop::Blur
            } else {
                Backdrop::None
            }
        }
        Backdrop::Acrylic => {
            if set_accent(hwnd, ACCENT_ENABLE_ACRYLICBLURBEHIND, blur_only) {
                Backdrop::Acrylic
            } else if set_system_backdrop(hwnd, DWMSBT_TRANSIENTWINDOW) {
                // 降级：Win11 官方亚克力，模糊在、但色调跟随系统
                Backdrop::Mica
            } else {
                Backdrop::None
            }
        }
        Backdrop::Mica => {
            if set_system_backdrop(hwnd, DWMSBT_MAINWINDOW) {
                Backdrop::Mica
            } else if set_accent(hwnd, ACCENT_ENABLE_ACRYLICBLURBEHIND, blur_only) {
                Backdrop::Acrylic
            } else {
                Backdrop::None
            }
        }
    }
}

/// 调未公开 API 设置强调策略。
fn set_accent(hwnd: HWND, state: u32, gradient_color: u32) -> bool {
    let Some(func) = resolve_set_wca() else {
        // 该系统没有这个未公开导出，交给调用方降级
        return false;
    };

    let mut policy = AccentPolicy {
        accent_state: state,
        accent_flags: 0,
        gradient_color,
        animation_id: 0,
    };
    let mut data = WindowCompositionAttributeData {
        attribute: WCA_ACCENT_POLICY,
        data: &mut policy as *mut AccentPolicy as *mut core::ffi::c_void,
        size_of_data: std::mem::size_of::<AccentPolicy>(),
    };

    // SAFETY: data 指向本函数栈上的 policy，生命周期覆盖整个调用；
    // func 是按真实签名转换得到的函数指针。
    unsafe { func(hwnd, &mut data) }.as_bool()
}

/// 调 DwmSetWindowAttribute 设置系统背景材质。不支持的版本会返回错误。
fn set_system_backdrop(hwnd: HWND, kind: i32) -> bool {
    // SAFETY: 传入的是本函数栈上 i32 的地址，长度参数与之一致。
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &kind as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        )
    }
    .is_ok()
}

/// 设置窗口圆角偏好。老系统上静默失败。
///
/// 注意这是**系统侧**的圆角：即便我们自己把面板画成直角，
/// 只要这里还要求圆角，DWM 仍会把窗口角部裁圆，露出后面的桌面。
/// 要做到"窗口外什么都没有"，这里必须一起设成不圆角。
fn set_round_corners(hwnd: HWND, round: bool) {
    let preference: i32 = if round { DWMWCP_ROUND } else { DWMWCP_DONOTROUND };
    // SAFETY: 传入的是本函数栈上 i32 的地址，长度参数与之一致。
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        )
    };
}

/// 深色标题栏。我们的窗口没有系统标题栏，但设上它可让系统对话框与阴影配色协调。
fn set_dark_titlebar(hwnd: HWND) {
    let dark = BOOL(1);
    // SAFETY: 同 set_system_backdrop。
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const BOOL as *const core::ffi::c_void,
            std::mem::size_of::<BOOL>() as u32,
        )
    };
}
