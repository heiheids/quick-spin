//! 面板底板：渐变填充 + 双色描边 + 外发光。
//!
//! ## 为什么发光不用 D2D 的 `Shadow` 特效
//!
//! `Shadow` 需要一张离屏位图加一遍高斯卷积。我们是**按需重绘**，
//! 而拖拽缩放窗口会触发密集重绘，那意味着每次都要付一遍卷积的代价。
//!
//! 改用"由外向内若干圈递减透明度的同心描边"：圆角矩形这种简单形状上
//! 两者视觉差异很小，开销却低一个量级（只是几次描边光栅化）。
//! 将来若要更柔和的发光，再换 `Shadow` 也不影响对外接口。
//!
//! ## 笔刷缓存
//!
//! 渐变笔刷只在**配色或透明度变化**时重建；窗口尺寸变化只更新渐变端点
//! （`SetStartPoint` / `SetEndPoint`，极便宜）。这样拖边框时不会反复创建资源。

use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F, D2D1_GRADIENT_STOP};
use windows::Win32::Graphics::Direct2D::*;
use windows_numerics::Vector2;

use crate::config::theme::Scheme;
use crate::error::{Result, Win32Ext};

use super::{rounded, to_d2d};

/// 发光层数。层数越多越柔和，代价是更多次描边。
const GLOW_RINGS: i32 = 6;

/// 单层发光的最大不透明度。太高会显出一圈圈的"洋葱环"。
const GLOW_PEAK_ALPHA: f32 = 0.30;

/// 背景网格纹理的间距（DIP）。
///
/// **固定值**，刻意不跟方格尺寸走：如果它跟着方格排布，就重新变成了
/// "对齐参考线"，那正是刚被去掉的东西。固定间距读起来才像底纹。
const TEXTURE_PITCH: f32 = 26.0;
/// 纹理线宽
const TEXTURE_WIDTH: f32 = 1.0;
/// 纹理不透明度（0~255）。
///
/// 压得很低是刻意的：纹理只该在"仔细看"时被察觉。
/// 强到一眼看到就变成干扰，会和方格抢注意力。
const TEXTURE_ALPHA: u8 = 10;

pub struct PanelPainter {
    panel_brush: Option<ID2D1LinearGradientBrush>,
    edge_brush: Option<ID2D1LinearGradientBrush>,
    /// 背景纹理用的实色笔刷。单独一支，免得和渐变笔刷互相污染颜色。
    texture_brush: Option<ID2D1SolidColorBrush>,
    /// 笔刷缓存键：**用来构造那几支笔刷的实际颜色** + 量化后的透明度。
    ///
    /// 原先是 `(scheme.name, 透明度)` —— 那是错的依据：改颜色但名字不变时，
    /// 缓存不失效，面板会继续用旧渐变，表现为"改了配色只有部分界面跟着变"。
    /// 这类 bug 不报错，只能靠肉眼发现，而且很容易被误判成"某处颜色写错了"。
    ///
    /// 现在直接比颜色：只要真正参与绘制的颜色变了就一定重建，
    /// 与方案叫什么名字无关。透明度仍单独量化，避免滑杆连续变化时反复重建。
    key: Option<PanelKey>,
}

/// 面板笔刷的缓存键。字段就是参与构造笔刷的那些颜色。
#[derive(PartialEq, Eq)]
struct PanelKey {
    panel_from: u32,
    panel_to: u32,
    border_from: u32,
    border_to: u32,
    texture: u32,
    opacity: u32,
}

impl PanelKey {
    fn of(scheme: &Scheme, opacity: f32) -> Self {
        let panel = scheme.panel_with_opacity(opacity);
        Self {
            panel_from: panel.from.to_abgr(),
            panel_to: panel.to.to_abgr(),
            border_from: scheme.border.from.to_abgr(),
            border_to: scheme.border.to.to_abgr(),
            // 纹理色带 alpha，`to_abgr` 会把它一起编码进去
            texture: scheme.text_primary.with_alpha(TEXTURE_ALPHA).to_abgr(),
            opacity: (opacity.clamp(0.0, 1.0) * 255.0).round() as u32,
        }
    }
}

impl PanelPainter {
    pub fn new() -> Self {
        Self {
            panel_brush: None,
            edge_brush: None,
            texture_brush: None,
            key: None,
        }
    }

    /// 按当前配色与尺寸刷新笔刷与渐变端点。每帧绘制前调用一次。
    pub fn prepare(
        &mut self,
        ctx: &ID2D1DeviceContext,
        rect: (f32, f32, f32, f32),
        scheme: &Scheme,
        opacity: f32,
    ) -> Result<()> {
        let key = PanelKey::of(scheme, opacity);

        if self.key.as_ref() != Some(&key) {
            let panel = scheme.panel_with_opacity(opacity);
            self.panel_brush = Some(make_linear_brush(
                ctx,
                to_d2d(panel.from),
                to_d2d(panel.to),
            )?);
            self.edge_brush = Some(make_linear_brush(
                ctx,
                to_d2d(scheme.border.from),
                to_d2d(scheme.border.to),
            )?);

            // 背景纹理的实色笔刷。颜色取自正文色 —— 它是这套配色里
            // "最接近中性、又能和底色拉开一点差"的颜色，比纯白/纯黑更贴合主题。
            let texture_color = to_d2d(scheme.text_primary.with_alpha(TEXTURE_ALPHA));
            // SAFETY: COM 调用；颜色是栈上局部量。
            self.texture_brush = Some(
                unsafe { ctx.CreateSolidColorBrush(&texture_color, None) }
                    .ctx("CreateSolidColorBrush(面板纹理)")?,
            );

            self.key = Some(key);
        }

        // 端点跟着矩形走。纯属性写入，不重建资源。
        let (start, end) = scheme.panel_with_opacity(opacity).endpoints(rect);
        let (edge_start, edge_end) = scheme.border.endpoints(rect);

        // SAFETY: COM 调用；参数是值类型，接口由 self 持有。
        unsafe {
            if let Some(brush) = &self.panel_brush {
                brush.SetStartPoint(Vector2 {
                    X: start.0,
                    Y: start.1,
                });
                brush.SetEndPoint(Vector2 { X: end.0, Y: end.1 });
            }
            if let Some(brush) = &self.edge_brush {
                brush.SetStartPoint(Vector2 {
                    X: edge_start.0,
                    Y: edge_start.1,
                });
                brush.SetEndPoint(Vector2 {
                    X: edge_end.0,
                    Y: edge_end.1,
                });
            }
        }

        Ok(())
    }

    /// 绘制整个面板：先发光，再底色，最后描边（顺序决定图层关系）。
    pub fn draw(
        &self,
        ctx: &ID2D1DeviceContext,
        rect: (f32, f32, f32, f32),
        radius: f32,
        scheme: &Scheme,
    ) -> Result<()> {
        let (left, top, right, bottom) = rect;
        let base = rounded(rect, radius);
        let no_style: Option<&ID2D1StrokeStyle> = None;

        // SAFETY: COM 调用；所有结构体都是本函数栈上的局部量，活到调用结束。
        unsafe {
            // 1) 外发光：由外向内画若干圈
            if scheme.glow_strength > 0.0 && scheme.glow_radius > 0.0 {
                if let Some(brush) = &self.edge_brush {
                    for ring in (0..GLOW_RINGS).rev() {
                        // step 越大越靠外
                        let step = ring as f32 / GLOW_RINGS as f32;
                        let inflate = scheme.glow_radius * step;
                        // 平方衰减比线性更接近真实发光
                        let alpha = scheme.glow_strength * (1.0 - step).powi(2) * GLOW_PEAK_ALPHA;
                        if alpha <= 0.004 {
                            continue;
                        }

                        brush.SetOpacity(alpha);
                        let outer = rounded(
                            (
                                left - inflate,
                                top - inflate,
                                right + inflate,
                                bottom + inflate,
                            ),
                            radius + inflate,
                        );
                        ctx.DrawRoundedRectangle(&outer, brush, 1.0, no_style);
                    }
                    // 复位，否则下面的描边会继承最后一次的透明度
                    brush.SetOpacity(1.0);
                }
            }

            // 2) 面板底色
            if let Some(brush) = &self.panel_brush {
                ctx.FillRoundedRectangle(&base, brush);
            }

            // 2.5) 背景网格纹理
            //
            // 目的只是"让大片底色不那么单调"，所以强度必须很低 ——
            // 强到能被注意到就变成了干扰，和方格抢注意力。
            //
            // 裁剪范围取「面板内缩一个圆角半径」的矩形：这个矩形**必定完整落在
            // 圆角形状之内**（圆心到矩形角点的距离为 0），所以不需要图层遮罩
            // 也不会有任何纹理溢出到圆角外面。代价是四周留出一圈无纹理的边距，
            // 而这一圈看起来像是刻意的留白，反而更整齐。
            //
            // 纹理间距是**固定值**，不跟着方格排布走 —— 否则会重新变成
            // "对齐参考线"，那正是刚被去掉的东西。
            if let Some(brush) = &self.texture_brush {
                let clip = D2D_RECT_F {
                    left: left + radius,
                    top: top + radius,
                    right: right - radius,
                    bottom: bottom - radius,
                };
                if clip.right > clip.left && clip.bottom > clip.top {
                    ctx.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);

                    // 起点向外对齐到纹理栅格，这样纹理不随窗口左上角移动而"游走"
                    let start_x = (clip.left / TEXTURE_PITCH).floor() * TEXTURE_PITCH;
                    let start_y = (clip.top / TEXTURE_PITCH).floor() * TEXTURE_PITCH;

                    let mut x = start_x;
                    while x <= clip.right {
                        ctx.DrawLine(
                            Vector2 { X: x, Y: clip.top },
                            Vector2 {
                                X: x,
                                Y: clip.bottom,
                            },
                            brush,
                            TEXTURE_WIDTH,
                            no_style,
                        );
                        x += TEXTURE_PITCH;
                    }
                    let mut y = start_y;
                    while y <= clip.bottom {
                        ctx.DrawLine(
                            Vector2 { X: clip.left, Y: y },
                            Vector2 {
                                X: clip.right,
                                Y: y,
                            },
                            brush,
                            TEXTURE_WIDTH,
                            no_style,
                        );
                        y += TEXTURE_PITCH;
                    }

                    ctx.PopAxisAlignedClip();
                }
            }

            // 3) 双色描边
            //
            // 描边矩形要**向内缩半个线宽**：D2D 的描边是以路径为中心向两侧各画一半，
            // 直接画在面板边界上会有一半溢出到窗口外被裁掉。
            // 玻璃关闭时面板铺满窗口，这个问题尤其明显 —— 描边会细一半。
            if scheme.border_width > 0.0 {
                if let Some(brush) = &self.edge_brush {
                    // 内缩半个线宽、半径同缩同一量：公式在 `inset_stroke_rect` /
                    // `inset_stroke_radius`，与弹窗共用同一处（原先两处各写一份，
                    // 弹窗那份写错了元组语义，见 `mod.rs` 里那两个函数的说明）。
                    let stroke_rect = rounded(
                        crate::render::painters::inset_stroke_rect(rect, scheme.border_width),
                        crate::render::painters::inset_stroke_radius(radius, scheme.border_width),
                    );
                    ctx.DrawRoundedRectangle(&stroke_rect, brush, scheme.border_width, no_style);
                }
            }
        }

        Ok(())
    }
}

impl Default for PanelPainter {
    fn default() -> Self {
        Self::new()
    }
}

/// 构造一个在两端之间插值的线性渐变笔刷。
///
/// 起止点先给占位值，真正的端点在 [`PanelPainter::prepare`] 里按矩形设置。
fn make_linear_brush(
    ctx: &ID2D1DeviceContext,
    from: D2D1_COLOR_F,
    to: D2D1_COLOR_F,
) -> Result<ID2D1LinearGradientBrush> {
    let stops = [
        D2D1_GRADIENT_STOP {
            position: 0.0,
            color: from,
        },
        D2D1_GRADIENT_STOP {
            position: 1.0,
            color: to,
        },
    ];

    let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
        startPoint: Vector2 { X: 0.0, Y: 0.0 },
        endPoint: Vector2 { X: 0.0, Y: 1.0 },
    };

    // SAFETY: COM 调用；stops 与 props 都在本函数栈上。
    unsafe {
        // 用 ID2D1DeviceContext 自带的 6 参版本（返回 GradientStopCollection1）。
        // 注意基接口 ID2D1RenderTarget 上有同名的 3 参版本，别混用。
        //
        // 色彩空间必须给 **SRGB**，不能顺手填 `D2D1_COLOR_SPACE_CUSTOM`：
        // CUSTOM 的语义是"色彩空间由自定义 color context 决定"，
        // 而我们并没有传 color context，D2D 会直接返回 E_INVALIDARG
        // （0x80070057）——这个错误只在真正调用时才会出现，编译期完全看不出来。
        let collection = ctx
            .CreateGradientStopCollection(
                &stops,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_COLOR_SPACE_SRGB,
                D2D1_BUFFER_PRECISION_8BPC_UNORM,
                D2D1_EXTEND_MODE_CLAMP,
                D2D1_COLOR_INTERPOLATION_MODE_STRAIGHT,
            )
            .ctx("CreateGradientStopCollection")?;

        ctx.CreateLinearGradientBrush(&props, None, &collection)
            .ctx("CreateLinearGradientBrush")
    }
}
