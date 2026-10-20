//! 悬停提示：鼠标在方格上停住 0.3 秒后，浮出**完整名称**。
//!
//! # 为什么需要它
//!
//! 方格下方的名称是定宽单行，超长会被截断成 `Developer Command Pro…`（见
//! `render::text` 里的 trimming）。用户看不出那是哪个程序 —— 尤其名字前半段
//! 相同的几个（`Visual Studio 2022` / `Visual Studio Code`）。
//!
//! # 为什么是"延迟弹出"而不是立刻跟着鼠标走
//!
//! 立刻弹出会在鼠标划过一排方格时连续闪出十几个提示，既晃眼又挡视线。
//! 停住 0.3 秒才弹，表达的是"我确实在看这一个"。
//!
//! 定时由窗口层的 [`crate::platform::window::Window::set_timeout`] 承担，
//! **不是轮询**：空闲时线程仍然睡在 `GetMessageW` 上（本程序的省电铁律）。
//!
//! # 与菜单的分工
//!
//! 提示是纯展示，不接点击、不进命中测试。所以它画在所有内容之上、
//! 但**永远不参与** `Layout` 的命中计算 —— 用户点在提示上时，那一下仍然
//! 落在它下面的方格上（这是对的：提示只是个标签）。

use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::*;

use crate::config::theme::Scheme;
use crate::error::{Result, Win32Ext};
use crate::platform::hittest::RectI;
use crate::render::text::{Role, TextEngine};

use super::{rounded_i, to_d2d};

/// 鼠标停住多久才弹出（毫秒）。使用者指定 0.3 秒。
pub const DELAY_MS: u32 = 300;

/// 提示框的内边距（逻辑像素）。
const PADDING_X: i32 = 10;
const PADDING_Y: i32 = 6;
/// 圆角。
const RADIUS: f32 = 6.0;
/// 与方格之间的间隔，避免提示压住图标本身。
const GAP: i32 = 6;
/// 单个字符的宽度估算（与 `painters::menu` 同一个思路：中文按上限取值）。
///
/// 不用 DirectWrite 精确测量：提示的位置与尺寸要在**弹出的那一刻**就定下来，
/// 而那时还没开始绘制。估宽一点只是右边多留些空白，估窄了才会把字裁掉。
pub const CHAR_WIDTH: i32 = 14;
/// 提示框的最小/最大宽度。太长的话一行读下来会跑出窗口，超过就截断。
const MIN_W: i32 = 60;
const MAX_W: i32 = 320;

pub struct TooltipPainter {
    brush: Option<ID2D1SolidColorBrush>,
}

impl TooltipPainter {
    pub fn new() -> Self {
        Self { brush: None }
    }

    pub fn ensure(&mut self, ctx: &ID2D1DeviceContext) -> Result<()> {
        if self.brush.is_none() {
            let white = D2D1_COLOR_F {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            };
            // SAFETY: COM 调用；white 是栈上局部量。
            self.brush = Some(
                unsafe { ctx.CreateSolidColorBrush(&white, None) }
                    .ctx("CreateSolidColorBrush")?,
            );
        }
        Ok(())
    }

    /// 算提示框该画在哪。
    ///
    /// `anchor` 是被指的方格矩形，`bounds` 是允许的整个客户区（逻辑像素）。
    ///
    /// 默认放在方格**下方**（与方格名称同侧，视线不用跳）；下方放不下就翻到
    /// 上方；再放不下就夹进窗口内 —— 宁可压住图标一点，也不要画到窗口外面
    ///（窗口外画了也看不见，等于提示丢了）。
    pub fn frame(anchor: RectI, text: &str, bounds: RectI) -> RectI {
        let chars = text.chars().count() as i32;
        let width = (chars * CHAR_WIDTH + PADDING_X * 2).clamp(MIN_W, MAX_W);
        let height = 24 + PADDING_Y * 2;

        // 水平居中于方格
        let mut x = anchor.x + (anchor.w - width) / 2;
        x = x.clamp(bounds.x, (bounds.x + bounds.w - width).max(bounds.x));

        let below = anchor.y + anchor.h + GAP;
        let above = anchor.y - GAP - height;
        let y = if below + height <= bounds.y + bounds.h {
            below
        } else if above >= bounds.y {
            above
        } else {
            // 上下都放不下：贴在方格下沿并在窗口内夹紧
            below.clamp(bounds.y, (bounds.y + bounds.h - height).max(bounds.y))
        };

        RectI::new(x, y, width, height)
    }

    /// 画提示。`frame` 由 [`Self::frame`] 算出（绘制不做几何决策）。
    pub fn draw(
        &self,
        ctx: &ID2D1DeviceContext,
        text: &mut TextEngine,
        frame: RectI,
        label: &str,
        scheme: &Scheme,
    ) -> Result<()> {
        let Some(brush) = &self.brush else {
            return Ok(());
        };

        // SAFETY: COM 调用；结构体都是本函数栈上的局部量。
        unsafe {
            // 底色必须**不透明**：提示是给"看不清名字"的人用的，
            // 背后透出图标与名称反而更读不清。
            brush.SetColor(&to_d2d(scheme.panel.midpoint().with_alpha(255)));
            ctx.FillRoundedRectangle(&rounded_i(frame, RADIUS), brush);

            // 与右键菜单同一套描边（深色内线 + 浅色外线），
            // 两个浮层长得一致，用户不用猜这是不是同一种东西。
            brush.SetColor(&to_d2d(scheme.tab_active_border));
            ctx.DrawRoundedRectangle(&rounded_i(frame, RADIUS), brush, 1.0, None);
        }

        let inner = (
            frame.x as f32 + PADDING_X as f32,
            frame.y as f32,
            (frame.w - PADDING_X * 2) as f32,
            frame.h as f32,
        );
        text.draw(ctx, label, Role::Tile, inner, scheme.text_primary)?;

        Ok(())
    }
}

/// 提示框里的文字该不该截断。返回可直接绘制的字符串。
///
/// 按估算宽度截断并补省略号：`TextEngine` 自己也会按矩形裁（带 `…`），
/// 但这里更早截断能让提示框**宽度与内容匹配**，否则一个超长名字会撑出一个
/// 320px 宽、右边大片空白的框。
pub fn fit_label(text: &str, _frame_w: i32) -> String {
    let max_chars = ((MAX_W - PADDING_X * 2) / CHAR_WIDTH).max(1) as usize;
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// 未使用的 D2D 矩形辅助（保留以便将来需要精确文字布局时使用）。
#[allow(dead_code)]
fn as_d2d(rect: RectI) -> D2D_RECT_F {
    D2D_RECT_F {
        left: rect.x as f32,
        top: rect.y as f32,
        right: (rect.x + rect.w) as f32,
        bottom: (rect.y + rect.h) as f32,
    }
}
