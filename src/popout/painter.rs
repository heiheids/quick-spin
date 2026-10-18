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
