//! 选色浮层：几何、命中判定，以及"这个窗口该用哪套配色"的**唯一规则**。
//!
//! ## 为什么单独一个模块
//!
//! 与 `dock.rs` 同一个思路：**纯算术**，没有窗口、没有 HWND，因此能被
//! `--selftest` 完全覆盖。这一点在这里尤其重要 —— `docs/ARCHITECTURE.md`
//! §17.8 记着：自检建不出第二个窗口，所以"选色浮层摆在哪、点到了哪一格"
//! 这类判据**只能**靠离线自检钉住，不能留给人工去撞。
//!
//! ## 色板从哪来
//!
//! **不在这里定义颜色**。候选色只有一处（`config::theme::BASE_PALETTE`，
//! 经 `theme::palette_colors()` 解析），主窗口设置面板与这里都引用它。
//! 这个模块只管"那些格子摆在哪"。
//!
//! 下面那几个尺寸常量是**本窗口自己的布局度量**，刻意不与主窗口的
//! `ui::layout` 共用（两个窗口的布局本来就是两套，见 `mod.rs` 的模块边界）。
//! 数值取成一样，是为了让两处选色看起来是同一个东西。

use crate::config::Color;
use crate::config::theme::{BASE_PALETTE_COLS, palette_colors};
use crate::platform::hittest::RectI;

/// 浮层内边距。
const PAD: i32 = 10;
/// 格子间距。与主窗口设置面板的 `SWATCH_GAP` 同值（见模块说明）。
const GAP: i32 = 5;
/// 标题行高度。
const TITLE_H: i32 = 18;
/// "跟随主窗口"那一行的高度。
const FOLLOW_H: i32 = 22;
/// 色块边长。与主窗口的 `SWATCH_SIZE` 同值。
const CELL: i32 = 20;
/// 窗口很窄时色块可以缩到的最小边长。
const CELL_MIN: i32 = 12;

/// 选色浮层上的两处文字。
///
/// 由装配层从 i18n 取好传进来 —— 窗口层不该知道语言文件的形状
///（与 `report`、`diag` 同一条依赖方向的理由）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaletteText {
    pub title: String,
    /// "跟随主窗口"那一格上的文字。
    pub follow_main: String,
}

/// 选色浮层摆在哪。
///
/// 每次都按当前内容区重算：它要跟着窗口尺寸走（拖动边框时窗口会重算布局）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteLayout {
    /// 整个浮层。点它外面 = 关闭。
    pub panel: RectI,
    /// 标题行。
    pub title: RectI,
    /// "跟随主窗口"那一行。
    pub follow: RectI,
    /// 每个色块，与 [`palette_colors`] **逐项对应**。
    pub swatches: Vec<RectI>,
}

impl PaletteLayout {
    /// 在 `area`（内容区，DIP）里摆一次。
    pub fn compute(area: RectI) -> Self {
        let count = palette_colors().len().max(1);
        let cols = BASE_PALETTE_COLS.max(1);
        let rows = count.div_ceil(cols).max(1) as i32;
        let cols_i = cols as i32;

        // 先看宽度能不能放下标准格子；窄窗口按比例缩，但不小于 `CELL_MIN`
        //（再小就点不准了，宁可让浮层溢出内容区、由绘制层裁掉）。
        let by_width = (area.w - 2 * PAD - (cols_i - 1) * GAP) / cols_i;
        let cell = by_width.clamp(CELL_MIN, CELL);

        let grid_w = cols_i * cell + (cols_i - 1) * GAP;
        let grid_h = rows * cell + (rows - 1) * GAP;
        let panel_w = grid_w + 2 * PAD;
        let panel_h = TITLE_H + GAP + FOLLOW_H + GAP + grid_h + 2 * PAD;

        // 居中；放不下时贴左上并让绘制层裁掉。
        //
        // **不在放不下时缩小浮层**：唯一能缩的是格子边长，而它已经到底了。
        // 那种情况只出现在"把窗口拖到最小"时，默认尺寸（320×420）下是放得下的。
        let px = area.x + ((area.w - panel_w) / 2).max(0);
        let py = area.y + ((area.h - panel_h) / 2).max(0);

        let title = RectI::new(px + PAD, py + PAD, grid_w, TITLE_H);
        let follow = RectI::new(px + PAD, title.y + TITLE_H + GAP, grid_w, FOLLOW_H);
        let grid_top = follow.y + FOLLOW_H + GAP;

        let mut swatches = Vec::with_capacity(count);
        for i in 0..count {
            let (c, r) = (i % cols, i / cols);
            swatches.push(RectI::new(
                px + PAD + c as i32 * (cell + GAP),
                grid_top + r as i32 * (cell + GAP),
                cell,
                cell,
            ));
        }

        Self {
            panel: RectI::new(px, py, panel_w, panel_h),
            title,
            follow,
            swatches,
        }
    }

    /// 点是否落在浮层内（用来判断"点别处 = 关闭"）。
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.panel.contains(x, y)
    }

    /// 点到了第几个色块（与 [`palette_colors`] 的下标一一对应）。
    pub fn swatch_at(&self, x: i32, y: i32) -> Option<usize> {
        self.swatches.iter().position(|r| r.contains(x, y))
    }

    /// 点到了"跟随主窗口"那一行吗。
    pub fn is_follow(&self, x: i32, y: i32) -> bool {
        self.follow.contains(x, y)
    }
}

/// 一个弹窗该用哪套配色。///
/// **这是 `PopoutWindow::base_color` 语义（空串 = 跟随主窗口）的唯一实现处**：
/// 创建时、主窗口换色时、用户在这次浮层里选完色时，都走它。
/// 三个地方各写一次判断，迟早会出现"选了色但某一条路没跟上"。
///
/// 解析不出来的值也回落到主窗口配色 —— 状态文件是用户可手改的，
/// 填了个坏值不该让窗口没有配色。
pub fn resolve_scheme(base_color: &str, main: &crate::config::Scheme) -> crate::config::Scheme {
    match Color::from_hex(base_color.trim()) {
        Some(c) => crate::config::Scheme::from_base(c),
        None => main.clone(),
    }
}

/// 这个基础色是不是"跟随主窗口"。
///
/// 单独一个函数而不是各处写 `is_empty()`：`base_color` 是字符串，
/// 手改后可能带空白，判据必须一致。
pub fn follows_main(base_color: &str) -> bool {
    Color::from_hex(base_color.trim()).is_none()
}

/// 绘制选色浮层要的全部输入。
///
/// 收成一个结构体而不是继续给绘制函数加参数：那个函数本来参数就多，
/// 而这些都是"这个浮层长什么样"的一部分。
pub struct PaletteView<'a> {
    pub layout: &'a PaletteLayout,
    pub labels: &'a PaletteText,
    /// 这个窗口当前用的配色 —— 浮层自己也是用它画的，所以选色时
    /// 整个窗口（含浮层）会跟着变色，所见即所得。
    pub scheme: &'a crate::config::Scheme,
    /// 主窗口的配色：用来把"跟随主窗口"那一行画成主窗口的样子。
    pub main_scheme: &'a crate::config::Scheme,
    /// 当前选中的色块下标（`None` = 跟随主窗口，或一个不在色板里的自定义色）。
    pub selected: Option<usize>,
    pub hover_swatch: Option<usize>,
    pub hover_follow: bool,
}

