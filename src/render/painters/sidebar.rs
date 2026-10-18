//! 侧栏：选项卡按钮 + 左下角设置按钮 + 分隔线。
//!
//! ## 圆形的画法
//!
//! 设置按钮是圆的。这里不用 `DrawEllipse`，而是用"圆角半径取边长一半的圆角矩形" ——
//! 当 `radiusX = width/2`、`radiusY = height/2` 时，D2D 画出来的就是正圆。
//! 这样整个模块只需要 `DrawRoundedRectangle` 与 `DrawLine` 两个已验证可用的调用，
//! 少一类 API 就少一类踩坑面。
//!
//! ## 图标占位
//!
//! 真实图标提取尚未接入，图标方块先画成一个带底色的圆角方块。
//! 图标到位后把位图画进这个方块即可，布局与命中测试都不需要改。

use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::*;
use windows_numerics::Vector2;

use crate::config::theme::Scheme;
use crate::error::{Result, Win32Ext};
use crate::render::{Role, TextEngine};
use crate::ui::layout::Layout;

use super::{rounded, rounded_i, to_d2d};

/// 选项卡按钮的圆角。
///
/// 收到 4（原来是 12）：按钮宽度只有 62px，12 的圆角会把四角吃掉一大块，
/// 文字反而显得被挤在中间。超小圆角让按钮更像一个方正的"文字格"。
const TAB_RADIUS: f32 = 4.0;
/// 设置齿轮的齿数
const GEAR_TEETH: usize = 8;

pub struct SidebarPainter {
    brush: Option<ID2D1SolidColorBrush>,
}

impl SidebarPainter {
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

    /// 画整个侧栏。
    ///
    /// `labels` 与 `layout.tab_buttons` 一一对应。
    ///
    /// 没有 `icons` 参数：选项卡已改为**纯文字按钮**（需求），不显示图标。
    /// 与其留一个永远传 `None` 的参数，不如删掉 —— 空参数会让人以为图标还能用。
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        ctx: &ID2D1DeviceContext,
        text: &mut TextEngine,
        layout: &Layout,
        scheme: &Scheme,
        labels: &[String],
        active: usize,
        hover_tab: Option<usize>,
        // 拖动排序时的落点。画成强调色外框，让用户看清会插到哪。
        drop_target: Option<usize>,
        hover_settings: bool,
        opacity: f32,
    ) -> Result<()> {
        let Some(brush) = &self.brush else {
            return Ok(());
        };
        let no_style: Option<&ID2D1StrokeStyle> = None;

        // SAFETY: COM 调用；结构体都是本函数栈上的局部量。
        unsafe {
            // ---- 侧栏底色 ----
            //
            // 这里**不能**直接填一个直角矩形：侧栏一直铺到面板底边，
            // 面板本身是圆角的，于是左下角会露出一个直角尖，顶在圆角外面
            // （这是肉眼可见的破绽，尤其在浅色背景上）。
            //
            // 做法：先用轴对齐裁剪把范围限定在侧栏矩形内，再填一个**四角都带面板圆角**
            // 的矩形 —— 左侧两个圆角正好与面板重合，右侧两个被裁掉，右上角落在标题栏
            // 分界处也看不出来。D2D 的圆角矩形只支持统一半径，做不出"只有左下角圆"，
            // 所以只能靠裁剪配合。
            let bg = scheme
                .sidebar_bg
                .with_alpha((scheme.sidebar_bg.a() as f32 * opacity).round() as u8);
            brush.SetColor(&to_d2d(bg));

            let sidebar = layout.sidebar;
            let clip = D2D_RECT_F {
                left: sidebar.x as f32,
                top: sidebar.y as f32,
                right: (sidebar.x + sidebar.w) as f32,
                bottom: (sidebar.y + sidebar.h) as f32,
            };
            // 注意：这里的 PushAxisAlignedClip / PopAxisAlignedClip 走的是
            // ID2D1RenderTarget 上的版本，返回 `()` 而不是 `Result`，
            // 所以不加 `.ctx()`（加了会报 "no method named ctx on unit type"）。
            ctx.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);

            // 右边界和上边界故意画到裁剪区之外，让那两对圆角被裁掉
            let (pl, pt, _, pb) = layout.panel;
            let fill = rounded((pl, pt, pl + sidebar.w as f32 + 400.0, pb), layout.radius);
            ctx.FillRoundedRectangle(&fill, brush);

            ctx.PopAxisAlignedClip();

            // ---- 分隔线已移除 ----
            //
            // 侧栏底色与内容区底色本身就是两块不同的面，边界已经能看出来；
            // 再描一条竖线属于重复信息，而且在高分屏上会有半像素的模糊——
            // 看起来像渲染瑕疵而不是设计。靠**色差**分区比靠线更干净。

            // ---- 选项卡按钮：纯文字 + 外框 ----
            //
            // 需求："左侧选项卡改为纯文字按钮，去掉底色，只保留外框"。
            //
            // 去掉的是两样东西：按钮的底色填充，以及图标方块（含真实图标）。
            // 侧栏只有 104px 宽，图标方块占掉一大半高度，文字反而被挤到角落里；
            // 只留外框 + 文字，信息密度和可读性都更好。
            //
            // 于是"当前选中"只剩外框颜色这一个信号，所以：
            //   * 选中：强调色、1.5px
            //   * 未选中：极淡的中性色、1px
            // 未选中也给框是刻意的 —— 只给选中项画框会让按钮"跳出来"，
            // 而用户需要先看到有哪几个可点。
            for (i, rect) in layout.tab_buttons.iter().enumerate() {
                let is_active = i == active;
                let is_hover = hover_tab == Some(i);
                let frame = rounded_i(*rect, TAB_RADIUS);

                // 悬停时加一层提亮叠色，用来反馈"鼠标在这里"。
                // 它不是"底色"—— 只在悬停瞬间出现，静止时按钮上什么都没有。
                //
                // 这里**不额外压低 alpha**。原来乘了个 0.6，在那时是合理的
                //（悬停色偏重，压一压更协调）；改成统一的提亮白纱之后，
                // 再乘 0.6 会让侧栏的悬停明显弱于菜单和设置面板 ——
                // 需求是"所有按钮和菜单"用同一种变淡方式，强度就不该分档。
                if is_hover && !is_active {
                    let fill = scheme.tab_hover;
                    brush.SetColor(&to_d2d(
                        fill.with_alpha((fill.a() as f32 * opacity).round() as u8),
                    ));
                    ctx.FillRoundedRectangle(&frame, brush);
                }

                if is_active {
                    brush.SetColor(&to_d2d(scheme.tab_active_border));
                    ctx.DrawRoundedRectangle(&frame, brush, 1.5, no_style);
                } else {
                    // 未选中的框要**很淡**：它的作用是暗示"这里可以点"，
                    // 而不是和选中项抢注意力。
                    // 未选中的框用**和面板边框同一个颜色**（`border.from`）。
                    //
                    // 原来拿的是 `tab_label`（文字色）—— 边框和文字共用颜色，
                    // 在交界处就多出一种无关的颜色。统一之后，
                    // 选中与未选中的差别只体现在**线的粗细和透明度**上，而不是色相。
                    let edge = scheme
                        .border
                        .from
                        .with_alpha((scheme.border.from.a() as f32 * opacity * 0.55).round() as u8);
                    brush.SetColor(&to_d2d(edge));
                    ctx.DrawRoundedRectangle(&frame, brush, 1.0, no_style);
                }

                // 拖动落点：加粗强调色外框。
                // 只看鼠标位置，用户判断不出"会插到第几个"，必须有明确的落点标记。
                if drop_target == Some(i) {
                    brush.SetColor(&to_d2d(scheme.accent));
                    ctx.DrawRoundedRectangle(&frame, brush, 2.0, no_style);
                }
            }
        }

        // ---- 选项卡文字（走文本引擎，需要 &mut）----
        for (i, rect) in layout.tab_buttons.iter().enumerate() {
            let Some(label) = labels.get(i) else {
                continue;
            };
            let color = if i == active {
                scheme.text_primary
            } else {
                scheme.tab_label
            };
            // 文字占满整个按钮。之前是压在图标方块下方那一条 ——
            // 现在选项卡是纯文字按钮，没有图标方块了，再压在下半条就会偏下。
            // 垂直居中由文本格式的 `DWRITE_PARAGRAPH_ALIGNMENT_CENTER` 负责，
            // 水平居中由 `DWRITE_TEXT_ALIGNMENT_CENTER` 负责，这里只要给满整个矩形。
            let label_rect = (rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32);
            text.draw(ctx, label, Role::Tab, label_rect, color)?;
        }

        // ---- 设置按钮：圆 + 齿 ----
        self.draw_gear(ctx, brush, layout, scheme, hover_settings)?;
        Ok(())
    }

    /// 在标题栏底边画一条**贯穿整个面板宽度**的分隔线。
    ///
    /// 需求："浅色时左侧栏与上面色差太突兀，在这里用外框同色绘制一条直线，
    /// 分割上下部分"。
    ///
    /// 用 `scheme.border` 的颜色，和窗口外框同色 —— 这样它在**所有方案**下
    /// 都自动成立（边框色是按各方案色系派生的），不必为浅色主题单独配色。
    ///
    /// **必须由调用方在方格之后调用**：方格框的上边缘正好压在这条线的位置
    ///（`GRID_PAD_TOP = 0`），先画就会被第一个方格的底色盖掉，
    /// 表现为"线在侧栏那一小段有、到了内容区就没了"。
    pub fn draw_title_separator(
        &self,
        ctx: &ID2D1DeviceContext,
        layout: &Layout,
        scheme: &Scheme,
    ) -> Result<()> {
        let Some(brush) = &self.brush else {
            return Ok(());
        };
        let no_style: Option<&ID2D1StrokeStyle> = None;

        // +0.5 让 1px 的线落在像素中心，否则会被抗锯齿摊成两条半透明的灰线。
        let y = layout.sidebar.y as f32 + 0.5;
        // `panel` 是 `(左, 上, 右, 下)` 的元组，不是 `RectI`
        let left = layout.panel.0;
        let right = layout.panel.2;

        // SAFETY: COM 调用；坐标都是栈上局部量。
        unsafe {
            brush.SetColor(&to_d2d(scheme.border.from));
            ctx.DrawLine(
                Vector2 { X: left, Y: y },
                Vector2 { X: right, Y: y },
                brush,
                1.0,
                no_style,
            );
        }
        Ok(())
    }

    /// 左下角设置按钮。
    fn draw_gear(
        &self,
        ctx: &ID2D1DeviceContext,
        brush: &ID2D1SolidColorBrush,
        layout: &Layout,
        scheme: &Scheme,
        hovered: bool,
    ) -> Result<()> {
        let rect = layout.settings_button;
        let radius = rect.w as f32 / 2.0;
        let center = (rect.x as f32 + radius, rect.y as f32 + radius);
        let no_style: Option<&ID2D1StrokeStyle> = None;

        // SAFETY: COM 调用；结构体都是本函数栈上的局部量。
        unsafe {
            if hovered {
                brush.SetColor(&to_d2d(scheme.accent_soft));
                ctx.FillRoundedRectangle(&rounded_i(rect, radius), brush);
            }

            // 外圈：圆角半径取一半边长 ⇒ 正圆
            brush.SetColor(&to_d2d(if hovered {
                scheme.tab_active_border
            } else {
                scheme.text_secondary
            }));
            ctx.DrawRoundedRectangle(&rounded_i(rect, radius), brush, 1.4, no_style);

            // 齿轮：内圈 + 若干根径向短齿
            let inner = 7.0;
            for i in 0..GEAR_TEETH {
                let angle = i as f32 * std::f32::consts::TAU / GEAR_TEETH as f32;
                let (sin, cos) = angle.sin_cos();
                ctx.DrawLine(
                    Vector2 {
                        X: center.0 + cos * inner,
                        Y: center.1 + sin * inner,
                    },
                    Vector2 {
                        X: center.0 + cos * (inner + 3.0),
                        Y: center.1 + sin * (inner + 3.0),
                    },
                    brush,
                    1.6,
                    no_style,
                );
            }
            // 中心小圆（同样用圆角矩形画）
            let hub = 5.0;
            let hub_rect = crate::platform::hittest::RectI::new(
                (center.0 - hub) as i32,
                (center.1 - hub) as i32,
                (hub * 2.0) as i32,
                (hub * 2.0) as i32,
            );
            ctx.DrawRoundedRectangle(&rounded_i(hub_rect, hub), brush, 1.4, no_style);
        }
        Ok(())
    }
}

impl Default for SidebarPainter {
    fn default() -> Self {
        Self::new()
    }
}
