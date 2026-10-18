//! 各类绘制器与共用的小工具。
//!
//! 每个绘制器只负责一种视觉元素，彼此不引用；
//! 需要的公共换算（颜色、圆角矩形）放在本模块，避免各写一份。

pub mod caption;
pub mod grid;
pub mod menu;
pub mod panel;
pub mod settings;
pub mod sidebar;

pub use caption::CaptionPainter;
pub use grid::GridPainter;
pub use panel::PanelPainter;
pub use settings::{SettingsItem, SettingsPainter};
pub use sidebar::SidebarPainter;
// 菜单类型不在这里 re-export：它还没接到 App 上，
// 提前导出只会产生"未使用导入"的噪声。接线时按需要引入即可。

use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT;

use crate::config::theme::Color;
use crate::platform::hittest::RectI;

/// `0xAARRGGBB` → D2D 的 `0.0~1.0` 浮点分量。
pub fn to_d2d(color: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: color.r() as f32 / 255.0,
        g: color.g() as f32 / 255.0,
        b: color.b() as f32 / 255.0,
        a: color.a() as f32 / 255.0,
    }
}

/// 浮点矩形 + 圆角半径 → D2D 圆角矩形。
pub fn rounded(rect: (f32, f32, f32, f32), radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: rect.0,
            top: rect.1,
            right: rect.2,
            bottom: rect.3,
        },
        radiusX: radius,
        radiusY: radius,
    }
}

/// 整数矩形（命中测试用的那种）→ D2D 圆角矩形。
pub fn rounded_i(rect: RectI, radius: f32) -> D2D1_ROUNDED_RECT {
    rounded(
        (
            rect.x as f32,
            rect.y as f32,
            (rect.x + rect.w) as f32,
            (rect.y + rect.h) as f32,
        ),
        radius,
    )
}

/// 描边矩形：把 `(left, top, right, bottom)` **四边各内缩半个线宽**。
///
/// ## 为什么必须有这个函数（2026-10-18）
///
/// D2D 的描边是**以路径为中心**向两侧各画一半。矩形正好压在窗口边界上时，
/// 外侧那一半会溢出到窗口外被裁掉（描边看起来细一半，玻璃关闭时尤其明显）。
/// 所以描边矩形要向内缩半个线宽 —— 这条规则原先在**两处各写了一份**：
/// 主窗口的 `panel.rs`（对的）与弹窗的 `popout/layout.rs`（写错了）。
///
/// 写错的那份把元组当成 `(left, top, width, height)` 来填，
/// 而消费方 `rounded()` 要的是 `right/bottom`，于是右/下多缩了半个线宽、
/// **描边不对称** —— 使用者报的"弹窗底部两角有浅色边"就是这个。
/// 对的那份**拦不住错的那份**，所以规则只能有一处（架构文档 §17.9 / §17.14）。
///
/// 与 [`inset_stroke_radius`] 分开成两个函数，是因为调用方需要它们的**时机不同**：
/// 弹窗算布局时还不知道半径（半径来自装配层的配色），所以只能先要矩形。
/// 硬合成一个"返回一对"的函数会逼出"传个假半径、只取一半结果"的写法。
pub fn inset_stroke_rect(rect: (f32, f32, f32, f32), stroke_width: f32) -> (f32, f32, f32, f32) {
    let half = stroke_width / 2.0;
    (rect.0 + half, rect.1 + half, rect.2 - half, rect.3 - half)
}

/// 描边半径：与 [`inset_stroke_rect`] 缩同样的量。
///
/// 内缩了多少，半径就减多少 —— 这样描边的**中线**与填充轮廓**同心**，
/// 转角处不会一边贴着填充、一边留缝。减到负值没有意义，夹到 0。
pub fn inset_stroke_radius(radius: f32, stroke_width: f32) -> f32 {
    (radius - stroke_width / 2.0).max(0.0)
}
