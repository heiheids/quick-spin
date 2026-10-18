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
