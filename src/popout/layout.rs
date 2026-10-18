//! 弹出子窗口的布局：把窗口尺寸换算成各区域矩形，单位是**逻辑像素（DIP）**。
//!
//! ## 为什么不复用 `ui::layout`
//!
//! 那份布局里耦合了侧栏、滚动条、搜索框、设置面板、色板 —— 弹出子窗口
//! 一个都没有。硬塞进去会让它变成"两个窗口的并集"：改主窗口的任何一处
//! 都要回头看弹出窗口会不会被带坏。所以这里重新算一份，只有需要的几块。
//!
//! ## 与主窗口一致的约定
//!
//! * 只在尺寸 / DPI / 图标尺寸 / 条目数变化时重算一次并缓存，
//!   命中测试与绘制都读缓存 —— 命中测试在鼠标移动时高频触发，
//!   里面绝不能现场重算布局。
//! * 全部用 DIP：D2D 的 DPI 已设成窗口 DPI，这一层不必关心物理像素。

use crate::platform::hittest::RectI;

/// 标题栏高度。比主窗口（44）矮一档：这里只有 4 个按钮、也没有侧栏。
pub const TITLE_BAR: i32 = 38;
/// 标题栏按钮边长。
pub const BUTTON: i32 = 28;
/// 标题栏按钮之间的间距。
pub const BUTTON_GAP: i32 = 4;
/// 按钮组距窗口右边缘。
pub const BUTTON_MARGIN: i32 = 8;
/// 左上角标题距窗口左边缘。
pub const TITLE_MARGIN: i32 = 10;
/// 内容区四周留白。
pub const CONTENT_PAD: i32 = 10;
/// 方格之间的间距。
pub const TILE_GAP: i32 = 8;
/// 内容区顶部（标题栏之下）与网格之间的额外留白。
pub const TILES_TOP_GAP: i32 = 6;
/// 窗口缩放边框宽度。与主窗口一致。
///
/// 下一步（`window.rs`）做命中测试时会用到：拖动这个宽度的边缘 = 改大小。
/// 现在还没有调用方，所以先显式允许死代码 —— 删掉再回加反而更容易漏。
#[allow(dead_code)]
pub const RESIZE_BORDER: i32 = 6;

/// 标题栏上的按钮。**枚举顺序即左→右的显示顺序**，
/// 下标直接当命中结果用（见 [`PopoutLayout::button_at`]），所以不要随意调整。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleButton {
    /// 改色（彩色四叶草图案）
    Color,
    /// 吸附（磁铁标志）
    Dock,
    /// 定在桌面（工字钉图案）
    Pin,
    /// 关闭（叉子标志）
    Close,
}

/// 四个按钮，按显示顺序。绘制与命中都遍历它，不各写一份顺序。
pub const TITLE_BUTTONS: [TitleButton; 4] = [
    TitleButton::Color,
    TitleButton::Dock,
    TitleButton::Pin,
    TitleButton::Close,
];

/// 一次算好的布局。
#[derive(Debug, Clone)]
pub struct PopoutLayout {
    /// 窗口客户区尺寸（DIP）
    pub width: i32,
    pub height: i32,
    /// 窗口整体（画面板背景与描边用）
    pub panel: RectI,
    /// 标题栏区域
    pub title_bar: RectI,
    /// 标题文字的排版区域
    pub title_text: RectI,
    /// 四个按钮，下标与 [`TITLE_BUTTONS`] 对应
    pub buttons: [RectI; 4],
    /// 内容区（已扣掉四周留白），图标网格在其中排布
    pub content: RectI,
    /// 网格第一格的左上角
    pub grid_origin: (i32, i32),
    /// 方格边长（等于当前图标尺寸）
    pub tile: i32,
    /// 列数（至少 1）
    pub columns: usize,
}

impl PopoutLayout {
    /// 按客户区尺寸与图标边长算一次布局。
    ///
    /// `icon_size` 是配置里的图标逻辑边长。
    pub fn compute(width: i32, height: i32, icon_size: i32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let panel = RectI::new(0, 0, width, height);

        let title_bar = RectI::new(0, 0, width, TITLE_BAR.min(height));

        // ---- 右上角四个按钮 ----
        //
        // 从右往左排：**关闭永远贴在最右**，这样即使以后增删按钮，
        // 关闭的位置也不变 —— 它是点得最多、也最不能点错的一个。
        let group_w = BUTTON * 4 + BUTTON_GAP * 3;
        let btn_y = (TITLE_BAR - BUTTON) / 2;
        let mut btn_x = (width - BUTTON_MARGIN - group_w).max(TITLE_MARGIN);
        let mut buttons = [RectI::new(0, 0, 0, 0); 4];
        for i in 0..4 {
            buttons[i] = RectI::new(btn_x, btn_y, BUTTON, BUTTON);
            btn_x += BUTTON + BUTTON_GAP;
        }
        // 宽度不够时按钮组会压到标题上，这是可接受的退化：
        // 窗口最小宽度（见 state::MIN_W）已经保证留得下标题的余地。

        let title_text = RectI::new(
            TITLE_MARGIN,
            0,
            (buttons[0].x - TITLE_MARGIN - BUTTON_GAP).max(0),
            TITLE_BAR,
        );

        // ---- 内容区 ----
        let content_y = TITLE_BAR;
        let content = RectI::new(
            CONTENT_PAD,
            content_y + CONTENT_PAD,
            (width - CONTENT_PAD * 2).max(0),
            (height - content_y - CONTENT_PAD * 2).max(0),
        );

        // ---- 网格 ----
        // 方格边长取配置值，但不超过内容区宽度，否则单列都放不下。
        let tile = icon_size.clamp(16, content.w.max(16));
        let columns = if content.w >= tile {
            ((content.w + TILE_GAP) / (tile + TILE_GAP)).max(1) as usize
        } else {
            1
        };
        let grid_origin = (content.x, content.y + TILES_TOP_GAP);

        Self {
            width,
            height,
            panel,
            title_bar,
            title_text,
            buttons,
            content,
            grid_origin,
            tile,
            columns,
        }
    }

    /// 第 `index` 个方格的位置（逻辑像素，左上角）。
    pub fn tile_at(&self, index: usize, row_height: i32) -> (i32, i32) {
        let c = (index % self.columns) as i32;
        let r = (index / self.columns) as i32;
        (
            self.grid_origin.0 + c * (self.tile + TILE_GAP),
            self.grid_origin.1 + r * (row_height + TILE_GAP),
        )
    }

    /// 命中哪个标题栏按钮。
    pub fn button_at(&self, lx: i32, ly: i32) -> Option<TitleButton> {
        // 按 `TITLE_BUTTONS` 的顺序找，所以下标与枚举一一对应。
        // 按钮之间不重叠（间距 4），不会出现同时命中两个的歧义。
        self.buttons
            .iter()
            .position(|r| r.contains(lx, ly))
            .map(|i| TITLE_BUTTONS[i])
    }

    /// 整行图标占的高度（方格 + 名称文字）。名称行固定 16 DIP。
    pub const fn row_height(&self) -> i32 {
        self.tile + 16
    }
}
