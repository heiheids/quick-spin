//! 标题栏按钮：圆角方框 + 最小化横线 / 关闭叉。
//!
//! 图标用线条自绘而不是用字体里的 `─` `✕`：字体缺字时会变成豆腐块，
//! 而线稿在任何系统上都一样，且能精确控制粗细与位置。

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Direct2D::*;
use windows_numerics::Vector2;

use crate::config::theme::{Color, Scheme};
use crate::error::{Result, Win32Ext};
use crate::platform::hittest::RectI;
use crate::ui::layout::CaptionButton;

use super::{rounded_i, to_d2d};

/// 按钮框的圆角半径
const FRAME_RADIUS: f32 = 9.0;
/// 符号线条粗细
const STROKE: f32 = 1.4;
/// 符号半径（叉的半臂长 / 横线的半长）
const SYMBOL: f32 = 5.5;

pub struct CaptionPainter {
    /// 一个实色笔刷，靠 `SetColor` 复用于描边与符号。
    /// 按钮只有两个，为每个状态单独建笔刷不值得。
    brush: Option<ID2D1SolidColorBrush>,
}

impl CaptionPainter {
    pub fn new() -> Self {
        Self { brush: None }
    }

    /// 首次绘制时创建笔刷。之后复用。
    pub fn ensure(&mut self, ctx: &ID2D1DeviceContext) -> Result<()> {
        if self.brush.is_none() {
            let white = D2D1_COLOR_F {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            };
            // SAFETY: COM 调用；white 是栈上局部量。
            let brush = unsafe { ctx.CreateSolidColorBrush(&white, None) }
                .ctx("CreateSolidColorBrush")?;
            self.brush = Some(brush);
        }
        Ok(())
    }

    /// 画一个按钮。
    pub fn draw(
        &self,
        ctx: &ID2D1DeviceContext,
        rect: RectI,
        kind: CaptionButton,
        hovered: bool,
        scheme: &Scheme,
        opacity: f32,
    ) -> Result<()> {
        let Some(brush) = &self.brush else {
            return Ok(());
        };

        let frame = rounded_i(rect, FRAME_RADIUS);
        let center = (
            rect.x as f32 + rect.w as f32 / 2.0,
            rect.y as f32 + rect.h as f32 / 2.0,
        );
        let no_style: Option<&ID2D1StrokeStyle> = None;

        // SAFETY: COM 调用；所有结构体都是本函数栈上的局部量。
        unsafe {
            // 悬停底色：用强调色的弱化版本，和整机配色保持一致
            if hovered {
                let fill: Color = scheme.accent_soft.with_alpha(
                    (scheme.accent_soft.a() as f32 * opacity).round() as u8,
                );
                brush.SetColor(&to_d2d(fill));
                ctx.FillRoundedRectangle(&frame, brush);
            }

            // 描边
            let outline = if hovered {
                scheme.text_primary
            } else {
                scheme.text_secondary
            };
            brush.SetColor(&to_d2d(outline));
            ctx.DrawRoundedRectangle(&frame, brush, 1.0, no_style);

            // 符号
            match kind {
                CaptionButton::HideDesktopIcons => {
                    // "桌面 + 斜杠"：矩形表示桌面，一条对角斜线表示"隐藏/不可见"。
                    // 斜杠是通用的隐藏标记，比画一个小房子更直观，
                    // 而且在 34px 的按钮里仍然看得清。
                    let half = SYMBOL;
                    let rect = windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
                        left: center.0 - half,
                        top: center.1 - half,
                        right: center.0 + half,
                        bottom: center.1 + half,
                    };
                    ctx.DrawRectangle(&rect, brush, STROKE, no_style);
                    // 斜杠要**略超出**矩形四角，否则看着像框里的一条装饰线，
                    // 而不是"划掉"整个桌面。
                    let over = half * 1.25;
                    ctx.DrawLine(
                        Vector2 {
                            X: center.0 - over,
                            Y: center.1 + over,
                        },
                        Vector2 {
                            X: center.0 + over,
                            Y: center.1 - over,
                        },
                        brush,
                        STROKE,
                        no_style,
                    );
                }
                CaptionButton::Add => {
                    // 加号：一横一竖。用和最小化/关闭同一套线宽与半长，
                    // 保证三个按钮的图形重量一致。
                    ctx.DrawLine(
                        Vector2 {
                            X: center.0 - SYMBOL,
                            Y: center.1,
                        },
                        Vector2 {
                            X: center.0 + SYMBOL,
                            Y: center.1,
                        },
                        brush,
                        STROKE,
                        no_style,
                    );
                    ctx.DrawLine(
                        Vector2 {
                            X: center.0,
                            Y: center.1 - SYMBOL,
                        },
                        Vector2 {
                            X: center.0,
                            Y: center.1 + SYMBOL,
                        },
                        brush,
                        STROKE,
                        no_style,
                    );
                }
                CaptionButton::Minimize => {
                    ctx.DrawLine(
                        Vector2 {
                            X: center.0 - SYMBOL,
                            Y: center.1,
                        },
                        Vector2 {
                            X: center.0 + SYMBOL,
                            Y: center.1,
                        },
                        brush,
                        STROKE,
                        no_style,
                    );
                }
                CaptionButton::Close => {
                    // 两条对角线组成叉
                    ctx.DrawLine(
                        Vector2 {
                            X: center.0 - SYMBOL,
                            Y: center.1 - SYMBOL,
                        },
                        Vector2 {
                            X: center.0 + SYMBOL,
                            Y: center.1 + SYMBOL,
                        },
                        brush,
                        STROKE,
                        no_style,
                    );
                    ctx.DrawLine(
                        Vector2 {
                            X: center.0 - SYMBOL,
                            Y: center.1 + SYMBOL,
                        },
                        Vector2 {
                            X: center.0 + SYMBOL,
                            Y: center.1 - SYMBOL,
                        },
                        brush,
                        STROKE,
                        no_style,
                    );
                }
            }
        }

        Ok(())
    }
}

impl Default for CaptionPainter {
    fn default() -> Self {
        Self::new()
    }
}
