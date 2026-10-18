//! 弹出子窗口的绘制。
//!
//! ## 为什么把图标外形单独抽出来
//!
//! 标题栏四个图标（四叶草 / 磁铁 / 工字钉 / 叉）如果直接写成一串 D2D 调用，
//! 就没法验证"图标有没有画出按钮外、四个图标是不是长得一样、
//! 点中的和画的是不是同一块地方"。所以先把外形算成**纯数据**（[`Glyph`]），
//! 它可以离线逐项检查；D2D 那层只负责照着画，不做任何几何决策。
//!
//! 原语刻意只有三种：圆环、实心圆、线段。够画这四个图标，
//! 而且都是 `ID2D1RenderTarget` 上最基础的方法（不需要几何路径 /
//! `ID2D1PathGeometry`）。少一种原语就少一类画错的可能。

use crate::platform::hittest::RectI;
use crate::popout::layout::TitleButton;
use crate::popout::layout::{PopoutLayout, TITLE_BUTTONS};

use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::*;

use crate::config::theme::Scheme;
use crate::error::{Result, Win32Ext};
use crate::popout::icons::Item;
use crate::render::{Role, TextEngine};

use crate::render::painters::{rounded_i, to_d2d};

/// 一个绘制原语。坐标是**窗口内的逻辑像素**（不是相对按钮的），
/// 这样绘制层可以直接用，测试层也可以直接判断它在不在按钮里。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Glyph {
    /// 实心圆
    Disc { c: (f32, f32), r: f32 },
    /// 线段
    Line {
        a: (f32, f32),
        b: (f32, f32),
        width: f32,
    },
}

impl Glyph {
    /// 这个原语的外接矩形（含描边宽度）。测试与命中都用它。
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        match *self {
            Glyph::Disc { c, r } => (c.0 - r, c.1 - r, c.0 + r, c.1 + r),
            Glyph::Line { a, b, width } => {
                let o = width / 2.0;
                (
                    a.0.min(b.0) - o,
                    a.1.min(b.1) - o,
                    a.0.max(b.0) + o,
                    a.1.max(b.1) + o,
                )
            }
        }
    }
}

/// 图标在按钮矩形里占的比例。留出边距，免得贴着按钮边缘看起来"顶住了"。
const INSET: f32 = 0.26;

/// 算出某个按钮的图标外形。
///
/// `on` 表示该按钮对应的状态是否**已开启**（吸附/钉桌面是有开关的），
/// 用于让绘制层区分两种观感；外形本身暂时一致 —— 用形状表达开关状态
/// 会让人认不出这是哪个按钮，开关状态交给颜色与底色表达。
pub fn glyph(button: TitleButton, r: RectI, on: bool) -> Vec<Glyph> {
    let _ = on;
    let cx = r.x as f32 + r.w as f32 / 2.0;
    let cy = r.y as f32 + r.h as f32 / 2.0;
    // 以按钮短边为基准算尺寸：按钮是正方形，但取短边更稳。
    let s = (r.w.min(r.h)) as f32 * (1.0 - INSET * 2.0);
    let h = s / 2.0;
    let line_w = (s * 0.14).max(1.2);

    match button {
        // 四叶草：四个交叠的圆。**返回顺序有意义** —— 绘制层按顺序给
        // 四个圆上不同的颜色，这就是"彩色四叶草"的由来。
        TitleButton::Color => {
            let d = h * 0.42;
            let rr = h * 0.46;
            vec![
                Glyph::Disc { c: (cx - d, cy - d), r: rr },
                Glyph::Disc { c: (cx + d, cy - d), r: rr },
                Glyph::Disc { c: (cx - d, cy + d), r: rr },
                Glyph::Disc { c: (cx + d, cy + d), r: rr },
            ]
        }

        // 磁铁：马蹄形。两条腿 + 顶上一条横梁，腿端各一小段"磁极"。
        TitleButton::Dock => {
            let leg = (cx - h * 0.52, cx + h * 0.52);
            let top = cy - h * 0.62;
            let bottom = cy + h * 0.62;
            vec![
                Glyph::Line {
                    a: (leg.0, top),
                    b: (leg.0, bottom),
                    width: line_w,
                },
                Glyph::Line {
                    a: (leg.1, top),
                    b: (leg.1, bottom),
                    width: line_w,
                },
                Glyph::Line {
                    a: (leg.0, top),
                    b: (leg.1, top),
                    width: line_w,
                },
                // 两个磁极：腿的下端各加粗一点，读起来才像磁铁而不是"Π"
                Glyph::Disc {
                    c: (leg.0, bottom),
                    r: line_w * 0.9,
                },
                Glyph::Disc {
                    c: (leg.1, bottom),
                    r: line_w * 0.9,
                },
            ]
        }

        // 工字钉：圆头 + 斜轴 + 底座。
        TitleButton::Pin => {
            let head_r = h * 0.34;
            let head = (cx, cy - h * 0.42);
            let foot = (cx, cy + h * 0.62);
            let base_y = cy + h * 0.62;
            vec![
                Glyph::Disc {
                    c: head,
                    r: head_r,
                },
                Glyph::Line {
                    a: (head.0, head.1 + head_r * 0.6),
                    b: foot,
                    width: line_w * 0.8,
                },
                Glyph::Line {
                    a: (cx - h * 0.42, base_y),
                    b: (cx + h * 0.42, base_y),
                    width: line_w * 0.8,
                },
            ]
        }

        // 关闭：叉。两条对角线，端点对称。
        TitleButton::Close => {
            let e = h * 0.62;
            vec![
                Glyph::Line {
                    a: (cx - e, cy - e),
                    b: (cx + e, cy + e),
                    width: line_w,
                },
                Glyph::Line {
                    a: (cx + e, cy - e),
                    b: (cx - e, cy + e),
                    width: line_w,
                },
            ]
        }
    }
}

/// 四叶草四个圆的颜色（按 [`glyph`] 返回的顺序）。
///
/// 单独一个函数而不是写死在绘制里：它是"彩色"这个需求的具体体现，
/// 也让测试能确认四个圆拿到的是四个**不同**的颜色。
pub fn clover_colors() -> [(u8, u8, u8); 4] {
    [
        (0xF2, 0x5B, 0x5B), // 红
        (0xF2, 0xB5, 0x2E), // 黄
        (0x4C, 0xAF, 0x50), // 绿
        (0x42, 0x8B, 0xF5), // 蓝
    ]
}

/// 弹出子窗口的绘制器。
///
/// 与其他绘制器同样的形状：一把白色笔刷，靠 `SetColor` 改色复用。
/// 每个窗口一份（弹出窗口自给自足，见 `mod.rs` 的选择 A）。
pub struct PopoutPainter {
    brush: Option<ID2D1SolidColorBrush>,
}

impl PopoutPainter {
    pub fn new() -> Self {
        Self { brush: None }
    }

    /// 建/复用自己的笔刷。**每次绘制都要调**：设备丢失重建后旧的笔刷失效。
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

    /// 画一帧：面板底 + 标题栏 + 四个图标 + 内容（方格 + 真图标 + 名称）+ 标题文字。
    ///
    /// `items` 是要显示的条目（与主窗口该选项卡的可见条目同步），
    /// `icons` 是它们对应的位图，缺失的位置是 `None`（还没加载好）。
    pub fn draw(
        &self,
        ctx: &ID2D1DeviceContext,
        text: &mut TextEngine,
        layout: &PopoutLayout,
        scheme: &Scheme,
        title: &str,
        hover: Option<TitleButton>,
        items: &[Item],
        icons: &[Option<ID2D1Bitmap1>],
    ) -> Result<()> {
        let Some(brush) = &self.brush else {
            return Ok(());
        };

        // SAFETY: COM 调用；所有结构体都是本函数栈上的局部量。
        unsafe {
            // ---- 面板底 ----
            brush.SetColor(&to_d2d(scheme.panel.from));
            ctx.FillRoundedRectangle(&rounded_i(layout.panel, 10.0), brush);
            // 同色系描边（方向由面板亮度决定，见 theme::finalize）
            brush.SetColor(&to_d2d(scheme.border.from));
            ctx.DrawRoundedRectangle(&rounded_i(layout.panel, 10.0), brush, 1.2, None);

            // ---- 标题栏底色：取渐变的另一端，让它读起来是"一条栏" ----
            brush.SetColor(&to_d2d(scheme.panel.to));
            ctx.FillRectangle(&rect_to_d2d(layout.title_bar), brush);

            // ---- 内容：方格底色 + 真图标 ----
            //
            // 方格底色有两个作用，所以**总是**画：它既是"整格的框"
            //（与主窗口的方格观感一致），也是图标还没加载好时的占位。
            // 图标到了就叠在上面 —— 不会出现"一会儿有一会儿没有"的闪。
            //
            // 数量与顺序来自主窗口推来的快照，所以两边必须一致 ——
            // 这正是"弹出只是多了一种显示方式"的可见证据。
            let visible = layout.visible_tiles(items.len());
            let row_h = layout.row_height();
            for i in 0..visible {
                let (tx, ty) = layout.tile_at(i, row_h);
                brush.SetColor(&to_d2d(scheme.tile_bg));
                ctx.FillRoundedRectangle(
                    &rounded_i(RectI::new(tx, ty, layout.tile, layout.tile), 6.0),
                    brush,
                );

                if let Some(bitmap) = icons.get(i).and_then(|b| b.as_ref()) {
                    // 目标矩形正好是方格。位图是按**物理像素**取的、绘制用 DIP，
                    // 而 D2D 上下文的 DPI 已设成窗口 DPI，所以这里是 1:1，
                    // 放大缩小时都不会发虚。位置与尺寸都取自布局，
                    // 这里不做任何几何决策 —— 几何错了应该在布局的自检里暴露。
                    let dest = D2D_RECT_F {
                        left: tx as f32,
                        top: ty as f32,
                        right: (tx + layout.tile) as f32,
                        bottom: (ty + layout.tile) as f32,
                    };
                    ctx.DrawBitmap(
                        bitmap,
                        Some(&dest),
                        1.0,
                        D2D1_INTERPOLATION_MODE_LINEAR,
                        None,
                        None,
                    );
                }
            }

            // ---- 四个图标 ----
            let colors = clover_colors();
            for (i, btn) in TITLE_BUTTONS.iter().enumerate() {
                let rect = layout.buttons[i];

                // 悬停底色：与主窗口按钮一致的观感
                if hover == Some(*btn) {
                    brush.SetColor(&to_d2d(scheme.tile_hover));
                    ctx.FillRoundedRectangle(&rounded_i(rect, 6.0), brush);
                }

                let shapes = glyph(*btn, rect, false);
                for (k, s) in shapes.iter().enumerate() {
                    // 四叶草的四个圆各上一色；其余图标统一用文字色。
                    // 直接构造 `D2D1_COLOR_F` 而不是先做一个 `Color`：
                    // 颜色空间转换（sRGB / 预乘）在这一层没有意义，
                    // 而 `Color` 的构造函数是给配置解析用的。
                    if *btn == TitleButton::Color {
                        let (r, g, b) = colors[k.min(colors.len() - 1)];
                        brush.SetColor(&D2D1_COLOR_F {
                            r: r as f32 / 255.0,
                            g: g as f32 / 255.0,
                            b: b as f32 / 255.0,
                            a: 1.0,
                        });
                    } else {
                        brush.SetColor(&to_d2d(scheme.text_primary));
                    }
                    match *s {
                        Glyph::Disc { c, r } => {
                            ctx.FillEllipse(&ellipse(c, r), brush);
                        }
                        Glyph::Line { a, b, width } => {
                            ctx.DrawLine(point(a), point(b), brush, width, None);
                        }
                    }
                }
            }
        }

        // ---- 名称（走文本引擎，需要 &mut）----
        // 与方格分开两轮画：先画完所有方格的底色与图标，再统一排文字，
        // 避免"方块底色盖住上一个名称"。
        //
        // 可见格数用的是**同一个判据**（`visible_tiles`），所以名称与方格
        // 永远一一对应 —— 两处各写一份就会出现"有方格没名字"这种错位。
        let visible = layout.visible_tiles(items.len());
        let row_h = layout.row_height();
        for i in 0..visible {
            let (tx, ty) = layout.tile_at(i, row_h);
            text.draw(
                ctx,
                &items[i].name,
                Role::Caption,
                (
                    tx as f32 - 4.0,
                    (ty + layout.tile) as f32,
                    layout.tile as f32 + 8.0,
                    16.0,
                ),
                scheme.text_primary,
            )?;
        }

        // ---- 标题文字（走文本引擎，需要 &mut）----
        text.draw(
            ctx,
            title,
            Role::Tab,
            layout.title_text.as_f32(),
            scheme.text_primary,
        )?;
        Ok(())
    }
}

fn point(p: (f32, f32)) -> windows_numerics::Vector2 {
    windows_numerics::Vector2 { X: p.0, Y: p.1 }
}

fn ellipse(c: (f32, f32), r: f32) -> D2D1_ELLIPSE {
    D2D1_ELLIPSE {
        point: point(c),
        radiusX: r,
        radiusY: r,
    }
}

fn rect_to_d2d(r: RectI) -> windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
    windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
        left: r.x as f32,
        top: r.y as f32,
        right: (r.x + r.w) as f32,
        bottom: (r.y + r.h) as f32,
    }
}
