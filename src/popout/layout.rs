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

/// 面板描边宽度（DIP）。
///
/// **唯一出处**：布局用它算"描边矩形"，绘制用它当线宽。两处各写一个数字，
/// 将来改了一处就会出现"描边画得出界/画在填充中间"这类只有肉眼能看出的错。
pub const PANEL_STROKE: f32 = 1.2;

/// 面板填充向**右**与**下**多画出去的一点点（DIP）。
///
/// ## 为什么需要外扩（2026-10-18）
///
/// 布局尺寸是**逻辑整数**（`logical_size` 用 `round` 把物理客户区除以缩放系数），
/// 而 D2D 绘制时再乘回缩放系数变成物理像素。**先除后乘会掉一点**：
/// 客户区 603 物理像素、缩放 1.25 时 `round(603 / 1.25) = 482`，
/// 而 `482 × 1.25 = 602.5` —— 面板的物理底边只到 602.5，
/// **最后半行没有盖住**，透出来的是桌面（浅灰），经抗锯齿后就是使用者报的
/// "底部一道极细的浅色边"。面板从左上角起排，所以缺口永远落在右/下两边。
///
/// ## 为什么是 0.5
///
/// `round(x)` 的误差严格小于 0.5，所以 `物理/缩放 − 逻辑 < 0.5` 对**任何**缩放系数
/// 成立（缩放系数在乘回去时约掉，这不是经验值而是 `round` 的性质）。
/// 于是右/下各外扩 `0.5` 就必定盖住整个物理客户区，**且与 DPI 无关** ——
/// 换显示器、改缩放、`DpiChanged` 都自动成立，不需要知道当前缩放是多少。
///
/// 外扩只加在右/下：左/上是 `0`，本来就没有缺口。
/// 多画出去的部分落在窗口外，由窗口自己裁掉，不影响观感。
///
/// ## 为什么不去改 `logical_size` 的取整
///
/// 试过把 `round` 改 `ceil`，但那会让**布局的逻辑尺寸**与
/// **落盘记录的逻辑尺寸**（`PopoutWindow::persistable` 用 `round`）对同一个窗口
/// 给出不同的数。而落盘尺寸要参与"重启后按记录恢复"，两边不一致就可能在
/// 反复重启之间漂移。外扩只动**绘制**，布局、命中测试、落盘一个字节都不变 ——
/// 这是把风险压到最小的做法。
pub const PANEL_OVERDRAW: f32 = 0.5;

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
    /// 窗口整体（命中测试与内容排版用；逻辑整数）
    pub panel: RectI,
    /// **面板填充**用的矩形 `(left, top, right, bottom)`，右/下各外扩
    /// [`PANEL_OVERDRAW`]。**元组含义与 `panel_stroke` 不同**：
    /// 这个是 `(left, top, right, bottom)`，直接喂给 `render::painters::rounded`
    ///（那个函数的第 3/4 个参数是 right/bottom）。
    ///
    /// ## 为什么需要它（2026-10-18）
    ///
    /// 逻辑尺寸是整数、绘制时乘回缩放系数，**先除后乘会掉一点**，
    /// 面板底边可能停在第 `N − 0.5` 个物理像素上 → 最后半行没盖住 →
    /// 透出浅灰桌面，经抗锯齿后是一道极细的浅色边（使用者报的"底部浅色边"）。
    /// 详见 [`PANEL_OVERDRAW`] 的推导。
    pub panel_fill: (f32, f32, f32, f32),
    /// **描边**用的矩形 `(left, top, right, bottom)`，四边各内缩半个描边宽度。
    ///
    /// ## 元组语义必须与消费方一致（2026-10-18 的错就在这）
    ///
    /// 这里曾按 `(left, top, width, height)` 填成 `width - PANEL_STROKE`，
    /// 而绘制那边是 `rounded(...)` —— **它的第 3/4 个参数是 right/bottom**
    ///（`rounded_i` 才做 `x+w` 换算）。于是右/下实际内缩了 1.2（一个整线宽）而不是
    /// 0.6：描边不对称，缺口落在底部与右侧。
    ///
    /// 主窗口那边（`render/painters/panel.rs`）一开始就是对的：
    /// `(left + half, top + half, right - half, bottom - half)`，因为它的 `rect`
    /// 参数本来就是 right/bottom 语义。**同一个公式两处实现，对的那份没能拦住这份**
    /// —— 见架构文档 §17.9。
    ///
    /// ## 为什么需要内缩
    ///
    /// `panel` 与客户区完全重合（`(0,0,width,height)`），而 D2D 的
    /// `DrawRoundedRectangle` 是**以路径为中心**画线的：直接用 `panel` 画，
    /// 外侧那半个描边（0.6px）就落在客户区外，被窗口边缘与 DWM 的圆角裁剪削掉，
    /// 弧上留下一道浅色毛边 —— 玻璃关闭时面板铺满窗口，尤其明显。
    ///
    /// 存 f32 而不是 `RectI`：半个线宽是 0.6，整数矩形表达不了
    ///（`render::painters::settings` 里那条 1.2 描边出于同样理由用了 f32 版）。
    pub panel_stroke: (f32, f32, f32, f32),
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
        // 填充矩形：右/下外扩，盖住"先除后乘"掉掉的那不到半个逻辑像素。
        // 元组是 (left, top, right, bottom)，与 `render::painters::rounded` 对齐。
        let panel_fill = (
            0.0,
            0.0,
            width as f32 + PANEL_OVERDRAW,
            height as f32 + PANEL_OVERDRAW,
        );

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
            panel_fill,
            // 描边：四边各内缩**半个线宽**，元组是 `(left, top, right, bottom)` ——
            // 与消费方 `render::painters::rounded` 一致（它把第 3/4 个参数直接当
            // right/bottom 用）。
            //
            // 这里曾经意图写"内缩半个线宽"，却按 `(left, top, width, height)` 填了
            // `width - PANEL_STROKE`，于是右边与下边实际内缩了 **1.2**（一个整线宽）
            // 而不是 0.6 —— 描边不对称，缺口全落在底部与右侧。
            // 这个错**骗得过"没越出客户区"那类断言**（错的版本更保守），
            // 所以自检查的是"四个内缩量彼此相等"。
            panel_stroke: crate::render::painters::inset_stroke_rect(
                (0.0, 0.0, width as f32, height as f32),
                PANEL_STROKE,
            ),
            title_bar,
            title_text,
            buttons,
            content,
            grid_origin,
            tile,
            columns,
        }
    }

    /// 描边矩形的圆角半径。
    ///
    /// 内缩了多少，半径就减多少 —— 这样描边的中线与面板填充的轮廓**同心**，
    /// 转角处不会出现"一边贴着填充、一边留缝"。减到负值没有意义，夹到 0。
    ///
    /// **公式在 `render::painters::inset_stroke_radius`，与主窗口共用同一处。**
    /// 这里只是转发：半径来自装配层的配色，算布局时还不知道，所以只能这时候要。
    pub fn panel_stroke_radius(&self, panel_radius: f32) -> f32 {
        crate::render::painters::inset_stroke_radius(panel_radius, PANEL_STROKE)
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

    /// 命中哪个方格，返回**可见序号**（与 [`PopoutLayout::tile_at`] 同口径）。
    ///
    /// 命中范围是**整行**（方格 + 它下面的名称），不是只有那个正方形。
    /// 主窗口的方格矩形同样是"图标 + 名称"一整块（`ui::layout` 里 `tile_h`
    /// 含 `LABEL_HEIGHT`），所以两边的点击手感一致 —— 点名字也算点那一格。
    ///
    /// 边界都按 `visible_tiles` 算：格间空隙、行尾空白、标题栏、四个按钮
    /// 都返回 `None`。**不要**在这里另写一份行高/列数判断，
    /// 否则"画出来的、取图标的、点得到的"会变成三个范围。
    pub fn tile_index_at(&self, lx: i32, ly: i32, total: usize) -> Option<usize> {
        let row_h = self.row_height();
        (0..self.visible_tiles(total)).find(|&i| {
            let (tx, ty) = self.tile_at(i, row_h);
            RectI::new(tx, ty, self.tile, row_h).contains(lx, ly)
        })
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

    /// 内容区里**完整放得下**的格子数（不足一整行的部分不算）。
    ///
    /// 绘制与"该给哪几格取图标"共用这一个判据。两处各写一份的话，
    /// 会出现"画了却没请求图标"（格子一直空着）或"请求了却不画"
    /// （白提取，还占着缓存）—— 而且这两种症状都不会报错。
    ///
    /// 判据逐字对应绘制时的约束：整行（方格 + 名称行）的底边不超过内容区下边缘。
    pub fn visible_tiles(&self, total: usize) -> usize {
        let row_h = self.row_height();
        let bottom = self.content.y + self.content.h;
        let mut n = 0;
        while n < total {
            let (_, ty) = self.tile_at(n, row_h);
            if ty + row_h > bottom {
                break;
            }
            n += 1;
        }
        n
    }
}
