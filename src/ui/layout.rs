//! 布局计算：把窗口尺寸与配置换算成各区域矩形，单位是**逻辑像素（DIP）**。
//!
//! ## 为什么必须缓存
//!
//! 命中测试在鼠标移动时高频触发，每次都要**同步**返回结果。
//! 所以布局只在尺寸 / DPI / 配色 / 内容数量变化时重算一次并缓存，
//! 命中测试与绘制都读这份缓存 —— 命中测试里绝不能现场重算布局。
//!
//! 单位统一用 DIP：D2D 上下文的 DPI 已设成窗口 DPI，
//! 配置里的尺寸也都是逻辑值，因此这一层完全不需要关心物理像素。

use crate::platform::hittest::{HitTest, RectI};

/// 标题栏高度
/// 标题栏高度。
///
/// 52 → 48 → 44（需求：顶部整体缩小 4 像素，分两次）。它同时决定侧栏与内容区的起点
///（`sidebar.y = panel.1 + TITLE_BAR`），所以整个顶部的下边界会一起上移。
const TITLE_BAR: i32 = 44;
/// 窗口缩放边框宽度
pub const RESIZE_BORDER: i32 = 6;
/// 标题栏按钮边长
/// 标题栏按钮的边长。
///
/// 34 → 30 → 26（需求：右侧按钮缩小 4 像素，分两次）。
/// `CAPTION_TOP` 保持 9 不用改 —— 按钮在标题栏里垂直居中，
/// 每一档算下来都是 `(高度 - 边长) / 2 = 9`：52/34、48/30、44/26 都成立。
const CAPTION_BUTTON: i32 = 26;
/// 标题栏按钮到面板右边缘的间距
const CAPTION_MARGIN: i32 = 12;
/// 标题栏按钮距面板顶部的间距
const CAPTION_TOP: i32 = 9;
/// 两个按钮之间的间距
const CAPTION_GAP: i32 = 6;
/// 标题文字距面板左边缘
/// 标题文字的左边距。
///
/// **与 `CAPTION_MARGIN` 取同一个值**：右侧按钮距右边缘多远，标题就距左边缘多远，
/// 两边的留白才对称。原来取 28 是照着设计稿的旧值，视觉上偏右。
const TITLE_TEXT_LEFT: f32 = CAPTION_MARGIN as f32;
/// 标题文字与左侧按钮之间的留白
const TITLE_TEXT_RIGHT_GAP: f32 = 14.0;

/// 侧栏宽度（逻辑像素）。做成常量而不是配置项：它同时决定命中测试与绘制，
/// 用户改它的收益远小于把布局搞坏的风险。
/// 侧栏宽度。
///
/// 原来是 104，收窄四分之一 → 78。选项卡按钮的宽度是由它减去左右内距算出来的
///（见下方 `button_w`），所以**文字边框会跟着一起收窄**，不需要另改。
pub const SIDEBAR_WIDTH: f32 = 78.0;

/// 侧栏按钮的上下间距
const TAB_GAP: i32 = 8;
/// 侧栏按钮距侧栏边缘的留白
/// 侧栏按钮距侧栏边缘的留白。
///
/// 从 10 收到 8：左右各少 2px，按钮宽度就多 4px（58 → 62）——
/// 需求要的就是这 4px，好让"主窗口"这类四个中文字能完整显示。
const TAB_SIDE_PADDING: i32 = 8;
/// 选项卡按钮高度。
///
/// 原来是"图标方块 44 + 文字条 20" = 64。改成纯文字按钮后图标方块不复存在，
/// 按需求"高度缩小到当前三分之一"取 64/3 ≈ 21，取整 22。
const TAB_HEIGHT: i32 = 22;
/// 左下角设置按钮的边长
const SETTINGS_BUTTON: i32 = 48;
/// 侧栏底部留给设置按钮的高度
const SETTINGS_AREA: i32 = SETTINGS_BUTTON + 20;
/// 方格内边距：方格边缘到**图标边缘**的固定距离。
///
/// 需求里明确"锁死 4 个像素"，所以这是个常量而不是比例。
/// `pub` 是给 `render::painters::grid` 用的 —— 绘制必须用同一份数值，
/// 否则图标和名称的位置会和布局算出来的错开。
pub const TILE_PADDING: f32 = 4.0;
/// 图标与名称之间的间隙。
///
/// 需求是"文字紧贴图标"，所以比内边距还小一点。
pub const LABEL_GAP: f32 = 2.0;
/// 名称行高（单行）。
pub const LABEL_HEIGHT: f32 = 18.0;
/// 名称的最小可用宽度。
///
/// 需求"文字长度最小小到 2 个中文长度"：字号 12.5px，一个中文字约 12.5px 宽，
/// 两个字约 25px，取 26 留一点余量。方格的**最小宽度**由它决定 ——
/// 再窄就一个字都显示不全，缩下去没有意义。
pub const MIN_LABEL_WIDTH: f32 = 26.0;

/// 方格的最小边长：恰好让名称放得下 2 个中文字。
///
/// 这个下限决定了用户能把图标缩到多小、一屏能塞下多少条目。
/// 之前是 64（相当于 4 个中文字），比需求宽了一倍。
const MIN_TILE: f32 = MIN_LABEL_WIDTH + TILE_PADDING * 2.0;

/// 内容区内边距。方格的起点、网格线的端点、滚动条的轨道都按它对齐 ——
/// 滚动条若从内容区顶边开始，会比第一行方格高出一截，看起来像没对齐。
/// 标题栏底边之下的内容起始下移量。
///
/// **侧栏按钮与方格框共用这一个值**。需求要求两者始终同高：
/// 各写一个字面量的话，改一个忘一个就会错位成"按钮比方格高 2px"，
/// 而这种错位不报错，只能靠肉眼发现。
const CONTENT_TOP_OFFSET: i32 = 5;

/// 方格网格的上内距。
///
/// 由 [`CONTENT_TOP_OFFSET`] 派生，**不要再单独给一个数**。
/// 原来这里是 0，方格框的上边缘正好压在标题栏底边的分隔线上；
/// 需求要求下移 2px，于是它和按钮一起让出这 2px。
/// 左右和下仍用 `GRID_PAD`，否则内容会顶到窗口边。
const GRID_PAD_TOP: f32 = CONTENT_TOP_OFFSET as f32;
/// 方格网格的左右下内距
pub const GRID_PAD: f32 = 16.0;

/// 设置面板距内容区边缘的留白
const SETTINGS_MARGIN: i32 = 12;
/// 设置面板关闭按钮边长
const SETTINGS_CLOSE: i32 = 30;
/// 设置项距面板左右边缘
const SETTINGS_ROW_INSET: i32 = 14;
/// 底部按钮：理想宽度、两档更小的宽度、下限宽度
const SETTINGS_BTN_W: i32 = 96;
const SETTINGS_BTN_W_MID: i32 = 84;
const SETTINGS_BTN_W_MIN: i32 = 68;
/// 底部按钮高度：正常 / 中等 / 最小
const SETTINGS_BTN_H: i32 = 30;
const SETTINGS_BTN_H_MID: i32 = 28;
const SETTINGS_BTN_H_MIN: i32 = 26;
/// 底部按钮之间的间距：标准 / 紧凑
const SETTINGS_BTN_GAP: i32 = 12;
const SETTINGS_BTN_GAP_MIN: i32 = 6;
/// 底部按钮距面板左右与下边缘
const SETTINGS_BTN_MARGIN: i32 = 16;
/// 第一个设置项距面板顶部的距离（让出标题行）
const SETTINGS_ROWS_TOP: i32 = 58;
/// 设置项行高
const SETTINGS_ROW_HEIGHT: i32 = 44;
/// 设置项行距（含间隔）
const SETTINGS_ROW_PITCH: i32 = 52;
/// 基础色色块的边长
const SWATCH_SIZE: i32 = 20;
/// 色块之间的最小间距
const SWATCH_GAP: i32 = 5;
/// 色块区与设置项之间的间距
const SWATCHES_TOP_GAP: i32 = 14;

/// 标题栏上的按钮。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionButton {
    /// "隐藏桌面图标"：切换桌面上文件与快捷方式的显示。
    HideDesktopIcons,
    /// "添加"按钮：点开菜单添加 shell 位置（此电脑、回收站…）
    Add,
    Minimize,
    Close,
}

/// 布局的输入参数。
///
/// 打成一个结构体而不是一串位置参数：调用点有十来个字段，
/// 位置参数传错一个顺序编译器也不会报错。
#[derive(Debug, Clone, Copy)]
pub struct LayoutInput {
    /// 窗口客户区宽度（DIP）
    pub width: f32,
    /// 窗口客户区高度（DIP）
    pub height: f32,
    /// 面板外框相对窗口的内缩量。
    ///
    /// * 玻璃效果开启时 = 发光半径 + 1：给外发光预留位置，否则会被窗口边界裁掉。
    /// * 玻璃效果关闭时 = **0**：面板铺满整个窗口，窗口外不再有任何透明/发光区域。
    pub panel_inset: f32,
    /// 面板圆角
    pub corner_radius: f32,
    /// 侧栏宽度
    pub sidebar_width: f32,
    /// 方格间距
    pub tile_gap: u32,
    /// 列数。`0` 表示按可用宽度自动分列。
    pub columns: u32,
    /// 图标边长，决定方格高度
    pub icon_size: u32,
    /// 当前要显示的条目数
    pub item_count: usize,
    /// 期望的纵向滚动量（DIP）。会被夹到 `[0, max_scroll]`。
    pub scroll: f32,
    /// 设置面板里有多少个设置项（决定行矩形数量）
    pub settings_rows: usize,
}

/// 一次算好、可反复查询的布局。
#[derive(Debug, Clone)]
pub struct Layout {
    /// 面板外框 `(left, top, right, bottom)`
    pub panel: (f32, f32, f32, f32),
    /// 面板圆角半径
    pub radius: f32,
    /// 标题文字矩形
    pub title_text: RectI,
    pub minimize: RectI,
    pub close: RectI,
    /// "添加"按钮。排在最小化左边 —— 它属于"内容操作"，
    /// 和窗口控制（最小化/关闭）分开，且离关闭最远，不会误点。
    pub add: RectI,
    /// "隐藏桌面图标"按钮。排在"添加"左边，同属桌面级操作。
    pub hide_desktop_icons: RectI,

    /// 侧栏区域
    pub sidebar: RectI,
    /// 侧栏上每个选项卡按钮的位置，顺序与配置里的选项卡一致
    pub tab_buttons: Vec<RectI>,
    /// 左下角设置按钮
    pub settings_button: RectI,

    /// 内容区
    pub content: RectI,
    /// 方格之间的间隙（DIP，水平与垂直同值）。
    ///
    /// 绘制层要用它把"拖拽排序的插入线"画在**两格之间的缝**里。这个位置
    /// 靠方格矩形反推不出来 —— 插在行尾那一道的右边没有下一个方格，
    /// 光看矩形只能猜一个宽度。所以间隙由布局明确给出，不让绘制层自己编。
    pub tile_gap: u32,
    /// **可见**方格的位置。
    ///
    /// `tiles[i]` 对应第 `first_item + i` 号条目，不是第 `i` 号。
    /// 只构造可见的那几条：条目上万时不该为屏幕外的位置算矩形。
    pub tiles: Vec<RectI>,
    /// 第一个可见条目的下标
    pub first_item: usize,
    /// 实际生效的滚动量（已夹紧）。调用方应据此同步自己的滚动状态。
    pub scroll: f32,
    /// 最大可滚动量。为 0 表示内容一屏放得下。
    pub max_scroll: f32,
    /// 总行数，供滚动条绘制使用
    pub total_rows: usize,

    /// 列数。缩放验证与滚动条都要用。
    pub grid_columns: u32,
    /// **可见**行数。滚动条按它算滑块长度。
    pub grid_rows: usize,
    /// 生效的图标边长（DIP）。
    ///
    /// 绘制图标时**必须**用这个值而不是从方格反算 —— 反算出来的尺寸
    /// 会比位图的实际尺寸大（方格边长是"图标 + 内边距 + 名称区"），
    /// 于是 128px 的位图被画成 136px，放大了就发虚。
    pub icon_size: f32,

    // ---- 设置面板（覆盖在内容区之上）----
    /// 设置面板外框
    pub settings_panel: RectI,
    /// 设置面板的关闭按钮
    pub settings_close: RectI,
    /// 设置面板底部的一排按钮：**退出程序 / 关于更新 / 使用说明**。
    ///
    /// 顺序即左→右。放在底部一排而不是并进设置项列表：它们都不改变任何设置
    ///（两个是动作、一个是打开文档），和开关混在一起会让人以为点它们会改什么。
    pub settings_buttons: [RectI; 3],
    /// 底部按钮的紧凑档位：`0` 正常，`1`/`2`/`3` 依次更小。
    ///
    /// 布局层只给出档位、不给出字号：`Role` 属于绘制层，布局层不该依赖它。
    /// 绘制层按档位选一个更小的文本角色（见 `settings.rs`）。
    pub settings_button_level: u8,
    /// 每个设置项的行矩形，顺序与界面上的项一致
    /// 设置面板里的基础色色块，按行铺开。
    pub settings_swatches: Vec<RectI>,

    pub settings_rows: Vec<RectI>,
}

impl Layout {
    pub fn compute(input: &LayoutInput, tab_count: usize) -> Self {
        // 内缩量由调用方决定：开了发光就要留边距，关掉发光就铺满窗口。
        let inset = input.panel_inset.max(0.0);
        let panel = (
            inset,
            inset,
            (input.width - inset).max(inset + 1.0),
            (input.height - inset).max(inset + 1.0),
        );

        // ---- 标题栏 ----
        let button = CAPTION_BUTTON as f32;
        let top = panel.1 + CAPTION_TOP as f32;
        let close_x = panel.2 - CAPTION_MARGIN as f32 - button;
        let minimize_x = close_x - CAPTION_GAP as f32 - button;
        let add_x = minimize_x - CAPTION_GAP as f32 - button;
        let desktop_x = add_x - CAPTION_GAP as f32 - button;
        let text_left = panel.0 + TITLE_TEXT_LEFT;
        let text_right = desktop_x - TITLE_TEXT_RIGHT_GAP;

        // ---- 侧栏 ----
        let sidebar_width = input
            .sidebar_width
            .clamp(64.0, (panel.2 - panel.0) * 0.4);
        let sidebar = RectI::new(
            panel.0 as i32,
            (panel.1 + TITLE_BAR as f32) as i32,
            sidebar_width as i32,
            ((panel.3 - TITLE_BAR as f32) - panel.1) as i32,
        );

        // 选项卡按钮：从侧栏顶部往下排
        let button_w = (sidebar.w - TAB_SIDE_PADDING * 2).max(24);
        let button_h = TAB_HEIGHT;
        let mut tab_buttons = Vec::with_capacity(tab_count);
        // 第一个按钮的上边缘在标题栏底边之下 `CONTENT_TOP_OFFSET` 处。
        // 这个偏移与方格共用（见该常量的说明），保证两者始终同高。
        let mut y = sidebar.y + CONTENT_TOP_OFFSET;
        for _ in 0..tab_count {
            tab_buttons.push(RectI::new(sidebar.x + TAB_SIDE_PADDING, y, button_w, button_h));
            y += button_h + TAB_GAP;
        }

        // 设置按钮贴在侧栏底部居中
        let settings_button = RectI::new(
            sidebar.x + (sidebar.w - SETTINGS_BUTTON) / 2,
            sidebar.y + sidebar.h - SETTINGS_AREA,
            SETTINGS_BUTTON,
            SETTINGS_BUTTON,
        );

        // ---- 内容区 ----
        let content = RectI::new(
            (panel.0 as i32) + sidebar.w,
            sidebar.y,
            (panel.2 as i32) - (panel.0 as i32) - sidebar.w,
            sidebar.h,
        );

        // ---- 方格网格 ----
        let gap = input.tile_gap as f32;
        let pad = GRID_PAD;
        // 上内距单独取，且为 0 —— 见 `GRID_PAD_TOP` 的说明。
        // 高度要**分别**扣上、下内距：写成 `pad * 2.0` 会把上面那 0 当成 16，
        // 于是少算一行，最后一行被挤出可视区。
        let pad_top = GRID_PAD_TOP;
        let usable_w = (content.w as f32 - pad * 2.0).max(MIN_TILE);
        let usable_h = (content.h as f32 - pad_top - pad).max(MIN_TILE);

        // 先定列数，再**反过来算方格宽度**把整行填满。
        //
        // 顺序很关键：若先固定方格宽度再排位置，用户配了 5 列而窗口只放得下 2 列时，
        // 后 3 列会被画到窗口外面去 —— 看起来像布局坏了，实际是算法顺序错了。
        //
        // 自动列数由 **icon_size** 推导，而不是配置里的固定 `tile_size`：
        // 这样 Ctrl+滚轮缩放时整个方格（含背景底板）一起变，一屏能放的条目数跟着变。
        // 之前用固定 tile_size 定列数、只让 tile_h 随 icon_size 变，
        // 结果是缩小图标只会让方格变扁，条目数一点没多。
        let max_columns = (((usable_w + gap) / (MIN_TILE + gap)).floor() as u32).max(1);
        let columns = if input.columns == 0 {
            // 目标宽度：图标宽度，但**不低于名称的最小宽度**（2 个中文字）。
            //
            // 不取图标宽度本身是因为图标可以缩到 16px，那样方格只有 24px，
            // 名称连一个字都放不下 —— 用户明确要求最小也要能显示 2 个中文字。
            let content_w = (input.icon_size as f32).max(MIN_LABEL_WIDTH);
            let want = content_w + TILE_PADDING * 2.0;
            (((usable_w + gap) / (want + gap)).floor() as u32).max(1)
        } else {
            input.columns.clamp(1, max_columns)
        };
        let tile_w = ((usable_w - (columns - 1) as f32 * gap) / columns as f32).max(MIN_TILE);

        // 方格高度：内边距 + 图标 + 间隙 + 名称 + 内边距。
        //
        // **不再做成正方形**。需求是"文字移到方格内部、紧贴图标、内边距固定 4px"，
        // 而名称行必须有高度 —— 方形就意味着宽度要跟着变高，
        // 那样内边距就不可能是固定 4px了。两者不可兼得，这里按新需求来。
        let tile_h = TILE_PADDING
            + input.icon_size as f32
            + LABEL_GAP
            + LABEL_HEIGHT
            + TILE_PADDING;
        let pitch_y = tile_h + gap;

        // ---- 滚动 ----
        let total_rows = input.item_count.div_ceil(columns as usize);
        // 内容总高：末行不带间距
        let content_height = total_rows as f32 * tile_h
            + total_rows.saturating_sub(1) as f32 * gap;
        let max_scroll = (content_height - usable_h).max(0.0);
        let scroll = input.scroll.clamp(0.0, max_scroll);

        // 只构造可见行范围内的方格
        let first_row = (scroll / pitch_y).floor() as usize;
        let last_row = (((scroll + usable_h) / pitch_y).ceil() as usize).min(total_rows);
        let first_item = (first_row * columns as usize).min(input.item_count);
        let last_item = (last_row * columns as usize).min(input.item_count);

        let mut tiles = Vec::with_capacity(last_item - first_item);
        for i in first_item..last_item {
            let row = i / columns as usize;
            let col = i % columns as usize;
            tiles.push(RectI::new(
                (content.x as f32 + pad + col as f32 * (tile_w + gap)) as i32,
                (content.y as f32 + pad_top + row as f32 * pitch_y - scroll) as i32,
                tile_w as i32,
                tile_h as i32,
            ));
        }
        let visible_rows = last_row.saturating_sub(first_row);

        // ---- 设置面板 ----
        // 覆盖在内容区之上，留一圈边距露出后面的面板底色。
        let settings_panel = RectI::new(
            content.x + SETTINGS_MARGIN,
            content.y + SETTINGS_MARGIN,
            (content.w - SETTINGS_MARGIN * 2).max(120),
            (content.h - SETTINGS_MARGIN * 2).max(120),
        );
        let settings_close = RectI::new(
            settings_panel.x + settings_panel.w - SETTINGS_CLOSE - 10,
            settings_panel.y + 10,
            SETTINGS_CLOSE,
            SETTINGS_CLOSE,
        );
        // ---- 底部一排按钮：退出程序 / 关于更新 / 使用说明 ----
        //
        // 需求："三个按钮均匀分布，如果放不下适当缩小按钮和字体。"
        //
        // "均匀分布"取**等宽 + 等间距**：三个按钮等宽，按钮之间、以及整组
        // 左右两侧到面板边缘的空白相等。整组**居中**而不是拉满整行 ——
        // 四个字的按钮拉成 190px 会很空。宽度不够时按档位依次缩：
        // 缩间距 → 缩按钮与高度 → 缩字号（字号由绘制层按档位选）。
        let avail = (settings_panel.w - SETTINGS_BTN_MARGIN * 2).max(48);
        let (gap, btn_w, btn_h, level) = if (avail - SETTINGS_BTN_GAP * 2) / 3 >= SETTINGS_BTN_W {
            (SETTINGS_BTN_GAP, SETTINGS_BTN_W, SETTINGS_BTN_H, 0u8)
        } else if (avail - SETTINGS_BTN_GAP * 2) / 3 >= SETTINGS_BTN_W_MID {
            (SETTINGS_BTN_GAP, SETTINGS_BTN_W_MID, SETTINGS_BTN_H, 1)
        } else if (avail - SETTINGS_BTN_GAP_MIN * 2) / 3 >= SETTINGS_BTN_W_MIN {
            (SETTINGS_BTN_GAP_MIN, SETTINGS_BTN_W_MIN, SETTINGS_BTN_H_MID, 2)
        } else {
            // 极窄：按可用宽度均分，用最小高度与最小字号。取 40 为绝对下限，
            // 再小就只是三个色块、点了也不知道是什么。
            (
                SETTINGS_BTN_GAP_MIN,
                ((avail - SETTINGS_BTN_GAP_MIN * 2) / 3).max(40),
                SETTINGS_BTN_H_MIN,
                3,
            )
        };
        let group_w = btn_w * 3 + gap * 2;
        let btn_x0 = settings_panel.x + (settings_panel.w - group_w) / 2;
        let btn_y = settings_panel.y + settings_panel.h - btn_h - SETTINGS_BTN_MARGIN;
        let mut settings_buttons = [RectI::default(); 3];
        for (i, r) in settings_buttons.iter_mut().enumerate() {
            *r = RectI::new(btn_x0 + i as i32 * (btn_w + gap), btn_y, btn_w, btn_h);
        }
        let settings_button_level = level;

        let row_width = settings_panel.w - SETTINGS_ROW_INSET * 2;
        let mut settings_rows = Vec::with_capacity(input.settings_rows);
        for i in 0..input.settings_rows {
            settings_rows.push(RectI::new(
                settings_panel.x + SETTINGS_ROW_INSET,
                settings_panel.y + SETTINGS_ROWS_TOP + i as i32 * SETTINGS_ROW_PITCH,
                row_width,
                SETTINGS_ROW_HEIGHT,
            ));
        }

        // 基础色色块：接在设置项下方。
        //
        // 面板矩形本来就填满内容区，而设置项只占顶部一段，所以网格**不用**
        // 额外增加面板高度，直接排在下面即可。
        let grid_top = settings_panel.y
            + SETTINGS_ROWS_TOP
            + input.settings_rows as i32 * SETTINGS_ROW_PITCH
            + SWATCHES_TOP_GAP;
        let cols = crate::config::theme::BASE_PALETTE_COLS.max(1);
        let count = crate::config::theme::BASE_PALETTE.len();
        let mut settings_swatches = Vec::with_capacity(count);
        // **铺满整行宽度**（需求）：格子大小按上面的常量固定，多出来的宽度
        // 均匀摊进列间距里，而不是让网格缩在左边。
        //
        // 用实际列数而不是固定列数算间距 —— 最后一列右边到面板边缘的距离
        // 也要一起摊掉，否则整块会看着偏左。
        let used_cols = cols.min(count.max(1));
        let pitch = if used_cols > 1 {
            ((row_width - SWATCH_SIZE * used_cols as i32) / (used_cols as i32 - 1))
                .max(SWATCH_GAP)
        } else {
            SWATCH_GAP
        };
        for i in 0..count {
            let r = (i / cols) as i32;
            let c = (i % cols) as i32;
            settings_swatches.push(RectI::new(
                settings_panel.x + SETTINGS_ROW_INSET + c * (SWATCH_SIZE + pitch),
                grid_top + r * (SWATCH_SIZE + SWATCH_GAP),
                SWATCH_SIZE,
                SWATCH_SIZE,
            ));
        }

        Self {
            panel,
            radius: input.corner_radius,
            title_text: RectI::new(
                text_left as i32,
                panel.1 as i32,
                ((text_right - text_left).max(20.0)) as i32,
                TITLE_BAR,
            ),
            minimize: RectI::new(minimize_x as i32, top as i32, button as i32, button as i32),
            close: RectI::new(close_x as i32, top as i32, button as i32, button as i32),
            add: RectI::new(add_x as i32, top as i32, button as i32, button as i32),
            hide_desktop_icons: RectI::new(desktop_x as i32, top as i32, button as i32, button as i32),            sidebar,
            tab_buttons,
            settings_button,
            content,
            tiles,
            first_item,
            scroll,
            max_scroll,
            total_rows,
            grid_columns: columns,
            grid_rows: visible_rows,
            icon_size: input.icon_size as f32,
            tile_gap: input.tile_gap,
            settings_panel,
            settings_close,
            settings_buttons,
            settings_button_level,
            settings_swatches,
            settings_rows,
        }
    }

    /// 生成 `WM_NCHITTEST` 需要的参数。
    ///
    /// `maximized` 为真时不给缩放边 —— 否则拖边缘会变成"还原窗口"。
    pub fn hit_test(&self, maximized: bool) -> HitTest {
        let mut cfg = HitTest {
            title_bar_height: TITLE_BAR,
            resize_border: if maximized { 0 } else { RESIZE_BORDER },
            ..Default::default()
        };
        // 按钮要从标题栏拖动区里排除掉，否则点"关闭"会被当成拖窗口。
        //
        // **每加一个标题栏按钮都必须在这里补一行**：漏掉的话，点它会被
        // WM_NCHITTEST 判成 HTCAPTION，系统于是发 WM_NCLBUTTONDOWN 而不是
        // WM_LBUTTONDOWN —— 应用层的点击处理**根本不会被调用**，
        // 表现为"按钮画出来了但点不动"，且没有任何报错。
        //（"+"按钮就踩过这个坑。）
        cfg.exclude(self.add);
        cfg.exclude(self.hide_desktop_icons);
        cfg.exclude(self.minimize);
        cfg.exclude(self.close);
        // 标题文字**不在这里**排除：它的可点范围只应覆盖文字本身，
        // 而宽度要按文本估算，布局层拿不到字符串。
        // 由 `App::hit_test` 用 `title_hit_rect()` 补上。
        cfg
    }

    /// 命中标题栏按钮。
    pub fn caption_button_at(&self, x: i32, y: i32) -> Option<CaptionButton> {
        if self.close.contains(x, y) {
            Some(CaptionButton::Close)
        } else if self.minimize.contains(x, y) {
            Some(CaptionButton::Minimize)
        } else if self.add.contains(x, y) {
            Some(CaptionButton::Add)
        } else if self.hide_desktop_icons.contains(x, y) {
            Some(CaptionButton::HideDesktopIcons)
        } else {
            None
        }
    }

    /// 命中侧栏选项卡，返回其序号。
    pub fn tab_at(&self, x: i32, y: i32) -> Option<usize> {
        self.tab_buttons.iter().position(|r| r.contains(x, y))
    }

    /// 命中左下角设置按钮。
    pub fn settings_at(&self, x: i32, y: i32) -> bool {
        self.settings_button.contains(x, y)
    }

    /// 命中某个方格，返回**条目下标**（不是 `tiles` 的下标）。
    pub fn tile_at(&self, x: i32, y: i32) -> Option<usize> {
        self.tiles
            .iter()
            .position(|r| r.contains(x, y))
            .map(|i| self.first_item + i)
    }

    /// 命中**插入位置**（拖拽排序的落点），返回"缝隙"的序号。
    ///
    /// 取值在 `first_item..=first_item + tiles.len()`：`s` 表示"插在第 `s` 条
    /// 之前"，取到上界表示"插在最后一条之后"。与 [`Layout::tile_at`] 同口径
    ///（都是**条目下标**），调用方不必再做一次换算。
    ///
    /// 点不在内容区里一律返回 `None`。这一条**不能省**：拖到侧栏、标题栏或
    /// 窗口外一点点时若还算得出落点，松手就会让条目莫名跳走 ——
    /// 而用户以为自己取消了这次拖动。
    pub fn drop_slot_at(&self, x: i32, y: i32) -> Option<usize> {
        if !self.content.contains(x, y) || self.tiles.is_empty() {
            return None;
        }

        // 落在某个方格上：按**中线**分前后，决定插在它前面还是后面。
        // 这是资源管理器与桌面的通用手感，用户不需要学。
        if let Some(i) = self.tiles.iter().position(|r| r.contains(x, y)) {
            let r = &self.tiles[i];
            let before = x < r.x + r.w / 2;
            return Some(self.first_item + if before { i } else { i + 1 });
        }

        // 落在方格之间、行尾、或最后一格右边的空白里：取**最近的方格中心**，
        // 再按中线分前后。少了这一支的话，"拖到最后一个图标右边"会算不出落点，
        // 而那恰恰是最常用的操作之一（把条目挪到末尾）。
        //
        // 比较平方距离，不开方：只需要大小关系，开方是白算的。
        let mut best = 0usize;
        let mut best_d = i64::MAX;
        for (i, r) in self.tiles.iter().enumerate() {
            let cx = (r.x + r.w / 2) as i64;
            let cy = (r.y + r.h / 2) as i64;
            let dx = cx - x as i64;
            let dy = cy - y as i64;
            let d = dx * dx + dy * dy;
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        let r = &self.tiles[best];
        let before = x < r.x + r.w / 2;
        Some(self.first_item + if before { best } else { best + 1 })
    }
}
