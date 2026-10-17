//! 内容区网格：淡网格线 + 方格（图标占位 + 名称）。
//!
//! 字符区（`labels`）由调用方按当前选项卡与搜索词过滤好后传进来，
//! 本模块不认识快捷方式结构 —— 它只负责把给定的若干项摆到给定矩形里。
//! 这样过滤逻辑变了不必动绘制代码。

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::*;
use windows_numerics::Vector2;

use crate::config::theme::Scheme;
use crate::error::{Result, Win32Ext};
use crate::render::{Role, TextEngine};
use crate::ui::layout::{LABEL_GAP, LABEL_HEIGHT, Layout, TILE_PADDING};
use super::{rounded, rounded_i, to_d2d};

/// 方格圆角
const TILE_RADIUS: f32 = 8.0;
/// 滚动条宽度
const SCROLLBAR_WIDTH: f32 = 4.0;
/// 滚动条距内容区右边缘
const SCROLLBAR_RIGHT_MARGIN: f32 = 4.0;
/// 滚动条最小长度：内容极长时也要保证能看见、能拖
const SCROLLBAR_MIN_LEN: f32 = 24.0;
/// 图标方块比图标本身上下各留出的余量，由布局决定（方格边长 = 图标 + 内边距 + 名称区）。
/// 这里不再单独缩放图标 —— 位图按原始尺寸 1:1 绘制才清晰。
///
/// 进入作用域时裁剪已经压栈，离开时负责出栈。
///
/// 存在的唯一理由是"提前返回也要配对"：D2D 的裁剪是**状态机**，
/// 压了不出会让后续所有绘制都受限，而且不会报任何错误。
struct ClipGuard<'a>(&'a ID2D1DeviceContext);

impl Drop for ClipGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: 与创建它时的 PushAxisAlignedClip 严格配对（同一上下文、同一次调用栈）。
        unsafe {
            self.0.PopAxisAlignedClip();
        }
    }
}

pub struct GridPainter {
    brush: Option<ID2D1SolidColorBrush>,
}

impl GridPainter {
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

    /// 画内容区。
    ///
    /// `labels` 与 `layout.tiles` 一一对应（即条目 `first_item..first_item+len`）。
    /// `icons` 同样一一对应，`None` 表示该图标还没加载好，画占位底色。
    /// `hover` 是**条目下标**，不是 `tiles` 下标，所以要和 `first_item` 相加后再比。
    ///
    /// `selected` 与 `layout.tiles` **逐项对齐**（`selected[i]` 管 `tiles[i]`）。
    /// 它和 `hover` 的下标口径不同是刻意的：选中集是按覆盖表键维护的，
    /// 由调用方按键算好再传进来，绘制层不必知道"键"这种概念。
    /// 长度为 0 或偏短时一律当"没选中"处理（`get` + 默认值），
    /// 免得某一帧的切片错位把未选中的格子画成选中。
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        ctx: &ID2D1DeviceContext,
        text: &mut TextEngine,
        layout: &Layout,
        scheme: &Scheme,
        labels: &[&str],
        icons: &[Option<ID2D1Bitmap1>],
        hover: Option<usize>,
        selected: &[bool],
        opacity: f32,
    ) -> Result<()> {
        let Some(brush) = &self.brush else {
            return Ok(());
        };
        let no_style: Option<&ID2D1StrokeStyle> = None;

        // 把整块内容裁剪到内容区。
        //
        // 这是**必须**的：滚动时第一行方格会上移，上半部分就画到了标题栏上，
        // 和标题文字、最小化/关闭按钮叠在一起 —— 看起来像窗口坏了。
        // 之前只给名称文字加了越界判断，方格底色和图标照样会画出去。
        // 一处裁剪统一解决三者，比拼三处判断可靠。
        let content_clip = D2D_RECT_F {
            left: layout.content.x as f32,
            top: layout.content.y as f32,
            right: (layout.content.x + layout.content.w) as f32,
            bottom: (layout.content.y + layout.content.h) as f32,
        };
        // SAFETY: COM 调用；裁剪结构体是栈上局部量。
        unsafe {
            ctx.PushAxisAlignedClip(&content_clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }
        // 用 RAII 保证 Pop 一定执行。
        //
        // **不能**在函数末尾手写 Pop：中间的绘制用了 `?`，一旦提前返回就会漏掉 Pop，
        // 裁剪栈失衡 —— 后果是之后所有绘制都被上一帧的裁剪矩形限制住，
        // 表现为"界面某块区域再也画不出东西"，而且极难定位。
        let _clip_guard = ClipGuard(ctx);

        // SAFETY: COM 调用；结构体都是本函数栈上的局部量。
        unsafe {
            // ---- 这里原本画一层网格线 ----
            //
            // 已移除。原因：方格本身已经有清晰的边界，再叠一层线是重复信息，
            // 而且在缩放后（方格变小）线与方格会挤在一起显得脏。
            // "背景不单调"改用面板底色上的**纹理**实现（见 `painters::panel`），
            // 纹理整体铺满、不随方格排布变化，视觉上更安静也更统一。

            // ---- 滚动条 ----
            // 只在内容超出一屏时出现。没有它的话，"还有 134 条在下面"这件事
            // 在界面上完全不可见，用户会以为程序漏了数据。
            //
            // 轨道上下端与**内容区**齐平（上端即标题栏下沿，下端即窗口底边），
            // 不跟着方格的内边距缩进 —— 滚动条代表的是整个可视区域的滚动进度，
            // 不是某一行的装饰，缩进去反而会显得没对齐。
            if layout.max_scroll > 0.0 && layout.total_rows > 0 {
                let track_top = layout.content.y as f32;
                let track_height = (layout.content.h as f32).max(1.0);
                let visible_ratio =
                    layout.grid_rows as f32 / layout.total_rows as f32;

                let thumb_len = (track_height * visible_ratio)
                    .max(SCROLLBAR_MIN_LEN)
                    .min(track_height);
                let travel = (track_height - thumb_len).max(0.0);
                let progress = if layout.max_scroll > 0.0 {
                    layout.scroll / layout.max_scroll
                } else {
                    0.0
                };
                let thumb_y = track_top + travel * progress;

                let x = (layout.content.x + layout.content.w) as f32
                    - SCROLLBAR_RIGHT_MARGIN
                    - SCROLLBAR_WIDTH;

                brush.SetColor(&to_d2d(
                    scheme
                        .text_secondary
                        .with_alpha(((scheme.text_secondary.a() as f32) * 0.5 * opacity) as u8),
                ));
                let thumb = crate::platform::hittest::RectI::new(
                    x as i32,
                    thumb_y as i32,
                    SCROLLBAR_WIDTH as i32,
                    thumb_len as i32,
                );
                ctx.FillRoundedRectangle(&rounded_i(thumb, SCROLLBAR_WIDTH / 2.0), brush);
            }

            // ---- 方格 ----
            for (i, rect) in layout.tiles.iter().enumerate() {
                let is_hover = hover == Some(layout.first_item + i);
                // 选中优先于悬停：鼠标正停在已选中的格子上时两种状态同时为真，
                // 若让悬停赢，用户会看到"刚选中的格子变回了普通悬停"，
                // 以为 Ctrl 那一下没生效。
                let is_selected = selected.get(i).copied().unwrap_or(false);
                let frame = rounded_i(*rect, TILE_RADIUS);

                let fill = if is_selected {
                    // 用派生色 `tile_active`（方格底色往强调色混 55%），
                    // 而不是自己在这里现算一个混色 —— 混色规则只该有一处定义，
                    // 否则换主题时选中底色会和选项卡选中框对不上。
                    scheme.tile_active
                } else if is_hover {
                    scheme.tile_hover
                } else {
                    scheme.tile_bg
                };
                brush.SetColor(&to_d2d(
                    fill.with_alpha((fill.a() as f32 * opacity).round() as u8),
                ));
                ctx.FillRoundedRectangle(&frame, brush);

                if is_selected {
                    // 选中态：强调色 2px 描边。
                    //
                    // 比悬停的 1.4px 更粗是必须的 —— 两种状态会同时出现
                    //（鼠标停在已选中的格子上），只靠颜色区分在高分屏上
                    // 几乎看不出来，粗细是更可靠的第二信号。
                    brush.SetColor(&to_d2d(scheme.accent));
                    ctx.DrawRoundedRectangle(&frame, brush, 2.0, no_style);
                } else if is_hover {
                    // 悬停时加一圈强调色描边，与设计稿的"选中发光"呼应
                    brush.SetColor(&to_d2d(scheme.tab_active_border));
                    ctx.DrawRoundedRectangle(&frame, brush, 1.4, no_style);
                }

                // ---- 图标 ----
                //
                // 图标背后原本有一块小圆角底色。已按需求去掉：
                // 方格本身已经是"底框"，图标再套一层框是重复的两层容器，
                // 视觉上显得琐碎，也让图标显得比实际小。
                //
                // 图标位置：方格内边距固定 `TILE_PADDING`，水平居中。
                // 尺寸直接用布局给的 `icon_size`，与位图 1:1 —— 放大就会发虚。
                if let Some(bitmap) = icons.get(i).and_then(|b| b.as_ref()) {
                    let size = layout.icon_size;
                    let cx = rect.x as f32 + rect.w as f32 / 2.0;
                    let dest = D2D_RECT_F {
                        left: cx - size / 2.0,
                        top: rect.y as f32 + TILE_PADDING,
                        right: cx + size / 2.0,
                        bottom: rect.y as f32 + TILE_PADDING + size,
                    };
                    // 线性插值：位图尺寸和绘制尺寸一致（1:1）时它等价于直接拷贝，
                    // 只有在 DPI 取整有偏差时才真正做插值，比最近邻平滑。
                    // 注意：这个重载（ID2D1RenderTarget 上的）返回 `()` 而不是 Result
                    ctx.DrawBitmap(
                        bitmap,
                        Some(&dest),
                        opacity,
                        D2D1_INTERPOLATION_MODE_LINEAR,
                        None,
                        None,
                    );
                }
            }
        }

        // ---- 名称文字 ----
        for (i, rect) in layout.tiles.iter().enumerate() {
            let Some(label) = labels.get(i) else {
                continue;
            };
            // 越界判断已由上面的裁剪负责，这里不再单独判断 ——
            // 两处都写容易改一处漏一处，裁剪是唯一权威。
            // 名称在方格**内部**、紧贴图标下方。
            // 宽度取方格去掉左右内边距的部分，高度固定一行。
            let label_rect = (
                (rect.x as f32 + TILE_PADDING),
                rect.y as f32 + TILE_PADDING + layout.icon_size + LABEL_GAP,
                (rect.w as f32 - TILE_PADDING * 2.0).max(1.0),
                LABEL_HEIGHT,
            );
            text.draw(
                ctx,
                label,
                Role::Tile,
                label_rect,
                scheme.text_primary,
            )?;
        }

        // 裁剪由 `_clip_guard` 的 Drop 负责出栈（含提前返回的路径），这里不再手写 Pop。
        Ok(())
    }
}

impl Default for GridPainter {
    fn default() -> Self {
        Self::new()
    }
}

impl GridPainter {
    /// 画就地编辑的文本框。
    ///
    /// 放在这里而不是单开一个绘制器：本类型已经持有一支实色笔刷，
    /// 而编辑框只用到实色填充与文字，复用它可以省掉一个模块和一套笔刷生命周期。
    ///
    /// `caret` 是**字符**下标（不是字节）。所有光标位置换算都在这里完成 ——
    /// 调用方只要按 char 记账就不会切断中文。
    pub fn draw_edit_box(
        &self,
        ctx: &ID2D1DeviceContext,
        text: &mut TextEngine,
        rect: (f32, f32, f32, f32),
        value: &str,
        caret: usize,
        scheme: &Scheme,
    ) -> Result<()> {
        let Some(brush) = &self.brush else {
            return Ok(());
        };
        let no_style: Option<&ID2D1StrokeStyle> = None;
        let (l, t, w, h) = rect;

        // SAFETY: COM 调用；结构体都是本函数栈上的局部量。
        unsafe {
            // 底：完全不透明。编辑框浮在方格与图标之上，透出背后内容会读不清。
            brush.SetColor(&to_d2d(scheme.panel.midpoint().with_alpha(255)));
            let frame = rounded((l, t, l + w, t + h), 6.0);
            ctx.FillRoundedRectangle(&frame, brush);
            // 描边用强调色：一眼看出"这里正在输入"
            brush.SetColor(&to_d2d(scheme.accent));
            ctx.DrawRoundedRectangle(&frame, brush, 1.5, no_style);
        }

        // 光标位置：按字符宽度累加估算。
        //
        // 不用 `IDWriteTextLayout` 精确测量：那要为每次按键重建一个 layout 对象，
        // 而这里只需要"光标大致在哪一列"。估算的代价是中文与英文混排时
        // 光标可能有几个像素的偏差 —— 对输入体验没有实质影响。
        const PAD: f32 = 8.0;
        let text_width: f32 = value
            .chars()
            .take(caret)
            .map(|c| if c.is_ascii() { 7.0 } else { 13.0 })
            .sum();

        text.draw(
            ctx,
            value,
            Role::Body,
            (l + PAD, t, (w - PAD * 2.0).max(1.0), h),
            scheme.text_primary,
        )?;

        // 光标竖线。夹在框内，免得长文本时光标跑到框外看不见。
        let caret_x = (l + PAD + text_width).min(l + w - PAD - 1.0);
        // SAFETY: COM 调用；坐标是栈上局部量。
        unsafe {
            if let Some(brush) = &self.brush {
                brush.SetColor(&to_d2d(scheme.text_primary));
                ctx.DrawLine(
                    Vector2 {
                        X: caret_x,
                        Y: t + 4.0,
                    },
                    Vector2 {
                        X: caret_x,
                        Y: t + h - 4.0,
                    },
                    brush,
                    1.5,
                    no_style,
                );
            }
        }

        Ok(())
    }
}