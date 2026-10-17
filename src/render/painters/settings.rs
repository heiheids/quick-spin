//! 设置面板：覆盖在内容区之上的一层。
//!
//! 只负责画，不负责判断点了哪个开关 —— 命中用的矩形在 `ui::layout` 里算好，
//! 绘制这边直接用，保证"画在哪里"和"点在哪里"来自同一份数据。

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Direct2D::*;
use windows_numerics::Vector2;

use crate::config::theme::{Color, Scheme};
use crate::error::{Result, Win32Ext};
use crate::render::{Role, TextEngine};
use crate::ui::layout::Layout;

use super::{rounded_i, to_d2d};

/// 一个设置项。
pub struct SettingsItem<'a> {
    pub label: &'a str,
    pub on: bool,
    /// 是否绘制右侧的开关。
    ///
    /// 有些设置项**不是开关**而是"从若干值里选一个"（配色方案）。
    /// 那种行如果还画一个开关，用户会以为它是能开能关的，点下去却换了颜色。
    pub show_toggle: bool,
}

/// 色板区与设置项之间的间距
const PALETTE_TITLE_OFFSET: f32 = 28.0;
/// 开关轨道的尺寸
const TRACK_W: f32 = 44.0;
const TRACK_H: f32 = 24.0;
/// 开关圆钮半径
const KNOB_R: f32 = 8.5;
/// 关闭按钮的圆角
const CLOSE_RADIUS: f32 = 8.0;

pub struct SettingsPainter {
    brush: Option<ID2D1SolidColorBrush>,
}

impl SettingsPainter {
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
                unsafe { ctx.CreateSolidColorBrush(&white, None) }.ctx("CreateSolidColorBrush")?,
            );
        }
        Ok(())
    }

    /// `title` 是面板标题；`items` 与 `layout.settings_rows` 一一对应。
    /// `swatches` 与 `layout.settings_swatches` 一一对应；`current_swatch` 是当前基础色的下标。
    /// `palette_title` 是色板区左上角的标题。
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        ctx: &ID2D1DeviceContext,
        text: &mut TextEngine,
        layout: &Layout,
        scheme: &Scheme,
        title: &str,
        items: &[SettingsItem<'_>],
        swatches: &[Color],
        current_swatch: Option<usize>,
        palette_title: &str,
        hover_row: Option<usize>,
        hover_close: bool,
    ) -> Result<()> {
        let Some(brush) = &self.brush else {
            return Ok(());
        };
        let no_style: Option<&ID2D1StrokeStyle> = None;
        let panel = layout.settings_panel;

        // SAFETY: COM 调用；结构体都是本函数栈上的局部量。
        unsafe {
            // ---- 面板本体 ----
            //
            // 这里**刻意不画遮罩**。原因有两层：
            //
            // 1. 没有需要压暗的东西了：设置面板展开时 App 根本不画网格
            //    （见 `App::draw`），内容区只剩主面板的渐变底色。
            // 2. 遮罩会盖住主窗口的描边。遮罩范围是内容区，而内容区右边界
            //    与面板右边界重合 —— 正好是 1.5px 描边所在的位置。
            //    结果是描边在标题栏以下被压暗成黑色，看起来像"边框断了"，
            //    用户会误判成滚动条没对齐。
            //
            // **完全不透明**：模态浮层背后不该有可辨认内容，
            // 留哪怕 5% 的透明度，浅色壁纸下都会透出来。
            // 不跟随"外圈玻璃"开关，也不乘 opacity。
            let frame = rounded_i(panel, 14.0);
            let base = scheme.panel.midpoint();
            brush.SetColor(&to_d2d(base.with_alpha(255)));
            ctx.FillRoundedRectangle(&frame, brush);
            // 外圈再补一层同色描边，盖掉圆角处的抗锯齿边缘
            ctx.DrawRoundedRectangle(&frame, brush, 1.0, no_style);
            // 用强调色描边，让设置面板明确是"浮在主面板之上的一层"。
            // 之前用的是几乎看不见的分隔线色，结果它和主面板糊成一片，
            // 连边框在哪都分不清。
            brush.SetColor(&to_d2d(scheme.tab_active_border.with_alpha(0x66)));
            ctx.DrawRoundedRectangle(&frame, brush, 1.2, no_style);

            // ---- 关闭按钮 ----
            let close = layout.settings_close;
            brush.SetColor(&to_d2d(if hover_close {
                scheme.text_primary
            } else {
                scheme.text_secondary
            }));
            ctx.DrawRoundedRectangle(&rounded_i(close, CLOSE_RADIUS), brush, 1.2, no_style);
            let cx = close.x as f32 + close.w as f32 / 2.0;
            let cy = close.y as f32 + close.h as f32 / 2.0;
            let d = 5.0;
            ctx.DrawLine(
                Vector2 { X: cx - d, Y: cy - d },
                Vector2 { X: cx + d, Y: cy + d },
                brush,
                1.4,
                no_style,
            );
            ctx.DrawLine(
                Vector2 { X: cx - d, Y: cy + d },
                Vector2 { X: cx + d, Y: cy - d },
                brush,
                1.4,
                no_style,
            );

            // ---- 每个设置项的开关 ----
            for (i, row) in layout.settings_rows.iter().enumerate() {
                let Some(item) = items.get(i) else {
                    continue;
                };

                // 行间分割线。画在**行的上沿**，最后一行下面不画 ——
                // 行下面画会在面板底边多出一条孤零零的线。
                if i > 0 {
                    let y = row.y as f32 - 0.5;
                    let line = scheme.text_secondary.with_alpha(0x28);
                    brush.SetColor(&to_d2d(line));
                    ctx.DrawLine(
                        Vector2 {
                            X: row.x as f32 + 12.0,
                            Y: y,
                        },
                        Vector2 {
                            X: (row.x + row.w) as f32 - 12.0,
                            Y: y,
                        },
                        brush,
                        1.0,
                        no_style,
                    );
                }

                if hover_row == Some(i) {
                    brush.SetColor(&to_d2d(scheme.tile_hover));
                    ctx.FillRoundedRectangle(&rounded_i(*row, 10.0), brush);
                }

                // 开关轨道：关时低对比，开时用强调色。
                //
                // 整段用 `show_toggle` 包住：配色方案那一行不是开关，
                // 画了开关会让人以为它只能开/关，而它其实是换颜色。
                if item.show_toggle {
                let track_x = (row.x + row.w) as f32 - TRACK_W - 12.0;
                let track_y = row.y as f32 + (row.h as f32 - TRACK_H) / 2.0;
                let track = rounded(
                    (track_x, track_y, track_x + TRACK_W, track_y + TRACK_H),
                    TRACK_H / 2.0,
                );
                brush.SetColor(&to_d2d(if item.on {
                    scheme.accent
                } else {
                    scheme.text_secondary.with_alpha(0x50)
                }));
                ctx.FillRoundedRectangle(&track, brush);

                // 圆钮
                let knob_cx = if item.on {
                    track_x + TRACK_W - TRACK_H / 2.0
                } else {
                    track_x + TRACK_H / 2.0
                };
                let knob_cy = track_y + TRACK_H / 2.0;
                let knob_rect = crate::platform::hittest::RectI::new(
                    (knob_cx - KNOB_R) as i32,
                    (knob_cy - KNOB_R) as i32,
                    (KNOB_R * 2.0) as i32,
                    (KNOB_R * 2.0) as i32,
                );
                brush.SetColor(&to_d2d(if item.on {
                    crate::config::theme::Color::WHITE
                } else {
                    scheme.text_secondary
                }));
                ctx.FillRoundedRectangle(&rounded_i(knob_rect, KNOB_R), brush);
                }
            }
        }

        // ---- 基础色色块网格 ----
        //
        // 每个色块直接用**它代表的那个颜色**填充，所以用户看到的就是实际效果，
        // 不是在抽象色卡里挑。当前选中的那一格加一圈强调色描边。
        if let Some(brush) = &self.brush {
            for (i, cell) in layout.settings_swatches.iter().enumerate() {
                let Some(color) = swatches.get(i) else {
                    break;
                };
                // SAFETY: COM 调用；颜色与矩形都是栈上局部量。
                unsafe {
                    brush.SetColor(&to_d2d(*color));
                    ctx.FillRoundedRectangle(&rounded_i(*cell, 4.0), brush);

                    if current_swatch == Some(i) {
                        brush.SetColor(&to_d2d(scheme.accent));
                        ctx.DrawRoundedRectangle(&rounded_i(*cell, 4.0), brush, 2.0, no_style);
                    } else {
                        // 未选中的给一圈极淡描边：浅色块落在浅色面板上时，
                        // 没有描边就分不出边界（尤其奶白、淡灰那几格）。
                        brush.SetColor(&to_d2d(scheme.text_secondary.with_alpha(0x30)));
                        ctx.DrawRoundedRectangle(&rounded_i(*cell, 4.0), brush, 1.0, no_style);
                    }
                }
            }
        }

        // ---- 文字（走文本引擎，需要 &mut）----
        // 面板标题
        let title_rect = (
            panel.x as f32 + 16.0,
            panel.y as f32 + 14.0,
            panel.w as f32 - 60.0,
            30.0,
        );
        text.draw(ctx, title, Role::Setting, title_rect, scheme.text_primary)?;

        // 每个设置项的标签
        for (i, row) in layout.settings_rows.iter().enumerate() {
            let Some(item) = items.get(i) else {
                continue;
            };
            let label_rect = (
                row.x as f32 + 12.0,
                row.y as f32,
                (row.w as f32 - TRACK_W - 40.0).max(20.0),
                row.h as f32,
            );
            text.draw(ctx, item.label, Role::Setting, label_rect, scheme.text_primary)?;
        }

        // 色板区标题，排在色块网格上方靠左。
        //
        // **左边距要和上面设置项的标签一致**（`row.x + 12.0`，见上面的 label_rect），
        // 直接用色块的 x 会少 12px，标题就比上面那几行文字偏左一截。
        if let Some(&first) = layout.settings_swatches.first() {
            let rect = (
                first.x as f32 + 12.0,
                first.y as f32 - PALETTE_TITLE_OFFSET,
                (panel.w as f32 - 32.0).max(20.0),
                PALETTE_TITLE_OFFSET,
            );
            text.draw(
                ctx,
                palette_title,
                Role::Setting,
                rect,
                scheme.text_primary,
            )?;
        }

        Ok(())
    }
}

impl Default for SettingsPainter {
    fn default() -> Self {
        Self::new()
    }
}

/// 由矩形与圆角直接构造（`rounded_i` 的浮点版本）。
fn rounded(rect: (f32, f32, f32, f32), radius: f32) -> D2D1_ROUNDED_RECT {
    super::rounded(rect, radius)
}
