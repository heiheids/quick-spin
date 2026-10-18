//! 弹出子窗口的窗口本体。
//!
//! 每个弹出窗口持有一整套自己的东西：`Window`（HWND）、`Renderer`（交换链）、
//! `PopoutPainter`、`TextEngine`、布局与配色，以及**自己的图标管线**
//!（`PopoutIcons`：一条加载线程 + 一份位图缓存，见 `icons.rs`）。
//! **不借用主窗口的任何运行时状态**（见 `mod.rs` 的选择 A），
//! 所以两层之间没有 `RefCell`、没有共享可变状态，也没有 `&mut` 别名问题。

use crate::config::theme::Scheme;
use crate::error::Result;
use crate::platform::hittest::HitTest;
use crate::platform::window::{CloseAction, Event, Geometry, Handler, Rect, Window};
use crate::popout::dock::{self, DockRequest};
use crate::config::model::CornerStyle;
use crate::platform::acrylic;
use crate::popout::icons::{Content, PopoutIcons, pixel_size};
use crate::popout::layout::{PopoutLayout, TitleButton};
use crate::popout::painter::{ButtonStates, PopoutPainter};
use crate::popout::palette::{self, PaletteLayout, PaletteText, PaletteView};
use crate::popout::state::DockSide;
use crate::render::composer::Composer;
use crate::render::device::Renderer;
use crate::render::TextEngine;

/// "一次单击"允许的最大移动量（逻辑像素）。
///
/// 与主窗口 `TILE_DRAG_THRESHOLD` 取同一个数（4），手感才一致。
/// 这里**不能**直接引用那个常量：它在 `app` 里，而弹出窗口是下层，
/// 让下层依赖装配层会把依赖方向弄反（同 `launch.rs` 里那条理由）。
/// 数值本身是小的，两处各写一份的风险可接受；要改就一起改。
const CLICK_TOLERANCE: i32 = 4;

/// 按下的方格，等松手时决定要不要启动。
///
/// `Copy`：两个字段都是标量，在 `MouseMove` 里每帧读一次时不必借用 `self.press`
///（借了就没法在同一个分支里写它）。
#[derive(Clone, Copy)]
struct TilePress {
    /// 可见序号（`content.items` 的下标）。
    index: usize,
    /// 按下的起点（逻辑坐标），用来判断"是不是只是抖了一下"。
    start: (i32, i32),
}

/// 一个活着的弹出子窗口。
pub struct Popout {
    window: Window,
    renderer: Renderer,
    composer: Composer,
    painter: PopoutPainter,
    text: TextEngine,
    layout: PopoutLayout,
    scheme: Scheme,
    /// **主窗口**当前的配色。
    ///
    /// 只有 [`Self::base_color`] 为空（"跟随主窗口"）时才会被用到，但两种
    /// 情况都必须留着它：用户随时可能从"自己一套"切回"跟随主窗口"，
    /// 那时要有东西可跟。
    main_scheme: Scheme,
    /// 这个窗口自己的基础色 `#RRGGBB`。**空串 = 跟随主窗口。**
    ///
    /// 真实的选色界面写在 `palette.rs` 的浮层里；这里存的是权威值 ——
    /// 创建时从记录里取，用户每次选色改成新的。`persistable()` 会把它交回
    /// 装配层落盘（见 `state::PopoutWindow::merge_persisted` 里那条分工）。
    base_color: String,
    /// 选色浮层上的两处文字（由装配层从 i18n 取好传进来）。
    palette_text: PaletteText,
    /// 选色浮层：`Some` = 正开着。存算好的布局，绘制与命中共用同一份
    ///（两处各算一次就会出现"点中的和画出来的不是同一格"）。
    picker: Option<PaletteLayout>,
    /// 浮层里悬停的色块下标 / 是否悬停在"跟随主窗口"那一行。
    hover_swatch: Option<usize>,
    hover_follow: bool,
    /// 标题（用户自命名，或退回选项卡名）
    title: String,
    /// 这个弹窗对应哪个选项卡（配置里的稳定 key）。
    ///
    /// 存 key 而不是标题：装配层按 key 存表（`App::popouts`），而且标题是
    /// 用户可以随时改的自由文本，改完就再也对不回是哪个选项卡了。
    /// 窗口自己关掉时要把这个 key 回报上去（见 `popout::report`）。
    tab_key: String,
    /// 当前悬停的标题栏按钮
    hover: Option<TitleButton>,
    /// 按下但还没松手的方格（见 [`TilePress`]）。
    ///
    /// 与主窗口同一个判据：**按下时不启动**，要等松手时确认"没怎么移动过"
    /// 才算一次单击。按下就启动的话，拖动窗口前的那一下按下就会顺手启动一个程序。
    press: Option<TilePress>,
    /// 是否正在拖动窗口尺寸
    resizing: bool,
    /// 要显示的条目，与主窗口该选项卡的**可见条目**保持同步。
    ///
    /// 需求："弹出窗口要同步它在主窗口的内容 —— 弹出只是多了一种显示方式，
    /// 并不影响本身在主窗口的内容。" 也就是说弹出**不把选项卡从侧栏拿走**，
    /// 它是同一份数据的第二个视图，数量与顺序必须和主窗口一致。
    content: Content,
    /// 这个窗口自己的图标加载线程与位图缓存。
    icons: PopoutIcons,
    /// 是否吸附在主窗口侧面（跟着它移动/缩放）。用户一拖动本窗口就变 `false`。
    docked: bool,
    /// **期望**贴在主窗口哪一侧。翻边不改它（见 `state::PopoutWindow::dock_side`）。
    dock_side: DockSide,
    /// 是否"钉住"：主窗口收起时它留着（只管可见性，见 `state::PopoutWindow::pinned`）。
    pinned: bool,
    /// 客户区物理尺寸。
    ///
    /// 吸附计算需要它，而为此每帧调一次 `GetClientRect` 是白开销 ——
    /// 尺寸只在 `WM_SIZE` / DPI 变化时变，跟着更新一份就够。
    phys: (i32, i32),
    /// 用户开始拖动/缩放**之前**的那一份几何 `(x, y, w, h)`。
    ///
    /// `Some` 表示"用户正在拖"。这是区分"用户拖动"与"程序吸附"的**全部依据**：
    /// 见 [`Popout::on_user_size_move_end`]。
    drag_start: Option<(i32, i32, i32, i32)>,
    /// 上一次**由本程序**摆出去的位置。用来跳过重复的 `SetWindowPos`。
    last_pos: Option<(i32, i32)>,
    /// 窗口外框的圆角（系统偏好 + 面板半径），**由装配层算好传进来**。
    ///
    /// 这里不自己推 `if outer_glass { corner_radius } else { 0 }`：那条规则
    /// 主窗口也在用，两处各写一份必然漂移（架构文档 §17.9 的清单）。
    /// 设置变化时由 `App::apply_backdrop` 推新值（见 [`Popout::set_corners`]）。
    corners: CornerStyle,
}

impl Popout {
    /// 创建一个弹出子窗口。
    ///
    /// `tab_key` 是它对应的选项卡 key，窗口自己关掉时要靠它回报装配层。
    /// `docked` / `dock_side` 是它的初始吸附状态（第一次摆位由装配层算好、
    /// 通过 `geometry` 传进来 —— 吸附数学只有 `dock.rs` 那一份）。
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        tab_key: &str,
        title: &str,
        geometry: Geometry,
        main_scheme: Scheme,
        base_color: String,
        palette_text: PaletteText,
        icon_size: i32,
        docked: bool,
        dock_side: DockSide,
        pinned: bool,
        corners: CornerStyle,
    ) -> Result<Self> {
        let window = Window::create(title, geometry, false)?;
        // 系统侧圆角必须显式设一次：Win11 对顶层窗口默认就给圆角，
        // 不设就会拿到系统默认值 —— 使用者报的"弹出小窗底部是圆角"就是这个来源
        //（主窗口一直在 `acrylic::apply` 里设，弹窗从来没设过）。
        acrylic::set_round_corners(window.hwnd(), corners.rounded);
        let (cw, ch) = window.client_size();
        let dpi = window.dpi();
        let (renderer, composer) =
            Renderer::new(window.hwnd(), cw.max(1) as u32, ch.max(1) as u32, dpi as f32)?;

        // 布局用**逻辑像素**：D2D 上下文的 DPI 已设成窗口 DPI，绘制时 DIP 会
        // 自动变成物理像素，所以这一层只跟 DIP 打交道。
        //（`client_size` 给的是物理像素，必须换算一次 —— 之前创建时直接拿物理
        //  尺寸去算布局，而 `Resized` 那条路是换算过的，两条路不一致：
        //  非 100% 缩放下刚创建出来的布局会整体偏大、右下角被裁掉一截。）
        let (lw, lh) = logical_size(cw, ch, window.scale());
        let layout = PopoutLayout::compute(lw, lh, icon_size);
        // 图标管线自己一份（见 `icons.rs`）：加载线程要在这个窗口上唤醒重绘。
        // 请求边长按 `icon_size` 算（与主窗口同一个来源），不是按算出来的方格边长。
        let icons = PopoutIcons::new(
            window.redraw_notifier(),
            pixel_size(icon_size, dpi as f32),
        );

        Ok(Self {
            window,
            renderer,
            composer,
            painter: PopoutPainter::new(),
            text: TextEngine::new()?,
            layout,
            // 自己设过配色就用自己那套，否则跟随主窗口 —— 判据只有一处
            //（`palette::resolve_scheme`），创建、换色、切换跟随都走它。
            scheme: palette::resolve_scheme(&base_color, &main_scheme),
            main_scheme,
            base_color,
            palette_text,
            picker: None,
            hover_swatch: None,
            hover_follow: false,
            title: title.to_string(),
            tab_key: tab_key.to_string(),
            hover: None,
            press: None,
            resizing: false,
            content: Content {
                icon_size,
                items: Vec::new(),
            },
            icons,
            docked,
            dock_side,
            pinned,
            phys: (cw.max(1), ch.max(1)),
            drag_start: None,
            last_pos: geometry.x.zip(geometry.y),
            corners,
        })
    }

    /// 用主窗口推来的快照更新内容。
    ///
    /// 返回内容是否**真的**变了：调用方据此决定要不要请求重绘 ——
    /// 内容没变还每帧重绘弹出窗口是白耗电。
    pub fn set_content(&mut self, content: Content) -> bool {
        if self.content == content {
            return false;
        }
        // 图标尺寸变了（主窗口 `Ctrl+滚轮`）：方格边长必须跟着变，
        // 否则图标与格子的关系会错位（图标比格子大一圈或小一圈）。
        // 弹出窗口自己也有一份尺寸，只在创建时拿到过一次，所以必须一起推过来。
        let icon_changed = self.content.icon_size != content.icon_size;
        self.content = content;
        // 内容换了 → 键表作废，下次绘制重建。
        // **作废的是键，不是缓存**：已经落地并命中同一批键的位图仍留在缓存里，
        // 不会因为换了个名字就把图标全部重新提取一遍。
        self.icons.invalidate();
        if icon_changed {
            self.relayout(self.window.client_size());
        }
        true
    }

    /// 按**物理**客户区尺寸重算布局（内部换算成 DIP），并同步图标请求边长。
    ///
    /// 这两件事必须一起做：请求的位图边长是从方格边长推出来的（1:1 绘制才清晰），
    /// 分开改就会出现"格子变了、位图还是旧尺寸"，图标被拉伸或缩得发虚。
    fn relayout(&mut self, phys: (i32, i32)) {
        let (lw, lh) = logical_size(phys.0, phys.1, self.window.scale());
        self.layout = PopoutLayout::compute(lw, lh, self.content.icon_size);
        self.sync_icon_size();
        // 选色浮层跟着内容区走：拖动边框时它必须重摆，否则绘制与命中
        // 会对着旧矩形 —— 表现是"点到的格子不是看到的那个"。
        if self.picker.is_some() {
            self.picker = Some(PaletteLayout::compute(self.layout.content));
        }
    }

    /// 让图标管线的请求边长跟上当前的图标尺寸与窗口 DPI。
    ///
    /// **取 `content.icon_size`（主窗口的图标尺寸）而不是 `layout.tile`。**
    /// 两个理由，方向相反但结论一致：
    ///
    /// * 需求要的是"同一个 `icon_size` 来源"—— 主窗口就是按 `icon_size` 取图的，
    ///   两边同一个来源才谈得上"图标一致"；
    /// * `layout.tile` 会被内容区宽度夹紧，也就是说**拖动窗口宽度时它在连续变化**。
    ///   拿它当键的输入，拖一次边框就会按每一帧的中间尺寸各请求一批图标，
    ///   而那一批尺寸永远不会再被用到 —— 白跑 shell 提取，正是"拖动变卡"的经典成因。
    ///
    /// 尺寸没变时它什么都不做（见 `PopoutIcons::set_pixel_size`）。
    fn sync_icon_size(&mut self) {
        let px = pixel_size(self.content.icon_size, self.window.dpi() as f32);
        self.icons.set_pixel_size(px);
    }

    /// 请求重绘一次（内容被推来后调用）。
    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }

    /// 把这个窗口当前该记住的状态交回装配层去落盘。
    ///
    /// 装配层持有 `Box<Popout>`，所以这里不必排队回报 —— 直接读即可。
    /// （"用户拖完松手"这种**需要立刻落盘**的时刻仍会走 `report::report_changed`，
    /// 因为那时是窗口自己在处理事件。）
    pub fn persistable(&self) -> crate::popout::state::PopoutWindow {
        let (x, y) = self.window.position();
        let mut w = crate::popout::state::PopoutWindow {
            open: true,
            // 吸附时不存位置：位置由主窗口推导，存下来只会是一份很快过期的
            // 副本，下次启动先按它摆一下再跳到吸附位，看起来就是"闪了一下"。
            x: if self.docked { None } else { Some(x) },
            y: if self.docked { None } else { Some(y) },
            width: (self.phys.0 as f32 / self.window.scale().max(1.0)).round() as i32,
            height: (self.phys.1 as f32 / self.window.scale().max(1.0)).round() as i32,
            docked: self.docked,
            dock_side: self.dock_side,
            pinned: self.pinned,
            name: None, // 装配层自己那份记录里已经有名字，别用这里覆盖它
            // **把自己那份配色交回去。**
            //
            // 早先这里是 `String::new()`（默认值），所以装配层必须"保留记录里的
            // `base_color`"才不至于把它抹掉 —— 但那条保护反过来会让用户选的颜色
            // 被静默丢弃（当时生效、重启就没了）。现在窗口持有权威值，
            // 装配层按 `merge_persisted` 的分工用它。
            base_color: self.base_color.clone(),
        };
        w.normalize();
        w
    }

    /// 吸附跟随：主窗口动了，把它摆到该在的位置。
    ///
    /// 返回**它现在占的矩形**（装配层拿它累积成"已经排上的窗口"再传给下一个，
    /// 堆叠就是这么串起来的）；`None` 表示这次没有参与排列（没吸附、正在被
    /// 用户拖、或拿不到工作区），那它也就不该挡住别人。
    ///
    /// **由装配层在主窗口 `Moved` / `Resized` 时调用**，事件驱动、没有定时器。
    ///
    /// 三种情况直接不动：
    /// * 没吸附（用户拖走过，或者手动关掉了吸附）；
    /// * 用户正在拖它 —— 这时去摆它就是和用户抢鼠标；
    /// * 拿不到显示器工作区（见 `dock::dock_position`）。
    ///
    /// 位置没变时也直接返回：主窗口移动会连发很多条 `WM_MOVE`，
    /// 每次都调一遍 `SetWindowPos` 是没必要的系统调用。
    pub fn follow_dock(
        &mut self,
        main: Rect,
        work: Option<Rect>,
        occupied: &[Rect],
    ) -> Option<Rect> {
        if !self.docked || self.drag_start.is_some() {
            return None;
        }
        // 当前位置：优先用本程序上次摆出去的那个（不产生 syscall）。
        // 只有它还不知道时才问一次窗口 —— 正常情况下创建时就填好了。
        let current = self.last_pos.or_else(|| Some(self.window.position()));
        let req = DockRequest {
            main,
            size: self.phys,
            side: self.dock_side,
            work,
            current,
            occupied,
        };
        let Some(d) = dock::dock_position(&req) else {
            return None;
        };
        if self.last_pos != Some((d.x, d.y)) {
            self.window.move_to(d.x, d.y);
            self.last_pos = Some((d.x, d.y));
        }
        Some(Rect {
            x: d.x,
            y: d.y,
            w: self.phys.0.max(1),
            h: self.phys.1.max(1),
        })
    }

    /// 用户拖完 / 缩放完松手了。
    ///
    /// ## 怎么区分"用户拖动"与"程序吸附"（这一步最容易做坏的地方）
    ///
    /// **`WM_ENTERSIZEMOVE` / `WM_EXITSIZEMOVE` 只有系统进入模态移动/缩放循环时
    /// 才会发**，也就是说只有用户真的抓住标题栏或边框才会来。程序自己调
    /// `SetWindowPos`（吸附跟随走的就是它）**不会**产生这两个消息。
    ///
    /// 所以判据是：只在"用户拖完之后"比较几何 —— 程序摆的那些从来没进过
    /// 这一段，就不可能被误判成用户拖动。**不靠时间窗口、不靠在 SetWindowPos
    /// 前后打标志**：前者是拿时序猜，后者一旦漏配对（异常路径、嵌套拖动）
    /// 就会让窗口莫名其妙脱离吸附。
    ///
    /// 位置变了 = 用户在**移动**它 → 脱离吸附；
    /// 只有尺寸变了 = 用户在**缩放**它 → 保持吸附（并记下新尺寸）。
    fn on_user_size_move_end(&mut self) {
        let Some((sx, sy, _, _)) = self.drag_start.take() else {
            return;
        };
        let (x, y) = self.window.position();
        // 尺寸始终以客户区为准（缩放会改它，移动不会）
        self.phys = self.window.client_size();
        let moved = x != sx || y != sy;

        if self.docked && moved {
            // 需求："弹窗被拖动就脱离吸附，松手后按自由位置保存。"
            self.docked = false;
            crate::diag::log(&format!("脱离吸附: {}", self.tab_key));
        }
        self.last_pos = Some((x, y));
        // 落盘：位置/尺寸/吸附状态都可能变了，而状态归装配层管。
        // 放在动作之后调一次（不是每帧），所以不会变成写盘风暴。
        crate::popout::report::report_changed(&self.tab_key);
    }

    /// 吸附按钮：开 / 关。
    ///
    /// **刻意不在这里算吸附位置**：那需要主窗口矩形与显示器工作区，而这是
    /// 装配层的信息（窗口层不该反向依赖它）。这里只翻状态并回报；
    /// 装配层收到回报后会立刻跑一次跟随，所以开的那一瞬间就会贴上去 ——
    /// 不会出现"点了要等主窗口动一下才生效"。
    pub fn toggle_dock(&mut self) -> bool {
        self.docked = !self.docked;
        // 清掉"上次摆在哪"，逼装配层这次的跟随真的摆一次：
        // 用户可能把窗口拖回吸附位、再点开吸附，那样位置恰好相等，
        // 不清就会因为"没变"而跳过。
        self.last_pos = None;
        crate::diag::log(&format!(
            "弹窗吸附: {} ({})",
            if self.docked { "开" } else { "关" },
            self.tab_key
        ));
        crate::popout::report::report_changed(&self.tab_key);
        true
    }

    /// 钉住按钮：开 / 关（只管"主窗口收起时留不留着"）。
    pub fn toggle_pin(&mut self) -> bool {
        self.pinned = !self.pinned;
        crate::diag::log(&format!(
            "钉住: {} ({})",
            if self.pinned { "开" } else { "关" },
            self.tab_key
        ));
        crate::popout::report::report_changed(&self.tab_key);
        true
    }

    /// 是否吸附中。装配层用它做一次廉价的前置判断（没有吸附中的窗口时
    /// 连显示器工作区都不必查）。
    pub fn is_docked(&self) -> bool {
        self.docked
    }

    /// 这个窗口**现在占的矩形**（屏幕物理像素）。
    ///
    /// 创建新弹窗时装配层拿它凑"别人占了哪"，好让新窗口避开它们（堆叠）。
    /// 用 `last_pos` 而不是问窗口：这是"每多开一个窗口就多一次 syscall"的位置，
    /// 而创建时几何一定填过 `last_pos`、之后每次程序摆位也都会更新它。
    /// 万一是 `None`（理论上只在几何里没给坐标时出现），才退回问一次窗口。
    pub fn occupied_rect(&self) -> Rect {
        let (x, y) = self.last_pos.unwrap_or_else(|| self.window.position());
        Rect {
            x,
            y,
            w: self.phys.0.max(1),
            h: self.phys.1.max(1),
        }
    }

    /// 装配层换了主窗口配色。**只有"跟随主窗口"的窗口要动。**
    ///
    /// 返回是否需要重绘。有自己配色的窗口完全不受影响 —— 这正是
    /// "不影响主窗口、也不被主窗口影响"这条需求的反方向。
    pub fn set_main_scheme(&mut self, scheme: Scheme) -> bool {
        let changed = self.main_scheme != scheme;
        self.main_scheme = scheme;
        if !changed || !palette::follows_main(&self.base_color) {
            return false;
        }
        self.scheme = palette::resolve_scheme(&self.base_color, &self.main_scheme);
        true
    }

    /// 装配层换了外框圆角（使用者切换「外圈玻璃」）。
    ///
    /// 返回是否需要重绘。**系统偏好与面板半径一起换**：只换面板半径的话，
    /// DWM 仍按旧偏好裁角，四个角会露出桌面（反之则是角上多出一块直角）。
    ///
    /// 只在真的变了才调 DWM：那是窗口属性，不该每帧设。
    pub fn set_corners(&mut self, corners: CornerStyle) -> bool {
        if corners == self.corners {
            return false;
        }
        self.corners = corners;
        acrylic::set_round_corners(self.window.hwnd(), corners.rounded);
        true
    }

    /// 配色按钮：开关选色浮层。
    ///
    /// 布局在这里算一次并**存下来**，绘制与命中共用同一份 —— 两处各算一次
    /// 就会出现"点中的和画出来的不是同一格"。
    pub fn toggle_picker(&mut self) -> bool {
        match self.picker.take() {
            Some(_) => {
                // 再点一次按钮 = 关掉。这是"浮层可以被同一个人两个动作关掉"
                // 里的第一个（另两个：点浮层外面、Esc）。
                self.hover_swatch = None;
                self.hover_follow = false;
                true
            }
            None => {
                self.picker = Some(PaletteLayout::compute(self.layout.content));
                true
            }
        }
    }

    /// 关掉选色浮层（点别处 / Esc / 失去焦点）。返回是否真的关掉了。
    pub fn close_picker(&mut self) -> bool {
        if self.picker.take().is_none() {
            return false;
        }
        // 悬停状态跟着作废：留着它会让下次打开时有一个"凭空的"高亮格子。
        self.hover_swatch = None;
        self.hover_follow = false;
        true
    }

    /// 选中一个基础色。`None` = 跟随主窗口。
    ///
    /// 立即换掉本窗口的整套配色（所见即所得），并回报装配层落盘。
    /// **不碰主窗口**：`main_scheme` 只读。
    fn pick_color(&mut self, hex: Option<&str>) -> bool {
        let next = hex.unwrap_or("").to_string();
        if next == self.base_color {
            return false;
        }
        self.base_color = next;
        self.scheme = palette::resolve_scheme(&self.base_color, &self.main_scheme);
        crate::diag::log(&format!(
            "弹窗配色: {} ({})",
            if self.base_color.is_empty() {
                "跟随主窗口"
            } else {
                &self.base_color
            },
            self.tab_key
        ));
        // 落盘靠装配层（状态归它管，见 `state.rs` 的分工）；
        // `persistable()` 会把新的 `base_color` 一起交回去。
        crate::popout::report::report_changed(&self.tab_key);
        true
    }

    /// 选色浮层开着时，处理一次点击。返回 `Some(是否需要重绘)`；
    /// `None` 表示"浮层没开，这次点击与它无关"。
    ///
    /// 抽成一个函数是因为"点浮层里 = 选色 / 点浮层外 = 关闭"必须确定性地
    /// 先于其它点击逻辑（方格启动、标题栏拖动）判一次，否则浮层开着时
    /// 点空白会顺手启动一个程序。
    fn on_picker_click(&mut self, lx: i32, ly: i32) -> Option<bool> {
        let layout = self.picker.as_ref()?;
        if layout.swatch_at(lx, ly).is_some() || layout.is_follow(lx, ly) {
            let hex = layout
                .swatch_at(lx, ly)
                // 用 `palette_hex` 而不是 `BASE_PALETTE[i]`：格子是按过滤后的
                // 颜色列表摆的，直接索引原表会在有项解析失败时错位。
                .and_then(crate::config::theme::palette_hex);
            let changed = self.pick_color(hex);
            // 选完**不关浮层**：使用者多半想连着试几个颜色，这与主窗口
            // 设置面板"点色块不关闭面板"是同一条理由。
            return Some(changed);
        }
        if layout.contains(lx, ly) {
            // 点在浮层内部的空白处：什么也不做，但也**不**穿透到下面的方格。
            return Some(false);
        }
        // 点在浮层外面 = 关闭意图，并吃掉这次点击（不启动方格、不拖窗口）。
        Some(self.close_picker())
    }

    /// 窗口还活着吗。
    ///
    /// 装配层在落盘前问它：已销毁的窗口读位置会得到 `(0,0)`，那是**合法坐标**，
    /// 存进去就是"下次启动窗口出现在屏幕左上角"。销毁有它自己的回报通道
    ///（见 `popout::report`），所以这里只需拒绝写入。
    pub fn is_alive(&self) -> bool {
        self.window.is_alive()
    }

    /// 登记到消息循环（**不进循环**：一个线程只有一个循环）。
    ///
    /// 先把裸指针拿出来再调用，避开"同时借用 `self.window` 与 `self`"。
    pub fn attach(&mut self) {
        let ptr: *mut dyn Handler = self;
        self.window.attach_ptr(ptr);
    }

    /// 请求关闭。
    pub fn close(&self) {
        self.window.close();
    }

    /// 显示窗口。
    ///
    /// **必须显式调用**：`Window::create` 只创建不显示（`WS_POPUP` 里没有
    /// `WS_VISIBLE`），主窗口那边也是在创建后自己调 `show()`。
    /// 漏掉这一步的表现是"点了菜单什么都没出现"，而不是报错。
    pub fn show(&self) {
        self.window.show();
    }

    /// 按主窗口的显隐同步这个窗口。**钉住的窗口完全不受影响。**
    ///
    /// 返回是否真的动了（调用方据此决定要不要重画）。
    /// 用 `is_visible` 判一次再动：`ShowWindow` 每次都发一堆消息，
    /// 而主窗口显隐是低频动作，这里省下来的是无谓的消息风暴。
    pub fn sync_visibility(&self, main_visible: bool) -> bool {
        if self.pinned {
            // 需求："钉的作用就只是留在桌面，不随主体最小化。"
            //
            // **但要放行"显示"**：窗口可能是创建了但还没显示（启动恢复先建窗口、
            // 显隐交给 `set_main_visible` 那条唯一链决定），那时只忽略"隐藏"
            // 会让它永远出不来 —— 从"主窗口收进托盘"回到"主窗口显示"之后，
            // 钉住的窗口就没机会再被显示一次。隐藏仍然忽略（这才是钉住的用处）。
            if !main_visible {
                return false;
            }
        } else if self.window.is_visible() == main_visible {
            // 状态已经对了就不动：`ShowWindow` 每次都发一堆消息，
            // 而显隐是低频动作，这里省下来的是无谓的消息风暴。
            return false;
        }
        if main_visible {
            if self.window.is_visible() {
                return false;
            }
            self.window.show();
        } else {
            self.window.hide();
        }
        true
    }

    fn to_logical(&self, x: i32, y: i32) -> (i32, i32) {
        let s = self.window.scale();
        if s <= 0.0 {
            (x, y)
        } else {
            ((x as f32 / s) as i32, (y as f32 / s) as i32)
        }
    }
}

/// 物理客户区尺寸 → 逻辑（DIP）尺寸。
///
/// 抽成一处是因为创建时、每次 `Resized`、每次 `DpiChanged` 都要做同一次换算。
/// 三处各写一份的后果只在非 100% 缩放下才暴露，而且症状很隐蔽：
/// 布局按物理算、绘制按 DIP 算，方格整体偏大、右下角被裁掉一截，
/// 看起来像"布局算法错了"，其实是单位没统一。
fn logical_size(phys_w: i32, phys_h: i32, scale: f32) -> (i32, i32) {
    if scale <= 0.0 {
        return (phys_w.max(1), phys_h.max(1));
    }
    (
        (phys_w as f32 / scale).round() as i32,
        (phys_h as f32 / scale).round() as i32,
    )
}

impl Handler for Popout {
    fn on_event(&mut self, event: Event) -> bool {
        match event {
            Event::Resized { width, height } => {
                let dpi = self.window.dpi();
                // 交换链与布局必须跟着一起变，否则画面会被拉伸。
                if self
                    .renderer
                    .resize(width.max(1), height.max(1), dpi as f32)
                    .is_err()
                {
                    return false;
                }
                // 布局用事件里的尺寸重算：`Resized` 带的就是物理像素，
                // 而 DIP 换算统一在 `relayout` 里做。
                //
                // 图标尺寸取 `content.icon_size`（主窗口那侧的真值）而不是
                // 上一次的方格边长：后者是被内容区宽度夹紧过的结果，
                // 拿它当输入会让"窗口缩窄再放宽"之后方格回不到原尺寸。
                self.relayout((width as i32, height as i32));
                self.phys = (width.max(1) as i32, height.max(1) as i32);
                true
            }
            // DPI 变了（把窗口拖到另一块缩放不同的显示器上）。
            //
            // 两件事都要做：布局的 DIP 尺寸变了（物理客户区没变但缩放系数变了），
            // 位图要按新的物理边长重新提取（键里带着尺寸，所以是另一批键）。
            // `relayout` 一次把两件都做了。
            Event::DpiChanged { .. } => {
                self.phys = self.window.client_size();
                self.relayout(self.phys);
                true
            }
            Event::SizeMove { entering } => {
                self.resizing = entering;
                if entering {
                    // 记下"用户开始拖了"以及拖之前的几何。
                    // 只有用户真的抓标题栏/边框才会有这条事件（见
                    // `on_user_size_move_end` 的说明），所以这里不必再判来源。
                    let (x, y) = self.window.position();
                    let (w, h) = self.phys;
                    self.drag_start = Some((x, y, w, h));
                } else {
                    self.on_user_size_move_end();
                }
                true
            }
            Event::MouseMove { x, y } => {
                let (lx, ly) = self.to_logical(x, y);
                let mut redraw = false;
                // 浮层开着时，悬停只归它 —— 它下面的标题栏按钮不该有悬停反馈
                //（浮层铺了一层压暗，按钮看起来已经是"背景"了）。
                //
                // 先算出结果再写字段：`as_ref()` 借着 `self.picker`，
                // 同一个作用域里赋值会被借用检查器拦下。
                let picker_hover = self
                    .picker
                    .as_ref()
                    .map(|pl| (pl.swatch_at(lx, ly), pl.is_follow(lx, ly)));
                if let Some((sw, fo)) = picker_hover {
                    if (sw, fo) != (self.hover_swatch, self.hover_follow) {
                        self.hover_swatch = sw;
                        self.hover_follow = fo;
                        redraw = true;
                    }
                    return redraw;
                }
                let h = self.layout.button_at(lx, ly);
                if h != self.hover {
                    self.hover = h;
                    redraw = true;
                }
                // 按下之后移动超过容差 = 这不是一次单击（用户可能在拖窗口，
                // 或者只是手抖）→ 取消这次启动候选。
                //
                // 不做这一步的症状：想微调窗口位置，手一抖就启动了一个程序。
                if let Some(p) = self.press {
                    let (dx, dy) = (lx - p.start.0, ly - p.start.1);
                    if dx.abs() > CLICK_TOLERANCE || dy.abs() > CLICK_TOLERANCE {
                        self.press = None;
                    }
                }
                redraw
            }
            Event::MouseLeave => {
                // 鼠标离开窗口：按下的候选必须作废。
                //
                // 弹窗没有 `SetCapture`，在窗口外松手时收不到 `MouseUp`，
                // 状态会一直挂着 —— 那样下一次松手（哪怕点的是别的东西）
                // 会莫名启动一个程序。这是主窗口"拖拽生命周期绝不跨过两次按下"
                // 的同一条保险，这里更简单：离开即作废。
                self.press = None;
                if self.hover.is_some() {
                    self.hover = None;
                    return true;
                }
                false
            }
            Event::MouseDown { x, y } => {
                let (lx, ly) = self.to_logical(x, y);
                // 配色按钮**最优先**：它就是浮层的开关，开着时再点它就该是"关"。
                // （浮层摆在内容区里，盖不到标题栏，所以这里必须显式判它，
                //   否则会被下面的"点浮层外面 = 关闭"分支吃掉，看起来像是
                //   "点按钮关不掉"。）
                if self.layout.button_at(lx, ly) == Some(TitleButton::Color) {
                    self.press = None;
                    return self.toggle_picker();
                }
                // 浮层开着时，这次点击先归它：点色块 = 选色、点外面 = 关闭。
                // **必须吃掉**，否则点空白处会顺手启动一个方格里的程序。
                if let Some(redraw) = self.on_picker_click(lx, ly) {
                    return redraw;
                }
                match self.layout.button_at(lx, ly) {
                    // 关闭：`WM_CLOSE` 走 `on_close_request`。
                    // 本窗口不是主窗口，所以销毁它**不会**结束消息循环
                    //（见 `window.rs` 的 WM_DESTROY）。
                    Some(TitleButton::Close) => {
                        self.press = None;
                        self.window.close();
                        false
                    }
                    // 三个开关按钮。改色是下一步（独立配色），现在点了不响应 ——
                    // 不做假的视觉反馈。
                    Some(TitleButton::Dock) => {
                        self.press = None;
                        self.toggle_dock()
                    }
                    Some(TitleButton::Pin) => {
                        self.press = None;
                        self.toggle_pin()
                    }
                    // `Color` 在上面就返回了（它是浮层开关，要先于浮层的
                    // "点外面即关闭"判定）。这里兜住其余情况。
                    Some(_) => {
                        self.press = None;
                        false
                    }
                    // 点在方格（或它下面的名称）上：**先不启动**，
                    // 记下来等松手时再判断（判据见 `press` 与 `CLICK_TOLERANCE`）。
                    None => {
                        self.press =
                            self.layout
                                .tile_index_at(lx, ly, self.content.items.len())
                                .map(|index| TilePress {
                                    index,
                                    start: (lx, ly),
                                });
                        // 弹窗这边按下不改任何视觉状态（没有选中集、也没有按下态），
                        // 所以不必请求重绘。
                        false
                    }
                }
            }
            Event::MouseUp { x, y, inside } => {
                // 一次单击 = 按下与松手之间没怎么移动过、松手时还在窗口里、
                // 而且**落在同一格**上。任何一条不满足都不启动 ——
                // 拖窗口前后那两下按下不该顺手启动一个程序。
                //
                // `inside` 目前**恒为真**：窗口没有 `SetCapture`，鼠标在窗口外
                // 松手时我们根本收不到 `MouseUp`（那种情况由 `MouseLeave` 作废）。
                // 这里仍然判一次，是为了让判据本身完整、不依赖平台层当前的实现 ——
                // 哪天给弹窗加上捕获，这条就不会突然失效。
                let Some(p) = self.press.take() else {
                    return false;
                };
                if !inside {
                    return false;
                }
                let (lx, ly) = self.to_logical(x, y);
                if self
                    .layout
                    .tile_index_at(lx, ly, self.content.items.len())
                    != Some(p.index)
                {
                    return false;
                }
                if let Some(item) = self.content.items.get(p.index) {
                    super::launch::launch(item);
                }
                // 启动不改变本窗口的任何像素，所以不请求重绘。
                false
            }
            // Esc 关掉选色浮层。
            //
            // 常量本地定义：为一个虚拟键码去开 `KeyboardAndMouse` 那一整个
            // feature 不划算，而**必须**是常量而不是 `let` 绑定 ——
            // 变量绑定出现在 match 分支里会变成"捕获所有值"的模式，
            // 后面所有分支静默不可达（`platform/window.rs` 里踩过一次）。
            Event::KeyDown(vk) => {
                const VK_ESCAPE: u32 = 0x1B;
                if vk == VK_ESCAPE && self.picker.is_some() {
                    return self.close_picker();
                }
                false
            }
            // 失去激活 = 用户点了别的窗口 → 浮层收起。
            // 与主窗口右键菜单靠同一个事件关闭是同一条理由：点在别的窗口上时，
            // 消息根本不会送到我们这里，"点外部关闭"是等不到的。
            Event::FocusLost => self.close_picker(),
            _ => false,
        }
    }

    fn hit_test(&self) -> HitTest {
        let mut ht = HitTest {
            title_bar_height: self.layout.title_bar.h,
            resize_border: crate::popout::layout::RESIZE_BORDER,
            ..Default::default()
        };
        // 标题栏上的四个按钮要排除掉，否则点它们会被当成"按住标题栏拖动窗口"。
        for r in self.layout.buttons.iter() {
            ht.exclude(*r);
        }
        ht
    }

    fn min_size(&self) -> (i32, i32) {
        (crate::popout::state::MIN_W, crate::popout::state::MIN_H)
    }

    fn paint(&mut self) {
        if self.renderer.begin().is_err() {
            return;
        }
        let ctx = self.renderer.context().clone();
        // 收异步结果 → 转 D2D 位图 → 进缓存。**必须在绘制之前做完**：
        // `insert` 会触发 LRU 淘汰，边画边淘汰可能把正要画的那张位图释放掉
        //（主窗口 `App::draw` 里有同一条规则的说明）。
        self.icons.poll_and_insert(&ctx);
        // **文字引擎也要 ensure。**
        //
        // 它的笔刷是懒创建的，没建之前 `TextEngine::draw` 会**静默返回**
        //（`let Some(brush) = self.brush.clone() else { return Ok(()) }`）——
        // 一个字都不画，也不报错、不留日志。主窗口在 `App::draw` 里调过这一句，
        // 这个窗口当时漏了，症状就是"图标都在，但标题和名称一个字都没有"。
        //
        // 失败不致命（与漏调时的表现相同：没有文字），所以这里不往上抛。
        let _ = self.text.ensure(&ctx);
        if self.painter.ensure(&ctx).is_ok() {
            // 只给**看得见**的格子取图标：看不见的既不该请求、也不该占缓存。
            // 判据与绘制共用 `visible_tiles`，两处不会算出不同的范围。
            let visible = self.layout.visible_tiles(self.content.items.len());
            let bitmaps = self.icons.collect(&self.content.items, visible);
            let states = ButtonStates {
                docked: self.docked,
                pinned: self.pinned,
            };
            let _ = self.painter.draw(
                &ctx,
                &mut self.text,
                &self.layout,
                &self.scheme,
                &self.title,
                self.hover,
                states,
                &self.content.items,
                &bitmaps,
                // 面板半径由装配层算好传来（规则见 `Appearance::corner_style`）
                self.corners.radius,
            );

            // ---- 选色浮层（**最后画 = 最上层**）----
            //
            // 几何直接取存下来的那一份（与命中判定同一份），绘制不做几何决策。
            // 借用说明：`pl` 借 `picker`，`&mut self.text` 是另一个字段，
            // `&self.painter` 又是一个 —— 三者互不相干，借用检查器放行。
            if let Some(pl) = self.picker.as_ref() {
                let view = PaletteView {
                    layout: pl,
                    labels: &self.palette_text,
                    scheme: &self.scheme,
                    main_scheme: &self.main_scheme,
                    // 判据只有一处（`theme::palette_index`），与主窗口一致：
                    // 只有正好等于某个色板项才高亮。
                    selected: crate::config::theme::palette_index(&self.base_color),
                    hover_swatch: self.hover_swatch,
                    hover_follow: self.hover_follow,
                };
                let _ = self.painter.draw_palette(&ctx, &mut self.text, &view);
            }
        }
        let _ = self.renderer.end(if self.resizing { 0 } else { 1 });
        let _ = self.composer.commit();
    }

    fn on_close_request(&mut self) -> CloseAction {
        // 弹出窗口的"关闭"就是销毁它自己。
        // 注意这里**不能**返回 `Exit` 之外的语义：`Exit` 在本窗口上
        // 只意味着 DestroyWindow，主窗口不受影响。
        CloseAction::Exit
    }

    /// 窗口即将销毁 → **回报装配层**。
    ///
    /// 不回报的话，弹窗被自己的 ✕ 关掉之后 `App::popouts` 里会留着一个死窗口：
    /// 右键菜单仍然显示「关闭子窗口」、`state` 里的 `open` 也还是 `true`。
    /// 详见 `popout::report`。
    ///
    /// 放在这里而不是 `on_close_request`：后者只有走 `WM_CLOSE` 才被调用，
    /// 而销毁覆盖所有路径。放这里也顺带保证了**回报时 `self` 还活着** ——
    /// 装配层要等到下一次收尾点才把 `Box<Popout>` 摘掉。
    fn on_destroyed(&mut self) {
        crate::popout::report::report_closed(&self.tab_key);
    }

    fn wants_tray(&self) -> bool {
        false
    }
}
