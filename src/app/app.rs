//! `App`：把各层接起来的那一层。
//!
//! 职责边界：
//! * 收到 [`Event`] → 改自己的状态 → 返回"要不要重绘"；
//! * [`Handler::paint`] 被调用时，把当前状态交给渲染层画一遍；
//! * 退出时把窗口几何写回 `state.toml`。

use std::sync::mpsc::{self, Receiver};

use crate::config::model::{TabDef, TabOpen};
use crate::config::theme::{Backdrop, Scheme};
use crate::config::{self, AppConfig, SchemeBook};
use crate::error::Result;
use crate::i18n::I18n;
use crate::model::Library;
use crate::paths;
use crate::platform::acrylic;
use crate::platform::hittest::{HitTest, RectI};
use crate::platform::icon_loader::{Key, Loader};
use crate::platform::window::{CloseAction, Event, Geometry, Handler, Window};
use crate::render::composer::Composer;
use crate::render::icon_cache::{DEFAULT_BUDGET as ICON_BUDGET, IconCache};
use crate::render::painters::menu::{Menu, MenuCommand, MenuItem, MenuPainter, MenuTarget};
use crate::render::painters::{
    CaptionPainter, GridPainter, PanelPainter, SettingsItem, SettingsPainter, SidebarPainter,
};
use crate::render::{Role, Renderer, TextEngine};
use crate::shell::com::ComGuard;
use crate::shell::scan::ScanReport;
use crate::state::{self, StateStore};
use crate::ui::layout::{
    CaptionButton, LABEL_GAP, Layout, LayoutInput, SIDEBAR_WIDTH, TILE_PADDING,
};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;

/// 预先解析好的界面文案。
///
/// 为什么不在绘制时直接调 `i18n.t()`：`t()` 返回的 `&str` **借用着 `I18n`**，
/// 而绘制同时要可变借用整个 `App`，借用检查器不允许。
/// 启动与切换语言时一次性解析成拥有所有权的字符串，之后绘制不再查表。
struct Strings {
    title: String,
    tab_labels: Vec<String>,
    settings_title: String,
    setting_outer_glass: String,
    setting_always_on_top: String,
    setting_hide_after_launch: String,
    setting_autostart: String,
    /// 色板区的标题
    setting_theme_custom: String,
    setting_hotkey: String,
    /// 设置面板右下角「使用说明」按钮的文字
    settings_help: String,
    /// 底部按钮：退出程序 / 关于更新
    settings_exit_program: String,
    settings_about_update: String,
}

/// 只扫程序自己的快捷方式目录。
///
/// 系统的开始菜单 / 桌面来源全部关掉 —— 见 `on_files_dropped` 的说明。
fn own_sources(dir: Option<std::path::PathBuf>) -> crate::config::model::Sources {
    crate::config::model::Sources {
        user_start_menu: false,
        common_start_menu: false,
        user_desktop: false,
        public_desktop: false,
        extra_dirs: dir.into_iter().collect(),
        recursive: false,
        watch: false,
    }
}

/// 把一个 `.lnk` 复制进程序目录。同名时自动加序号，不覆盖已有条目。
///
/// 直接覆盖会造成"拖了两次同一个快捷方式，第一条就没了"，
/// 而用户显然期望两条并存（或至少不该丢东西）。
fn copy_shortcut_in(
    src: &std::path::Path,
    dest_dir: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "shortcut".into());

    let mut dest = dest_dir.join(format!("{stem}.lnk"));
    let mut n = 2;
    while dest.exists() {
        dest = dest_dir.join(format!("{stem} ({n}).lnk"));
        n += 1;
        // 极端情况下（同名几百个）仍不放弃，但加个上限避免死循环
        if n > 999 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "同名快捷方式过多",
            ));
        }
    }
    std::fs::copy(src, &dest)?;
    Ok(dest)
}

/// 就地编辑的目标。
enum EditTarget {
    /// 方格条目。存**覆盖表的键**（`.lnk` 完整路径）而不是下标：
    /// 编辑期间集合可能变化（拖入、重新扫描），下标会错位到别的条目上，改名就改错了对象。
    Tile(String),
    /// 侧栏选项卡下标。
    ///
    /// 这里可以用下标：选项卡集合只在用户主动增删时变化，
    /// 而编辑期间不会发生那种操作。
    Tab(usize),
    /// 标题栏左上角的标题文字。全局只有一个，所以不需要附加标识。
    Title,
    /// 新建网址时临时的输入框。它不是任何已有条目，提交后才会创建条目。
    NewUrl,
}

/// 就地编辑中的文本框。
struct Editing {
    target: EditTarget,
    /// 当前文本
    text: String,
    /// 光标位置，按 **char** 计数。
    ///
    /// 不能用字节：中文一个字 3 字节，按字节记光标位置会切断字符 ——
    /// 轻则乱码，重则 `String::insert` 直接 panic。
    caret: usize,
}

/// 方格拖拽（把条目拖到侧栏选项卡上改分组）。
///
/// 判断"是不是在拖"靠的是**移动距离**而不是按下时间，理由见
/// [`TILE_DRAG_THRESHOLD`]。
struct TileDrag {
    /// 按下时命中的覆盖表键。
    ///
    /// 存键而不是下标：拖拽期间界面可能重算（悬停、重绘、甚至一次重新扫描），
    /// 下标一旦漂移，松手时改的就是**另一个条目**的分组。
    key: String,
    /// 按下时命中的可见下标（用于未越阈时启动）。
    index: usize,
    /// 按下的起点（逻辑坐标）
    start: (i32, i32),
    /// 是否已经越过阈值、真的进入拖拽。
    ///
    /// 必须等鼠标移动超过阈值才算真的进入拖拽，
    /// 否则每一次点击启动都会顺带被当成拖拽。
    active: bool,
    /// 拖拽期间鼠标所指的**插入位置**（可见条目下标，`0..=visible.len()`）。
    ///
    /// 与 `tile_drop`（选项卡落点）互斥：鼠标要么在侧栏选项卡上（那是改分组），
    /// 要么在网格里（那是改排序），不可能同时成立，所以两者一起算一次就好。
    /// `None` = 当前两者都不是，松手时什么都不做。
    ///
    /// 存**下标**而不是"目标条目的键"：插入位置本来就是"第几道缝"，
    /// 用键去表达还得区分"插在它前面还是后面"，反而多一层换算。
    /// 排序在松手时一次性完成，中途视图不会重排，下标不存在漂移问题。
    over_index: Option<usize>,
}

/// 空白处按住拖出来的选框（框选多选）。
///
/// 只在**没有命中任何控件**的空白处起框（见 `App::on_mouse_down`）：
/// 方格上按下是"拖拽排序 / 单击启动"，那里不能同时兼作框选的起点。
struct Marquee {
    /// 起点（逻辑坐标）
    start: (i32, i32),
    /// 当前点（逻辑坐标）
    current: (i32, i32),
    /// 按下那一刻的选中集。
    ///
    /// 框选过程中每一帧的选中集都是「**这份基线** ∪ 框内条目」。
    /// 必须留基线、而不是往选中集里追加：追加的话框划过再划回来，
    /// 已经划中的格子会一直亮着，没法"取消划中" —— 而所见即所得
    /// 正是框选唯一的价值。
    ///
    /// 基线同时也承担了 Ctrl 的语义：按下时按着 Ctrl 就以现有选择为基线
    ///（追加），否则以空集为基线（替换）。
    base: std::collections::HashSet<String>,
}

impl Marquee {
    /// 归一化后的选框矩形（拖拽方向任意，要把左上/右下算回来）。
    fn rect(&self) -> crate::platform::hittest::RectI {
        crate::platform::hittest::RectI::new(
            self.start.0.min(self.current.0),
            self.start.1.min(self.current.1),
            (self.start.0 - self.current.0).abs(),
            (self.start.1 - self.current.1).abs(),
        )
    }

    /// 是否只是"点了一下"，而不是真的划出了框。
    ///
    /// 用几像素的阈值区分：低于它就不当框选处理。否则在方格之间点一下，
    /// 那个 0×0 的框会与紧邻的方格相切，于是"点空白"顺带选中了旁边一条，
    /// 用户会觉得莫名其妙。
    fn is_click(&self) -> bool {
        (self.start.0 - self.current.0).abs() < MARQUEE_MIN
            && (self.start.1 - self.current.1).abs() < MARQUEE_MIN
    }
}

/// 判定"这是一次框选"所需的最小拖动距离（逻辑像素）。与方格拖拽同一量级。
const MARQUEE_MIN: i32 = 3;

/// 使用说明的文件名。落在**程序同目录**（便携布局）。
const HELP_FILE: &str = "使用说明.txt";

/// 内嵌的使用说明正文。
///
/// **内嵌而不是"随程序放一个文本文件"**：程序只有单个 exe，
/// 分发时不必额外记事别带文档；首次点「使用说明」时自动写到同目录。
/// 想改内容改 `assets/使用说明.txt` 即可，编译时会被打包进去。
const HELP_TEXT: &str = include_str!("../../assets/使用说明.txt");

/// 帧耗时统计。
///
/// **只在持续重绘时写日志**（每秒最多一行，且那一秒内至少画了 20 帧）。
/// 偶发重绘（悬停换格、图标就绪）不写：写日志本身比画一帧还贵，
/// 平时开着它反而会制造卡顿。而"连续绘帧"基本只有一种成因 ——
/// 用户正在拖动，那正是需要知道"每秒实际能画几帧"的时刻。
///
/// 存在的理由：`--selftest` 里的帧计时测的是**没有合成**的窗口
///（自检不显示窗口，DWM 不参与），所以它只能证明"我们的绘制很便宜"，
/// 证明不了"真实窗口里一帧多少钱"。要定位拖动卡顿，必须在真实条件下量。
struct FrameStat {
    since: std::time::Instant,
    frames: u32,
    total_ms: f32,
    worst_ms: f32,
    moves: u32,
    move_ms: f32,
}

impl FrameStat {
    fn new() -> Self {
        Self {
            since: std::time::Instant::now(),
            frames: 0,
            total_ms: 0.0,
            worst_ms: 0.0,
            moves: 0,
            move_ms: 0.0,
        }
    }

    /// 记一次鼠标移动**及其处理耗时**。
    ///
    /// 一定要连耗时一起记：只有次数的话，无法区分"移动很多但每次都很便宜"
    /// 与"移动不多但每次都很贵"，而这两者的修法完全不同。
    fn note_move(&mut self, ms: f32) {
        self.moves += 1;
        self.move_ms += ms;
    }

    fn sample(&mut self, ms: f32, glass: bool) {
        self.frames += 1;
        self.total_ms += ms;
        if ms > self.worst_ms {
            self.worst_ms = ms;
        }
        // **注意**：本函数只在绘制时被调用，所以窗口长度由
        // `since.elapsed()` 决定，绘制稀时窗口会被拉长。
        // 因此下面必须把窗口时长一起打出来 —— 只报"帧数"会被读成"帧/秒"，
        // 那是错的（第一版就犯了这个错）。
        let secs = self.since.elapsed().as_secs_f32();
        if secs < 1.0 {
            return;
        }
        // 抽屉门槛：窗口里至少要有几帧才值得记，否则空闲时也会每秒写一行。
        if self.frames >= 10 {
            let f = self.frames as f32;
            let m = self.moves as f32;
            diag(&format!(
                "帧统计: 外圈玻璃={} | 窗口 {secs:.2}s | 绘制 {f} 帧（{:.0} fps）平均 {:.2}ms 最慢 {:.2}ms 合计 {:.0}ms | \
                 鼠标移动 {m} 次（{:.0}/s）处理平均 {:.3}ms 合计 {:.0}ms | 两者合计占窗口 {:.0}%",
                if glass { "开" } else { "关" },
                f / secs,
                self.total_ms / f,
                self.worst_ms,
                self.total_ms,
                m / secs,
                if m > 0.0 { self.move_ms / m } else { 0.0 },
                self.move_ms,
                (self.total_ms + self.move_ms) / (secs * 1000.0) * 100.0
            ));
        }
        self.since = std::time::Instant::now();
        self.frames = 0;
        self.total_ms = 0.0;
        self.worst_ms = 0.0;
        self.moves = 0;
        self.move_ms = 0.0;
    }
}

/// 方格拖拽的启动阈值（逻辑像素）。与侧栏选项卡排序用的是同一个量级。
///
/// 没有它的话，"按下即拖拽"会把**每一次普通单击**都吞掉 ——
/// 启动动作在抬起时才执行，用户点一下图标什么都不会发生。
/// 阈值也不能太大：超过 4px 才承认拖拽的话，手稳的用户会以为拖拽坏了。
const TILE_DRAG_THRESHOLD: i32 = 4;

/// 诊断日志：**GUI 模式下没有控制台，`println!` 的输出会被直接丢掉**，
/// 所以排查"卡在哪一步"必须写文件。记录相对进程启动的毫秒数，便于看间隔。
///
/// 只在动作路径上调用（启动、打开位置），每帧都调会拖慢绘制。
fn diag(msg: &str) {
    use std::io::Write;
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    let t0 = T0.get_or_init(std::time::Instant::now);
    let Ok(dir) = crate::paths::data_dir() else {
        return;
    };
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("诊断.log"))
    else {
        return;
    };
    let _ = writeln!(f, "[{:>7} ms] {msg}", t0.elapsed().as_millis());
}

/// 由覆盖表键（`.lnk` 完整路径）推出默认显示名：文件名去掉 `.lnk`。
///
/// 这也正是资源管理器里重命名时看到的名字，符合直觉。
fn shortcut_display_name(key: &str) -> String {
    std::path::Path::new(key)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 把 **char 下标**换算成**字节下标**。
///
/// 所有涉及光标位置的字符串操作都必须经过它。`String::insert` / `remove`
/// 接受的是字节下标，而中文一个字占 3 字节 —— 直接把 char 下标当字节用，
/// 会切在字符中间：轻则乱码，重则 `insert` 因不在字符边界而 panic。
///
/// 下标超出范围时夹到末尾（而不是 panic）：光标位置来自按键序列，
/// 文本一旦被别处改动就可能越界，为此崩溃不值得。
fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

/// 由网址推出一个简短、可作文件名的条目名。
///
/// 取域名部分并去掉 `www.`：整条 URL 当文件名既长又含 `/` `:` 等非法字符。
fn url_short_name(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.strip_prefix("www.").unwrap_or(host);
    let trimmed = host.trim();
    if trimmed.is_empty() {
        "网址".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 把文件名里的非法字符换成下划线，得到一个可安全用作文件名的串。
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii() {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect()
}

/// 设置面板里的项目数。顺序必须与点击处理里的 match 分支一致。
const SETTINGS_ITEM_COUNT: usize = 5;

impl Strings {
    fn resolve(i18n: &I18n, tabs: &[TabDef]) -> Self {
        let tr = |key: &str| i18n.t(key).to_string();
        Self {
            // 空则由调用方用 `General::title` 覆盖；这里先给多语言的默认值。
            title: i18n.t("app.title").to_string(),
            tab_labels: tabs.iter().map(|t| t.label(&tr)).collect(),
            settings_title: i18n.t("settings.title").to_string(),
            setting_outer_glass: i18n.t("settings.outer_glass").to_string(),
            setting_always_on_top: i18n.t("settings.always_on_top").to_string(),
            setting_hide_after_launch: i18n.t("settings.hide_after_launch").to_string(),
            setting_autostart: i18n.t("settings.autostart").to_string(),
            setting_theme_custom: i18n.t("settings.theme_custom").to_string(),
            setting_hotkey: i18n.t("settings.hotkey").to_string(),
            settings_help: i18n.t("settings.help").to_string(),
            settings_exit_program: i18n.t("settings.exit_program").to_string(),
            settings_about_update: i18n.t("settings.about_update").to_string(),
        }
    }
}

/// 应用主体。
pub struct App {
    // COM 守卫必须最后析构：字段按声明顺序析构（先声明者后析构），
    // 所以放在最前面，才能保证它晚于下面所有 COM 对象释放。
    _com: ComGuard,

    window: Window,
    renderer: Renderer,
    /// 必须持有：合成器一旦释放，视觉树与窗口的关联就断了，画面会直接消失。
    composer: Composer,

    panel: PanelPainter,
    caption: CaptionPainter,
    sidebar: SidebarPainter,
    grid: GridPainter,
    settings: SettingsPainter,
    menu_painter: MenuPainter,
    text: TextEngine,
    /// 图标位图缓存，按字节预算做 LRU 淘汰
    icons: IconCache,
    /// 图标提取线程
    loader: Loader,

    cfg: AppConfig,
    /// 配置仓库。改选项卡（重命名/换图标/删除）时要写回 `config.toml`。
    cfg_store: crate::config::TomlStore<AppConfig>,
    /// 国际化表。改选项卡名之后要重新解析 `Strings`，所以得留着它。
    i18n: I18n,
    /// 当前配色的副本。
    ///
    /// 不存 `&Scheme` 是因为绘制时要同时借用配色与 `&mut App` 的其他字段；
    /// 一份几百字节的副本换来借用关系的彻底简化，很划算。
    scheme: Scheme,
    state: StateStore,
    strings: Strings,
    layout: Layout,

    /// 已扫描到的全部快捷方式。初始为空，后台线程扫完后填入。
    library: Library,
    /// 后台扫描的接收端。取到结果后置空，表示"已就绪"。
    scan_rx: Option<Receiver<ScanReport>>,

    tabs: Vec<TabDef>,
    active_tab: usize,
    /// 搜索词。搜索框尚未绘制，先把状态与过滤链路留好。
    query: String,
    /// 当前选项卡 + 搜索词命中的条目在 `library` 中的下标。
    visible: Vec<usize>,
    /// 当前图标边长（`Ctrl+滚轮` 会改它）。
    icon_size: u32,
    /// 内容区纵向滚动量（DIP）。布局会把它夹到合法范围并回写。
    scroll: f32,

    /// 实际生效的背景材质，可能低于配置值（系统不支持时降级）。
    backdrop: Backdrop,
    /// 窗口是否最大化。缓存它是因为命中测试在鼠标移动时高频触发，
    /// 不能每次都去 syscall 问 `IsZoomed`。
    maximized: bool,

    hover_caption: Option<CaptionButton>,
    hover_tab: Option<usize>,
    hover_tile: Option<usize>,
    hover_settings: bool,
    /// 设置面板是否展开
    settings_open: bool,
    /// 鼠标悬停在设置面板的第几项
    hover_setting_row: Option<usize>,
    /// 鼠标悬停在设置面板的关闭按钮
    hover_settings_close: bool,
    /// 鼠标停在设置面板底部第几个按钮上（`None` = 都不在）。
    ///
    /// 用下标而不是三个布尔：三个按钮是同一排平级控件，按下标处理
    /// 命中判断与悬停只写一遍，加/减按钮也不会漏改某处。
    hover_settings_button: Option<usize>,

    /// 用户已经**明确要求退出**（托盘右键菜单的「退出」、或设置面板的
    /// 「退出程序」）。置位后 `on_close_request` 不再把关闭解释成"收进托盘"。
    ///
    /// 需要这个标志，是因为"关闭窗口"在这类常驻托盘的程序里有两种含义：
    /// 点标题栏的 × 是收进托盘，**明确点退出才是真退出**。
    /// 两者最终都走 `WM_CLOSE`，只能靠这个标志区分 ——
    /// 不区分就是先前那个 bug：托盘菜单点「退出」只是把窗口藏了起来。
    exit_requested: bool,

    /// 右键菜单。`Some` 表示菜单正展开。
    ///
    /// 它是**最上层**浮层：绘制最后画，点击最先处理。
    menu: Option<Menu>,
    /// 正在拖动的侧栏按钮下标；`None` 表示没在拖。
    ///
    /// 拖动排序不用 `SetCapture`：侧栏按钮的拖动范围就在窗口内，
    /// 鼠标移出窗口时失去捕获反而能自然取消。而右键菜单必须捕获
    ///（它要在窗口外点击时也能收到消息），两者需求不同。
    dragging_tab: Option<usize>,
    /// 拖动时的落点提示（高亮哪个按钮）。
    drop_tab_target: Option<usize>,

    /// 选中的条目，存覆盖表键（`.lnk` 完整路径）而不是可见下标 ——
    /// 切选项卡 / 重新扫描会让下标漂移，用下标会出现"删掉的不是选中的那个"。
    ///
    /// 键跟着条目本身走，即使中间换过选项卡、重排过顺序也依然指向同一条。
    selected: std::collections::HashSet<String>,
    /// 方格拖拽状态；`None` = 没在拖。
    tile_drag: Option<TileDrag>,
    /// 方格拖拽到侧栏时的落点选项卡下标，只用于绘制高亮。
    ///
    /// 单独一个字段而不是复用 `drop_tab_target`：后者是**选项卡排序**的落点，
    /// 松手时会被重排逻辑读走。共用一个字段的话，一次方格拖拽结束留下的落点
    /// 会让紧接着的一次选项卡点击被当成"拖到这一位"，顺序莫名其妙地变了。
    tile_drop: Option<usize>,

    /// 空白处框选（多选）的进行中状态；`None` = 没在框选。
    ///
    /// 与 `tile_drag` **互斥**：一个从方格上起手、一个从空白处起手，
    /// 不可能同时存在。两者的落点处理也因此互不干扰。
    marquee: Option<Marquee>,

    /// 帧耗时统计（只在持续重绘时写 `诊断.log`，见 [`FrameStat`]）。
    frame_stat: FrameStat,

    /// 累计发起过多少次图标请求（只用于自检判定"是否有永远提取不成功的图标"）。
    ///
    /// 正常情况下每个图标只请求一次、落地后就不再请求，所以这个计数应当很快
    /// 停止增长。若它随帧数线性增长，说明有图标**每帧都在重试** ——
    /// 那会让加载线程一直做白工，并把 UI 一起拖慢。
    icon_requests: u32,

    /// 正在录制全局热键。
    ///
    /// 录制期间按键**优先给录制器**，不能落到就地编辑的输入框里 ——
    /// 两者都从 `WM_KEYDOWN` 取键，没有这个标志就会同时触发。
    recording_hotkey: bool,

    /// 正在就地编辑的文本输入框。`None` 表示没有在编辑。
    editing: Option<Editing>,
    /// 菜单的悬停项。放在 App 上而不是 Menu 里，是为了让 `Menu`
    /// 保持纯数据、绘制时不必可变借用它。
    menu_hover: Option<usize>,
    /// 弹出菜单时的逻辑坐标（即鼠标位置）。
    ///
    /// 二级菜单（"移动到"）要从**这个点**弹出，而不是从父菜单那一行的边缘 ——
    /// 后者会让菜单跳到屏幕另一处，用户的手还停在原地，得重新找。
    menu_anchor: (i32, i32),
    /// 正在拖动窗口尺寸：此时走轻量绘制（反拖影铁律 6）。
    resizing: bool,
}

impl App {
    /// 装配整个应用：读配置与状态 → 建窗口 → 建渲染器 → 应用材质 → 起后台扫描。
    pub fn new() -> Result<Self> {
        // DComposition 需要 COM。STA 与窗口消息循环天然契合。
        let com = ComGuard::sta()?;

        let cfg_store = config::open()?;
        let state_store = state::open()?;
        let cfg = cfg_store.get().clone();
        let saved = state_store.get().clone();

        let schemes = SchemeBook::new(&cfg.schemes);
        let scheme_key = if saved.scheme.trim().is_empty() {
            cfg.appearance.scheme.clone()
        } else {
            saved.scheme.clone()
        };
        let scheme = match crate::config::Color::from_hex(&cfg.appearance.base_color) {
            // 自定义基础色优先：有它就从它派生整套主题。
            Some(base) => config::Scheme::from_base(base),
            // 没配或解析失败就退回命名方案。
            //
            // **解析失败不报错、只回退**是刻意的：配置文件可能被手改坏，
            // 而"配色回退到默认"远比"程序打不开"要好。但也不能静默 ——
            // 所以下面打一行日志，让改错字的人知道为什么没生效。
            None => {
                if !cfg.appearance.base_color.trim().is_empty() {
                    eprintln!(
                        "基础色 {:?} 不是合法的 #RRGGBB，已回退到方案「{}」",
                        cfg.appearance.base_color, scheme_key
                    );
                }
                schemes.resolve(&scheme_key).clone()
            }
        };

        let i18n = I18n::load(&cfg.language.locale, paths::lang_dir().ok().as_deref());

        let tabs = if cfg.tabs.is_empty() {
            TabDef::defaults()
        } else {
            cfg.tabs.clone()
        };
        let mut strings = Strings::resolve(&i18n, &tabs);
        // 配置里有自定义标题就用它，否则保留多语言默认值。
        if !cfg.general.title.trim().is_empty() {
            strings.title = cfg.general.title.clone();
        }

        // 窗口尺寸优先用上次关闭时记住的
        let (width, height) = if saved.window.width > 0 && saved.window.height > 0 {
            (saved.window.width, saved.window.height)
        } else {
            (
                cfg.layout.panel_width as i32,
                cfg.layout.panel_height as i32,
            )
        };

        let always_on_top = saved
            .always_on_top
            .unwrap_or(cfg.general.always_on_top);
        let outer_glass = saved.outer_glass.unwrap_or(cfg.appearance.outer_glass);

        let window = Window::create(
            &strings.title,
            Geometry {
                width,
                height,
                x: saved.window.x,
                y: saved.window.y,
            },
            always_on_top,
        )?;

        let (client_w, client_h) = window.client_size();
        let dpi = window.dpi() as f32;

        // 注册配置里的全局热键。失败只记日志：热键不可用不该让程序打不开。
        if !cfg.general.hotkey.trim().is_empty() {
            if let Err(e) = crate::platform::hotkey::apply(window.hwnd(), &cfg.general.hotkey) {
                eprintln!("注册全局热键「{}」失败: {e}", cfg.general.hotkey);
            }
        }

        // 挂托盘图标。
        //
        // 放在窗口建好之后、进入消息循环之前：拖到更晚的话，
        // 用户在启动期间就点不到托盘；而建窗口之前没有 `hwnd` 可用
        //（托盘回调要靠它把消息送回我们）。
        //
        // 失败只记一行日志：托盘不可用不该让程序打不开。
        if cfg.general.show_tray {
            if let Err(e) = crate::platform::tray::install(window.hwnd()) {
                eprintln!("挂托盘图标失败（最小化将退化为普通最小化）: {e}");
            }
        }
        let (renderer, composer) = Renderer::new(
            window.hwnd(),
            client_w.max(1) as u32,
            client_h.max(1) as u32,
            dpi,
        )?;

        // 外圈玻璃效果关掉时，不向系统申请背景模糊，面板退化为纯半透明。
        let requested = if outer_glass {
            cfg.appearance.backdrop
        } else {
            Backdrop::None
        };
        let backdrop = acrylic::apply(
            window.hwnd(),
            requested,
            scheme.acrylic_tint(cfg.appearance.opacity),
            outer_glass,
        );
        if backdrop != requested {
            eprintln!(
                "背景材质已降级: 配置 {:?} → 实际 {:?}（当前系统不支持所选项）",
                requested, backdrop
            );
        }

        let icon_size = if saved.icon_size > 0 {
            saved.icon_size
        } else {
            cfg.layout.icon_size
        };
        let active_tab = tabs
            .iter()
            .position(|t| t.key == saved.active_tab)
            .unwrap_or(0);

        let (dip_w, dip_h) = renderer.dip_size();
        // 开了发光就要给发光留边距；关掉发光则面板铺满窗口，
        // 窗口外不再有任何透明/发光区域。
        let layout_inset = if outer_glass {
            scheme.glow_radius + 1.0
        } else {
            0.0
        };
        let layout = Layout::compute(
            &LayoutInput {
                width: dip_w,
                height: dip_h,
                panel_inset: layout_inset,
                // 平面模式下连圆角也去掉：圆角外那圈三角区域是透明的，
                // 只要还有圆角，"窗口外完全无透明"就做不到。
                corner_radius: if outer_glass {
                    cfg.appearance.corner_radius
                } else {
                    0.0
                },
                sidebar_width: SIDEBAR_WIDTH,
                tile_gap: cfg.layout.tile_gap,
                columns: cfg.layout.columns,
                icon_size,
                item_count: 0,
                scroll: 0.0,
                settings_rows: SETTINGS_ITEM_COUNT,
            },
            tabs.len(),
        );

        let maximized = window.is_maximized();

        // ---- 后台扫描 ----
        //
        // 扫的是**程序自己的 shortcuts 目录**，不是系统开始菜单/桌面。
        // 按需求"自动扫描先不做、全部手动添加"：条目只能由用户拖进来，
        // 拖入时会被复制到这个目录，所以它就是唯一的条目来源。
        //
        // 仍然放后台：目录里条目多了以后 IO 仍会累积，而首屏不该等它。
        let (tx, rx) = mpsc::channel();
        let own_dir = paths::shortcuts_dir().ok();
        // 用 RedrawNotifier 而不是直接搬 Window：Window 含裸指针句柄，不是 Send。
        let notify = window.redraw_notifier();
        std::thread::spawn(move || {
            let sources = own_sources(own_dir);
            match crate::shell::scan::scan(&sources) {
                Ok(report) => {
                    // 接收端若已消失（程序正在退出）说明没人要了，忽略即可
                    let _ = tx.send(report);
                }
                Err(e) => eprintln!("后台扫描失败: {e}"),
            }
            // 唤醒 UI 线程去取结果 —— 比挂轮询定时器省电得多
            notify.request_redraw();
        });

        Ok(Self {
            _com: com,
            window,
            renderer,
            composer,
            panel: PanelPainter::new(),
            caption: CaptionPainter::new(),
            sidebar: SidebarPainter::new(),
            grid: GridPainter::new(),
            settings: SettingsPainter::new(),
            menu_painter: MenuPainter::new(),
            text: TextEngine::new()?,
            icons: IconCache::new(ICON_BUDGET),
            loader: Loader::spawn(window.redraw_notifier()),
            cfg,
            cfg_store,
            i18n,
            scheme,
            state: state_store,
            strings,
            layout,
            library: Library::new(Vec::new()),
            scan_rx: Some(rx),
            tabs,
            active_tab,
            query: String::new(),
            visible: Vec::new(),
            icon_size,
            scroll: 0.0,
            backdrop,
            maximized,
            hover_caption: None,
            hover_tab: None,
            hover_tile: None,
            hover_settings: false,
            settings_open: false,
            hover_setting_row: None,
            hover_settings_close: false,
            hover_settings_button: None,
            exit_requested: false,
            menu: None,
            menu_hover: None,
            editing: None,
            recording_hotkey: false,
            dragging_tab: None,
            drop_tab_target: None,
            selected: std::collections::HashSet::new(),
            tile_drag: None,
            tile_drop: None,
            marquee: None,
            frame_stat: FrameStat::new(),
            icon_requests: 0,
            menu_anchor: (0, 0),
            resizing: false,
        })
    }

    /// 显示窗口并进入消息循环，直到窗口关闭。
    pub fn run(mut self) -> Result<()> {
        // 复制一份句柄再调用：`self.window.run(&mut self)` 会同时借用
        // `self` 的字段与整体，借用检查器不接受。`Window` 是 Copy 的。
        let window = self.window;
        window.show();
        let result = window.run(&mut self);
        self.save_state();
        result
    }

    /// 实际生效的背景材质（供自检与设置界面显示）。
    pub fn backdrop(&self) -> Backdrop {
        self.backdrop
    }

    /// 已扫描到的快捷方式总数（供自检确认后台扫描真的跑通了）。
    pub fn scanned_total(&self) -> usize {
        self.library.total()
    }

    /// 当前选项卡下可见的条目数。
    pub fn visible_count(&self) -> usize {
        self.visible.len()
    }

    /// 当前选项卡的键。自检输出会带上它 ——
    /// 否则看到"显示 0 条"会以为是扫描坏了，实际只是停在一个空选项卡上。
    pub fn active_tab_key(&self) -> &str {
        self.tabs
            .get(self.active_tab)
            .map(|t| t.key.as_str())
            .unwrap_or("")
    }

    /// 自检用：把滚动拉到底并重绘一帧。
    ///
    /// 返回 `(首个可见条目, 末尾条目, 总可滚量, 条目总数, 用到的选项卡键)`。
    /// 这些值都在测试**进行中**捕获 —— 测试结束时会还原用户状态，
    /// 若用还原后的状态去打印，就会出现"选项卡 desktop 显示第 133~142 条"这种自相矛盾。
    ///
    /// 固定切到**第 0 个选项卡（全部）**再测：否则用户上次停在某个空选项卡
    ///（比如桌面上没有快捷方式）时，这个验证会因为没有内容而变成空转。
    pub fn selftest_scroll_to_end(&mut self) -> Result<(usize, usize, f32, usize, String)> {
        let saved_tab = self.active_tab;
        self.active_tab = 0;
        self.refresh_view();

        let tab_key = self.active_tab_key().to_string();
        self.scroll = self.layout.max_scroll;
        self.rebuild_layout();
        self.draw()?;
        let result = (
            self.layout.first_item,
            self.layout.first_item + self.layout.tiles.len(),
            self.layout.max_scroll,
            self.visible.len(),
            tab_key,
        );

        // 还原，别让自检改变程序状态
        self.active_tab = saved_tab;
        self.scroll = 0.0;
        self.refresh_view();
        Ok(result)
    }

    /// 自检用：改变窗口尺寸，验证交换链与布局是否跟着重建。
    ///
    /// 返回 `(渲染器 DIP 宽, 渲染器 DIP 高, 布局面板宽, 布局面板高)`。
    ///
    /// 这是对"拖边框后内容被裁切/空白"这类问题的直接复现：
    /// 若 `ResizeBuffers` 失败，渲染器尺寸会停在旧值，
    /// 于是内容按旧尺寸绘制 —— 变小则右侧被裁掉，变大则新增区域空白。
    pub fn selftest_resize(&mut self, width: i32, height: i32) -> Result<(f32, f32, f32, f32)> {
        use windows::Win32::UI::WindowsAndMessaging::{
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos,
        };

        // SAFETY: hwnd 有效；只改尺寸，不动位置与 Z 序。
        unsafe {
            let _ = SetWindowPos(
                self.window.hwnd(),
                None,
                0,
                0,
                width,
                height,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }

        // SetWindowPos 会发 WM_SIZE，但自检**不在消息循环里**（handler 未安装），
        // 所以这里显式走一遍与 WM_SIZE 完全相同的处理路径。
        let (cw, ch) = self.window.client_size();
        self.on_resized(cw.max(1) as u32, ch.max(1) as u32);

        self.draw()?;

        let (dw, dh) = self.renderer.dip_size();
        Ok((dw, dh, self.layout.panel.2 - self.layout.panel.0, self.layout.panel.3 - self.layout.panel.1))
    }

    /// 自检用：打开设置面板画一帧，验证该绘制路径。
    pub fn selftest_settings(&mut self) -> Result<()> {
        self.settings_open = true;
        let r = self.draw();
        self.settings_open = false;
        r
    }

    /// 面板相对窗口的内缩量。
    ///
    /// 玻璃关闭时应为 **0** —— 面板铺满整个窗口，窗口外不再有任何透明/发光区域。
    /// 这是"关不干净"问题的直接判据。
    pub fn panel_inset(&self) -> f32 {
        self.layout.panel.0
    }

    /// 面板圆角半径。平面模式下必须为 **0** ——
    /// 只要还有圆角，圆角外那圈三角区域就是透明的，
    /// "窗口外完全无透明"就无法成立。
    pub fn panel_radius(&self) -> f32 {
        self.layout.radius
    }

    /// 自检用：切换外圈玻璃开关，返回切换前后的状态。
    ///
    /// 每项是 `(是否申请了背景模糊, 面板内缩量, 面板圆角半径)`。
    /// 验证的是**开关真的接通了系统层**（走到 `SetWindowCompositionAttribute`
    /// 与 `DwmSetWindowAttribute`），而不是只改了一个内存里的布尔值。
    ///
    /// 注意：`toggle_setting` 内部会落盘，所以这里必须把用户原本的覆盖值还原并写回 ——
    /// 否则跑一次 `--selftest` 就会把用户在界面里的设置改掉。
    pub fn selftest_toggle_glass(&mut self) -> ((bool, f32, f32), (bool, f32, f32)) {
        let original = self.state.get().outer_glass;

        let snap = |app: &Self| {
            (
                app.backdrop != Backdrop::None,
                app.panel_inset(),
                app.panel_radius(),
            )
        };

        let before = snap(self);
        self.toggle_setting(0);
        let after = snap(self);
        self.toggle_setting(0);

        // 还原成跑自检之前的样子
        self.state.get_mut().outer_glass = original;
        let _ = self.state.save();

        (before, after)
    }

    /// 渲染一帧。供 `--selftest` 在**不显示窗口**的情况下验证整条图形链路。
    pub fn draw_once(&mut self) -> Result<()> {
        // 自检时后台扫描可能还没回来，这里等一下，
        // 保证自检覆盖到"有内容"的绘制路径（方格、空状态）。
        if let Some(rx) = self.scan_rx.take() {
            match rx.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(report) => self.adopt_scan(report),
                Err(e) => eprintln!("自检等待扫描结果失败: {e}"),
            }
        }
        self.draw()
    }

    /// 自检用：连画若干帧并计时，返回 `(平均毫秒, 最慢毫秒)`。
    ///
    /// `no_vsync` 为真时走"正在拖动"的 Present 策略（不等垂直同步），
    /// 量到的是**这一帧真正要花的时间**；为假时量到的是"加上等垂直同步"
    /// 的墙上时间。两者一减就是同步等待的代价。
    ///
    /// 加这个是为了回答"拖动为什么卡"这类问题：凭感觉猜是错误的来源，
    /// 而"每秒能画几帧"必须测量 —— 帧耗时超过一个刷新周期时，
    /// 鼠标移动就会被积压，手感就是"发涩"。
    pub fn selftest_frame_bench(&mut self, frames: usize, no_vsync: bool) -> (f32, f32) {
        let saved = self.resizing;
        self.resizing = no_vsync;
        let mut total = 0.0f32;
        let mut worst = 0.0f32;
        let frames = frames.max(2);
        for i in 0..frames {
            let t = std::time::Instant::now();
            if self.draw().is_err() {
                break;
            }
            let ms = t.elapsed().as_secs_f32() * 1000.0;
            // 第 0 帧会带上延迟初始化（DXGI 首帧、字形缓存等），不计入平均。
            if i > 0 {
                total += ms;
                worst = worst.max(ms);
            }
        }
        self.resizing = saved;
        (total / (frames - 1) as f32, worst)
    }

    /// 自检用：先空转 `warmup` 帧让能落地的图标都落地，再连画 `frames` 帧，
    /// 返回这段里**重新发起**的图标请求次数。
    ///
    /// 正常情况下应当为 **0**：每个图标只请求一次，落地后进缓存就不再请求。
    /// 若它随帧数线性增长，说明有图标**永远提取不成功**，而每帧都在重试 ——
    /// 加载线程会一直做白工，UI 每帧重绘都在催它，拖动就会明显卡。
    /// 这是纯逻辑，自检即可判定，不需要开界面。
    pub fn selftest_icon_rerequests(&mut self, warmup: usize, frames: usize) -> u32 {
        for _ in 0..warmup {
            if self.draw().is_err() {
                return 0;
            }
        }
        let before = self.icon_requests;
        for _ in 0..frames {
            if self.draw().is_err() {
                break;
            }
        }
        self.icon_requests.saturating_sub(before)
    }

    /// 自检用：模拟一次"空白处框选拖动"，返回 `(移动次数, 其中请求了重绘的次数)`。
    ///
    /// 这条判定直接对应"选框流畅不流畅"：拖动期间**每一次移动都必须请求重绘**，
    /// 因为选框本身要跟着鼠标走。若只在"选中集发生变化"时才请求重绘，
    /// 选框就会停在原地、直到划到下一个方格才跳一下 —— 实测每秒只画 12 帧，
    /// 而处理每次移动只花 0.01ms，即"既不忙也不动"。
    pub fn selftest_marquee_redraws(&mut self, steps: usize) -> (usize, usize) {
        let scale = self.window.scale();
        // 逻辑坐标 → 物理坐标（`on_mouse_*` 收的是物理坐标）
        let phys = |lx: i32, ly: i32| {
            (((lx as f32) * scale) as i32, ((ly as f32) * scale) as i32)
        };

        // 起点取内容区左下角的空白处：方格从上往下填，那里通常没有方格。
        let content = self.layout.content;
        let (x0, y0) = (content.x + 6, content.y + content.h - 8);
        if self.layout.tile_at(x0, y0).is_some() {
            // 万一那里真有方格（条目很多），就退回"不测"而不是给出假数据
            return (0, 0);
        }
        let (px, py) = phys(x0, y0);
        let _ = self.on_mouse_down(px, py);

        let steps = steps.max(1);
        let mut asked = 0;
        for i in 1..=steps {
            // 向右上小幅移动，始终不出内容区
            let (mx, my) = phys(x0 + i as i32 * 2, y0 - i as i32);
            if self.on_mouse_move(mx, my) {
                asked += 1;
            }
        }
        let (ex, ey) = phys(x0 + steps as i32 * 2, y0 - steps as i32);
        let _ = self.on_mouse_up(ex, ey);
        (steps, asked)
    }

    /// 采纳扫描结果。
    fn adopt_scan(&mut self, report: ScanReport) {
        // 先记下"这一批之前就已存在的条目"，用来分辨哪些是**真正新加的**。
        // 必须在替换 `library` 之前取，否则新旧分不出来。
        let before: std::collections::HashSet<String> = (0..self.library.total())
            .filter_map(|i| self.library.get(i))
            .map(crate::model::library::path_key)
            .collect();

        self.library = Library::new(report.items);
        self.assign_new_order(&before);
        self.prune_overrides();
        self.normalize_groups();
        self.refresh_view();
    }

    /// 给本次**新加**的条目排到末尾。
    ///
    /// 只动"不在 `before` 里、且还没有 order"的条目 —— 老条目一律不碰，
    /// 这样它们保持"无 order"状态，按 `Library::view` 的规则继续按扫描顺序
    /// 显示，用户现有的排列不会因为加了一个新快捷方式就被打乱。
    ///
    /// 新条目取"现有最大值 + 1"，于是天然落在末尾（需求："新快捷方式自动放到末尾"）。
    fn assign_new_order(&mut self, before: &std::collections::HashSet<String>) {
        // **首次扫描必须直接返回。**
        //
        // 程序启动时 `library` 是空的，第一次 `adopt_scan` 的 `before` 必然是空集，
        // 于是全部条目都会被判成"新加的"、统统写上序号 —— 实测会把 `state.toml`
        // 从 1288 字节撑到 2045 字节（18 条全被物化）。视觉顺序虽然没变，
        // 但状态文件被无谓撑大，且"只给新条目排序"的语义也丢了。
        //
        // 空库时也确实不需要排序：没有"老条目"可比，谁先谁后都一样。
        if before.is_empty() {
            return;
        }

        let mut max = self
            .state
            .get()
            .entries
            .values()
            .filter_map(|o| o.order)
            .max()
            .unwrap_or(0);

        let mut changed = false;
        for i in 0..self.library.total() {
            let Some(s) = self.library.get(i) else {
                continue;
            };
            let key = crate::model::library::path_key(s);
            if before.contains(&key) {
                continue; // 老条目，不动
            }
            let mut ov = self.state.get().entry(&key);
            if ov.order.is_some() {
                continue; // 已经有位置了（例如拖过一次），别覆盖用户排的序
            }
            max += 1;
            ov.order = Some(max);
            self.state.get_mut().set_entry(&key, ov);
            changed = true;
        }
        if changed {
            if let Err(e) = self.state.save() {
                eprintln!("保存新条目顺序失败: {e}");
            }
        }
    }

    /// 把 `key` 挪到当前选项卡里的第 `slot` 道缝（可见条目下标，
    /// 由 [`Layout::drop_slot_at`] 算出）。
    ///
    /// ## 为什么必须先物化整张表
    ///
    /// 排序规则是"**有 order 的排在无 order 的后面**"（见 `Library::view`）。
    /// 所以只给被拖的那一条写序号的话，它会直接甩到末尾，而不是落到用户松手的
    /// 位置 —— 往第一个格子前面拖，结果跑到最后一格，看起来就像功能坏了。
    /// 因此这里先把当前选项卡内**全部**条目的先后落成显式序号，再挪那一条。
    ///
    /// 物化只覆盖**当前选项卡**：`Library::view` 先按选项卡过滤再排序，
    /// 不同选项卡的序号永远不参与同一次比较，各自独立编号是安全的。
    /// 顺带的好处是"动一个图标"不会把别的选项卡也写进状态文件。
    ///
    /// 基准视图用**不带搜索词**的那一份：搜索时可见集合只是它的一个子序列，
    /// 若拿搜索结果去编号，没被搜到的条目会继续保持"无序号"，
    /// 于是清空搜索框的那一刻它们整体跳到最前面 —— 看起来像排列被打乱了。
    ///
    /// 返回 `false` 表示位置没变（或这条根本不在当前选项卡里），此时**不写盘**。
    fn apply_reorder(&mut self, key: &str, slot: usize) -> bool {
        let Some(tab) = self.tabs.get(self.active_tab).cloned() else {
            return false;
        };
        let overrides = self.state.get().entries.clone();
        let full = self.library.view(&tab, "", &overrides);
        let mut order: Vec<String> = full
            .iter()
            .filter_map(|&i| self.library.get(i))
            .map(crate::model::library::path_key)
            .collect();

        let Some(from) = order.iter().position(|k| k == key) else {
            // 键不在当前选项卡里（例如拖拽中途改过分组）。
            // 什么都不做比"猜一个位置"安全。
            return false;
        };

        // 可见落点 → 基准视图里的位置。可见集合是基准视图的子序列，
        // 所以"可见集合的末尾"就是"基准视图的末尾"，取不到直接算末尾。
        let target = match self.visible.get(slot).and_then(|&i| self.library.get(i)) {
            Some(s) => {
                let k = crate::model::library::path_key(s);
                match order.iter().position(|x| *x == k) {
                    Some(p) => p,
                    None => return false,
                }
            }
            None => order.len(),
        };

        // 先把被拖的那条**摘出来**，再插进目标位置 —— 顺序反了会用错的下标。
        // 摘除之后，目标位置若原本在它后面，就要整体前移一格。
        let moved = order.remove(from);
        let target = if target > from { target - 1 } else { target };
        let target = target.min(order.len());
        if target == from {
            // 位置没变（拖了一点又放回原处）：不写盘。
            // 每次拖动都重写整张表既没必要，也会让"手抖一下"留下无谓的磁盘写。
            return false;
        }
        order.insert(target, moved);
        debug_assert!(order.iter().any(|k| k == key));

        for (i, k) in order.iter().enumerate() {
            let mut ov = self.state.get().entry(k);
            ov.order = Some(i as u32);
            self.state.get_mut().set_entry(k, ov);
        }
        if let Err(e) = self.state.save() {
            eprintln!("保存条目顺序失败: {e}");
        }

        // 顺序变了，可见集合要按新序号重算（顺带清空选中集与拖拽状态）。
        self.refresh_view();
        true
    }

    /// 清掉覆盖表里"指向已不存在的条目"的记录。
    ///
    /// 覆盖表按 `.lnk` **完整路径**做键（见架构文档 §9.2），所以在程序之外
    /// 删掉或改名某个 `.lnk` 之后，那条记录就成了孤儿：它永远不会再被匹配到，
    /// 却一直留在 `state.toml` 里，越积越多。
    ///
    /// 这在"条目只来自我们自己的目录"之后变得可以自动处理 —— 目录就是权威清单，
    /// 凡是不在清单里的键一律可以清掉。
    ///
    /// **不会误删"隐藏"标记**：删除只写覆盖表、不动文件，
    /// 所以被隐藏的条目文件仍在清单里，不会命中这条清理。
    fn prune_overrides(&mut self) {
        let live: std::collections::HashSet<String> = (0..self.library.total())
            .filter_map(|i| self.library.get(i))
            .map(crate::model::library::path_key)
            .collect();

        let before = self.state.get().entries.len();
        if before == 0 {
            return;
        }
        self.state
            .get_mut()
            .entries
            .retain(|key, _| live.contains(key));
        let removed = before - self.state.get().entries.len();
        if removed > 0 {
            if let Err(e) = self.state.save() {
                eprintln!("清理失效条目记录失败: {e}");
            } else {
                eprintln!("已清理 {removed} 条失效的条目记录（对应文件已不在目录里）");
            }
        }
    }

    /// 按当前选项卡与搜索词重算可见条目，并同步布局。
    fn refresh_view(&mut self) {
        let Some(tab) = self.tabs.get(self.active_tab).cloned() else {
            // 选项卡不存在（配置被手改坏）时可见集合会变空，
            // 选中集必须跟着清 —— 否则菜单会对着一个空视图报
            // "删除（5 项）"，而用户看不到那 5 项里的任何一项。
            self.clear_selection();
            self.visible.clear();
            return;
        };
        // 覆盖表来自 state.toml：删除/重命名/分组都在这里生效。
        // 先克隆出来解开对 self.state 的借用，否则下面 `self.visible = ...`
        // 会和 `self.library` 一起被借两次。
        let overrides = self.state.get().entries.clone();
        self.visible = self.library.view(&tab, &self.query, &overrides);

        // 视图内容集变了 → 选中集和拖拽状态必须一起清掉。
        //
        // 这是批量操作**最危险的失败模式**：选中集里留着此刻不在视图里的条目，
        // 之后一次"删除（5 项）"会把它一起隐藏掉 —— 用户看到的是"我只删了一个，
        // 怎么少了好几个"，而且他根本不知道少了哪几个。
        //
        // 放在这里而不是每个调用点各写一次：`self.visible` 全项目只有上面这一处
        // 赋值，切选项卡 / 搜索 / 重新扫描 / 改分组 / 删条目全部经这里，是唯一漏斗。
        self.clear_selection();

        self.rebuild_layout();
    }

    fn rebuild_layout(&mut self) {
        // 这里**故意不清**选中集，清空点见 `refresh_view`。
        //
        // 清空的唯一理由是"选中集里留了此刻不在视图里的条目"，而那只有当
        // **视图内容集**（`self.visible`）变化时才会发生，`self.visible`
        // 全项目只在 `refresh_view` 里被赋值，清在那边正好覆盖全部入口。
        //
        // 放在这里就过头了：滚动、窗口缩放、DPI 变化、Ctrl+滚轮改图标尺寸
        // 都会调 `rebuild_layout`，但它们**不改变视图里有哪几条**（同一条目
        // 只是换了位置或被滚出可视区）。在这些路径上清空，后果是用户选了
        // 5 条、往下滚一屏想再选 3 条，前面的选中全没了 —— 而条目多到需要
        // 滚动，恰恰是批量操作最常用的场合。
        let (dip_w, dip_h) = self.renderer.dip_size();
        let layout_inset = if self.outer_glass() {
            self.scheme.glow_radius + 1.0
        } else {
            0.0
        };
        self.layout = Layout::compute(
            &LayoutInput {
                width: dip_w,
                height: dip_h,
                panel_inset: layout_inset,
                corner_radius: if self.outer_glass() {
                    self.cfg.appearance.corner_radius
                } else {
                    0.0
                },
                sidebar_width: SIDEBAR_WIDTH,
                tile_gap: self.cfg.layout.tile_gap,
                columns: self.cfg.layout.columns,
                icon_size: self.icon_size,
                item_count: self.visible.len(),
                scroll: self.scroll,
                settings_rows: SETTINGS_ITEM_COUNT,
            },
            self.tabs.len(),
        );
        // 布局会把滚动量夹到 [0, max_scroll]，这里同步回来，
        // 否则窗口变大后 self.scroll 会停在一个超出范围的值上。
        self.scroll = self.layout.scroll;
    }

    /// 清空多选与方格拖拽状态。
    ///
    /// 两者**必须一起清**：只清选中集而留着 `tile_drag`，松手时的批量改分组
    /// 就落在一个空集合上（表现为"拖了没反应"）；只清拖拽而留着选中集，
    /// 则可能留下用户已经看不见的选中项 —— 那正是批量删除最危险的形态。
    fn clear_selection(&mut self) {
        self.selected.clear();
        self.tile_drag = None;
        self.tile_drop = None;
        // 框选状态也要一起清：`refresh_view` 会在视图内容集变化时调用本函数，
        // 留着一个指向旧方格的框，松手时就会按**过期的几何**去选条目。
        self.marquee = None;
    }

    /// 可见下标 → 覆盖表键。
    ///
    /// 多选与拖拽一律只记键，所以这里是唯一的换算入口：
    /// 换算逻辑散在多处是"键不一致、改动莫名不生效"的常见来源。
    fn tile_key_at(&self, item_index: usize) -> Option<String> {
        let &visible_index = self.visible.get(item_index)?;
        self.library
            .get(visible_index)
            .map(crate::model::library::path_key)
    }

    /// 把**整个选中集**改到某个分组（`None` = 主界面）。只写覆盖表。
    ///
    /// 返回 `false` 表示选中集是空的（什么都没做）。
    fn move_selection_to_group(&mut self, group: Option<String>) -> bool {
        // 先把键克隆成一份列表再写：`self.state.get_mut()` 会可变借用 `self.state`，
        // 而边遍历 `self.selected` 边写状态是对 `self` 的同时借用，借用检查器不允许。
        // 克隆几十字节的字符串换来借用关系彻底简单，很划算。
        let keys: Vec<String> = self.selected.iter().cloned().collect();
        if keys.is_empty() {
            return false;
        }

        for key in &keys {
            let mut ov = self.state.get().entry(key);
            ov.group = group.clone();
            self.state.get_mut().set_entry(key, ov);
        }
        if let Err(e) = self.state.save() {
            eprintln!("保存条目分组失败: {e}");
        }

        // 分组一变，这些条目可能**整批**离开当前选项卡，可见集合必须重算。
        // 重算顺带清空选中集（见 `rebuild_layout` 里的说明）。
        self.refresh_view();
        true
    }

    /// 把**整个选中集**标记为隐藏（菜单里的"删除"）。只写覆盖表，不动任何文件。
    ///
    /// 返回 `false` 表示选中集是空的（什么都没做）。
    fn hide_selection(&mut self) -> bool {
        let keys: Vec<String> = self.selected.iter().cloned().collect();
        if keys.is_empty() {
            return false;
        }

        for key in &keys {
            let mut ov = self.state.get().entry(key);
            ov.hidden = true;
            self.state.get_mut().set_entry(key, ov);
        }
        if let Err(e) = self.state.save() {
            eprintln!("保存条目设置失败: {e}");
        }

        // 可见集合少了这些条目，必须重算（顺带清空选中集）。
        self.refresh_view();
        true
    }

    /// 把窗口几何与界面状态写回 `state.toml`。
    /// 说明文档的文件名。放在**程序同目录**（便携布局，见 `paths::data_dir`）。
    fn help_doc_path() -> Option<std::path::PathBuf> {
        crate::paths::data_dir().ok().map(|d| d.join(HELP_FILE))
    }

    /// 确保同目录下有使用说明文件，返回它的路径。
    ///
    /// **只在该文件不存在时写入**：用户可能自己在上面做批注，
    /// 每次启动都覆盖会把批注冲掉。想恢复成内置版本，删掉它再点一次按钮即可。
    ///
    /// 写的是 UTF-8 **带 BOM**：不带 BOM 的话，部分编辑器（含 Windows 上一些
    /// 老牌记事本实现）会按系统 ANSI 代码页解读，中文直接变乱码 ——
    /// 本工程的备份脚本就踩过同一个坑。
    fn ensure_help_doc() -> Option<std::path::PathBuf> {
        let path = Self::help_doc_path()?;
        if !path.exists() {
            let mut bytes = Vec::with_capacity(HELP_TEXT.len() + 3);
            bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            bytes.extend_from_slice(HELP_TEXT.as_bytes());
            if let Err(e) = std::fs::write(&path, &bytes) {
                // 写不进去不算致命：下面照样打开（或让系统报"找不到文件"）
                eprintln!("写入使用说明失败: {e}");
            }
        }
        Some(path)
    }

    /// 明确退出程序：置上标志再走正常的 `WM_CLOSE` 流程。
    ///
    /// **必须走 `WM_CLOSE` 而不是直接 `DestroyWindow`**：那条路径上还有
    /// 摘托盘图标、写回窗口几何、`PostQuitMessage` 等收尾动作，
    /// 绕过去会留下幽灵托盘图标和丢失的窗口位置。
    ///
    /// 放在 `impl App` 里而不是 `impl Handler`：它是本程序自己的动作，
    /// 不是窗口层要求实现的回调。
    fn request_exit(&mut self) {
        self.exit_requested = true;
        self.window.close();
    }

    /// 「关于更新」：用默认浏览器打开项目的发布页。
    ///
    /// 程序里**没有**自动更新机制（那需要联网拉清单、校验、替换自身，
    /// 对一个便携小工具来说是过度设计）。所以这个按钮做的是它能诚实做到的事：
    /// 把"去哪儿看有没有新版"直接送到浏览器，用户自己判断要不要下载替换。
    ///
    /// 不联网、不检查、不提示 —— 按钮上写着"关于更新"，点开就是更新信息页。
    fn open_about(&mut self) {
        const RELEASES: &str = "https://github.com/heiheids/quick-spin/releases";
        diag("open_about 开始");
        match crate::shell::launch::open_url(RELEASES) {
            Ok(()) => diag("open_about: shell 已返回成功"),
            Err(e) => diag(&format!("open_about: 打开失败 {e}")),
        }
    }

    /// 打开使用说明（没有就先生成一份）。
    fn open_help_doc(&mut self) {
        let Some(path) = Self::ensure_help_doc() else {
            diag("open_help_doc: 取数据目录失败");
            return;
        };
        diag(&format!("open_help_doc 开始: {}", path.display()));
        match crate::shell::launch::open(&path) {
            Ok(()) => diag("open_help_doc: shell 已返回成功"),
            Err(e) => diag(&format!("open_help_doc: 打开失败 {e}")),
        }
    }

    /// 把窗口几何与界面状态写回 `state.toml`。
    fn save_state(&mut self) {
        let geo = self.window.geometry();
        let maximized = self.maximized;
        let active_key = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.key.clone())
            .unwrap_or_default();
        let icon_size = self.icon_size;

        {
            let s = self.state.get_mut();

            // 只在几何看起来有效时才写窗口字段。
            //
            // 本函数有**两条**调用路径：`WM_DESTROY` 时（窗口还在，几何正确）
            // 与消息循环返回后（窗口已销毁，GetWindowRect 与客户区尺寸都返回 0）。
            // 不加这道判断的话，后一条会把前一条写好的正确值覆盖成全 0 ——
            // 结果是窗口尺寸记不住，而且下次启动因为 x=Some(0) 会跑到屏幕左上角。
            if geo.width > 0 && geo.height > 0 {
                s.window.width = geo.width;
                s.window.height = geo.height;
                s.window.maximized = maximized;
                // 位置单独判断：最小化时 GetWindowRect 会给出 -32000 这类哨兵值
                if let (Some(x), Some(y)) = (geo.x, geo.y) {
                    if x > -30000 && y > -30000 {
                        s.window.x = Some(x);
                        s.window.y = Some(y);
                    }
                }
            }

            s.active_tab = active_key;
            s.icon_size = icon_size;
        }
        if let Err(e) = self.state.save() {
            eprintln!("保存界面状态失败: {e}");
        }
    }

    /// 尺寸变化：重建交换链缓冲 + 重算布局。
    fn on_resized(&mut self, width: u32, height: u32) {
        let dpi = self.window.dpi() as f32;
        if let Err(e) = self.renderer.resize(width, height, dpi) {
            eprintln!("重建交换链失败: {e}");
        }
        self.maximized = self.window.is_maximized();
        self.rebuild_layout();
    }

    /// 取可见方格那一段的图标，缺的发起异步加载。
    ///
    /// 返回的 `Vec` 与 `layout.tiles` 一一对应；`None` 表示还没加载好。
    /// 返回的是 `ID2D1Bitmap1` 的**克隆**（COM 引用计数 +1），
    /// 这样调用方不必同时借用 `self.icons`，绘制代码也不用关心缓存的生命周期。
    /// 每帧十几个 AddRef/Release 的开销可以忽略。
    fn collect_icons(&mut self, first: usize, count: usize) -> Vec<Option<ID2D1Bitmap1>> {
        let size = self.icon_size;
        // 按**物理像素**提取，不是 DIP。
        //
        // 绘制时用的是 DIP，而 D2D 上下文已按窗口 DPI 设过 SetDpi，
        // 所以 DIP 会被乘以缩放系数变成物理像素。在 150% 缩放下，
        // 128 DIP = 192 物理像素 —— 若位图只有 128 像素，就会被放大 1.5 倍而发虚。
        // 按物理像素提取才能和绘制尺寸 1:1 对上。
        //
        // 用物理像素做缓存键也顺带解决了跨显示器拖动（DPI 变化）的问题：
        // 换了 DPI 自然就是另一个键，不会错误复用低分辨率的旧位图。
        let dpi = self.window.dpi() as f32;
        let pixel_size = ((size as f32 * dpi / 96.0).round() as u32).clamp(16, 256);
        let mut out = Vec::with_capacity(count);

        for slot in 0..count {
            let Some(&index) = self.visible.get(first + slot) else {
                out.push(None);
                continue;
            };
            // 图标路径优先用覆盖表里的**图标覆盖**（"更换图标"的结果）。
            // 不查覆盖的话，换了图标界面不会变。
            //
            // 两种取法必须分开：自定义图标是图片，要按缩略图取（`Key::image`）；
            // 普通条目按图标取（`Key::new`）。混用会拿到"文件类型图标"，
            // 表现为"导入的图片看不到"。
            let override_icon = self
                .state
                .get()
                .entry(&crate::model::library::path_key(
                    self.library.get(index).unwrap(),
                ))
                .icon;
            let (path, as_image) = match override_icon {
                Some(icon) => (icon, true),
                None => (
                    self.library.get(index).unwrap().lnk_path.clone(),
                    false,
                ),
            };

            let key = if as_image {
                Key::image(path, pixel_size)
            } else {
                Key::new(path, pixel_size)
            };
            match self.icons.get(&key) {
                Some(bitmap) => out.push(Some(bitmap.clone())),
                None => {
                    self.loader.request(key);
                    self.icon_requests = self.icon_requests.saturating_add(1);
                    out.push(None);
                }
            }
        }

        out
    }

    /// 自检用：把图标尺寸依次设成给定档位，返回每档的
    /// `(图标请求边长, 方格边长, 一屏列数, 一屏行数, 一屏可见条目数)`。
    ///
    /// 验证的是用户直接感知的行为：**缩小图标应该让一屏放下更多条目**。
    /// 之前的实现在缩放时只改图标不改方格，条目数一点不变 ——
    /// 这个检查就是为了让那种退化再也无法悄悄发生。
    pub fn selftest_zoom(&mut self) -> Vec<(u32, i32, u32, usize, usize)> {
        let saved = self.icon_size;
        let mut out = Vec::new();

        for size in [16u32, 32, 64, 128, 256] {
            self.icon_size = size;
            self.rebuild_layout();
            let t = self.layout.tiles.first().copied();
            let side = t.map(|r| r.w).unwrap_or(0);
            // 按可见方格数反推，而不是直接读 grid_rows ——
            // 前者才是"用户真的能看到几个"，后者只是理论行数
            out.push((
                size,
                side,
                self.layout.grid_columns,
                self.layout.grid_rows,
                self.layout.tiles.len(),
            ));
        }

        self.icon_size = saved;
        self.rebuild_layout();
        out
    }

    /// 自检用：等图标加载落地。
    ///
    /// 返回 `(缓存条目数, 占用字节, 预算, 淘汰次数, 仍在途, 尺寸说明)`。
    ///
    /// 验证的是整条图标链路的**端到端**行为：请求 → 后台线程提取 →
    /// 转 D2D 位图 → 进缓存。任何一环断了，这里都会显示"条目数 0"。
    ///
    /// 等待上限是必要的：加载是异步的，自检不能无限等下去
    ///（比如某个条目永远提取不出来）。超时就如实报告还剩几个在途。
    pub fn selftest_icons(&mut self) -> (usize, usize, usize, u64, usize, String) {
        // 先请求一屏图标（`draw` 里会为可见方格发起请求）
        let _ = self.draw();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            let loaded = self.loader.poll();
            if !loaded.is_empty() {
                let ctx = self.renderer.context();
                for item in loaded {
                    if let Ok(bitmap) = item.result {
                        let _ = self.icons.insert(ctx, item.key, &bitmap);
                    }
                }
                // 来了新图标就重绘一次，顺便为下一屏发起新请求
                let _ = self.draw();
            }
            if self.loader.in_flight() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        (
            self.icons.len(),
            self.icons.used_bytes(),
            self.icons.budget(),
            self.icons.evictions(),
            self.loader.in_flight(),
            self.icons.size_summary(),
        )
    }

    /// 把用户选的图标**导入**成本程序自己的 128px PNG 并返回存放路径。
    ///
    /// 为什么不直接记住原文件路径：
    ///
    /// 1. **尺寸归一**。用户可能选一张 4000×3000 的照片，直接引用会让
    ///    图标缓存按原图尺寸吃内存（一张就 48 MB），128px 才是方格真正需要的。
    /// 2. **不受源文件变动影响**。原图被移动、改名、删除后图标不会消失。
    ///
    /// 缩放与解码都交给 shell 的缩略图接口完成（见 `icon::extract_image`），
    /// 所以这里不需要引入图像库，也自动处理了 EXIF 旋转之类的细节。
    fn import_icon(&self, source: &std::path::Path) -> Result<std::path::PathBuf> {
        /// 需求指定的存储边长。
        const STORED_SIZE: u32 = 128;

        let bitmap = crate::platform::icon::extract_image(source, STORED_SIZE)?;

        let dir = paths::icon_cache_dir()?;
        std::fs::create_dir_all(&dir).map_err(|e| crate::error::AppError::io(&dir, e))?;

        // 文件名用"原名 + 尺寸"，避免同一张图不同尺寸互相覆盖；
        // 再哈希一下完整路径，防止两个不同目录下的同名图片撞车。
        let stem = source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "icon".into());
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            source.hash(&mut h);
            h.finish() as u32
        };
        let file = dir.join(format!(
            "{}_{}_{:08x}.png",
            sanitize(&stem),
            bitmap.width,
            hash
        ));

        crate::platform::icon::write_png(&bitmap, &file)
            .map_err(|e| crate::error::AppError::io(&file, e))?;
        Ok(file)
    }

    /// 功能键。
    fn on_key_down(&mut self, vk: u32) -> bool {
        use crate::platform::hotkey;

        /// 常用虚拟键码。就地定义可读性更好。
        const VK_BACK: u32 = 0x08;
        const VK_RETURN: u32 = 0x0D;
        const VK_ESCAPE: u32 = 0x1B;
        const VK_END: u32 = 0x23;
        const VK_HOME: u32 = 0x24;
        const VK_LEFT: u32 = 0x25;
        const VK_RIGHT: u32 = 0x27;
        const VK_DELETE: u32 = 0x2E;

        // 录制热键优先吃按键。
        //
        // 放在最前面是必须的：就地编辑也会消费方向键、退格等，两者抢同一批
        // `WM_KEYDOWN`。没有这条优先级，录制时按下的键会同时被输入框吃掉。
        if self.recording_hotkey {
            if vk == VK_ESCAPE {
                // 取消录制，保留原来的热键
                self.recording_hotkey = false;
                return true;
            }
            // 修饰键本身不算数 —— 按 Ctrl+Alt+Q 的过程中 Ctrl 和 Alt
            // 会各自产生一次 WM_KEYDOWN，那时还没有主键。
            if !hotkey::is_modifier_vk(vk) {
                let mods = hotkey::current_modifiers();
                if let Some(spec) = hotkey::build_spec(mods, vk) {
                    self.apply_hotkey(&spec);
                }
                // 不认识的键（比如 VK_TAB）或没带修饰键时保持录制态，
                // 让用户接着按，而不是直接退出录制。
                self.recording_hotkey = false;
                return true;
            }
            return true;
        }

        // 编辑中优先吃按键，不要让 Esc 之类顺带把菜单也关了（本来也没开）
        if self.editing.is_some() {
            match vk {
                VK_RETURN => {
                    self.commit_editing();
                    return true;
                }
                VK_ESCAPE => {
                    // 取消：直接丢弃，不写覆盖表
                    self.editing = None;
                    return true;
                }
                VK_BACK => {
                    self.edit_remove_before();
                    return true;
                }
                VK_DELETE => {
                    self.edit_remove_after();
                    return true;
                }
                VK_LEFT => return self.edit_move_caret(-1),
                VK_RIGHT => return self.edit_move_caret(1),
                VK_HOME => return self.edit_set_caret(0),
                VK_END => {
                    let len = self
                        .editing
                        .as_ref()
                        .map(|e| e.text.chars().count())
                        .unwrap_or(0);
                    return self.edit_set_caret(len);
                }
                _ => return false,
            }
        }

        if vk == VK_ESCAPE && self.menu.is_some() {
            self.close_menu();
            return true;
        }
        false
    }

    /// 字符输入。控制字符（回车、退格、Tab…）由 `on_key_down` 处理，这里要挡掉，
    /// 否则它们会被当成普通字符插进文本里。
    fn on_char(&mut self, c: char) -> bool {
        if self.editing.is_none() || c.is_control() {
            return false;
        }
        let caret = self.editing.as_ref().map(|e| e.caret).unwrap_or(0);
        if let Some(ed) = self.editing.as_mut() {
            let byte = char_to_byte(&ed.text, caret);
            ed.text.insert(byte, c);
            ed.caret += 1;
        }
        true
    }

    /// 光标左移 / 右移。`delta` 为 -1 / +1。
    fn edit_move_caret(&mut self, delta: i32) -> bool {
        let Some(ed) = self.editing.as_ref() else {
            return false;
        };
        let len = ed.text.chars().count();
        let next = (ed.caret as i32 + delta).clamp(0, len as i32) as usize;
        self.edit_set_caret(next)
    }

    fn edit_set_caret(&mut self, caret: usize) -> bool {
        let Some(ed) = self.editing.as_mut() else {
            return false;
        };
        if ed.caret == caret {
            return false;
        }
        ed.caret = caret;
        true
    }

    /// 删光标前一个字符。
    fn edit_remove_before(&mut self) -> bool {
        let Some(ed) = self.editing.as_mut() else {
            return false;
        };
        if ed.caret == 0 {
            return false;
        }
        let start = char_to_byte(&ed.text, ed.caret - 1);
        let end = char_to_byte(&ed.text, ed.caret);
        ed.text.replace_range(start..end, "");
        ed.caret -= 1;
        true
    }

    /// 删光标后一个字符。
    fn edit_remove_after(&mut self) -> bool {
        let Some(ed) = self.editing.as_mut() else {
            return false;
        };
        let len = ed.text.chars().count();
        if ed.caret >= len {
            return false;
        }
        let start = char_to_byte(&ed.text, ed.caret);
        let end = char_to_byte(&ed.text, ed.caret + 1);
        ed.text.replace_range(start..end, "");
        true
    }

    /// 提交编辑：写进覆盖表或配置并落盘。
    ///
    /// **空文本等于清除别名**（回退到原名），不存空字符串 ——
    /// 存空串会让方格显示成空白、选项卡没有文字，看起来像坏了。
    fn commit_editing(&mut self) {
        let Some(ed) = self.editing.take() else {
            return;
        };
        let trimmed = ed.text.trim().to_string();

        match ed.target {
            EditTarget::Tile(key) => {
                let mut ov = self.state.get().entry(&key);
                ov.alias = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                };
                self.state.get_mut().set_entry(&key, ov);
                if let Err(e) = self.state.save() {
                    eprintln!("保存条目别名失败: {e}");
                }
                self.refresh_view();
            }
            EditTarget::Tab(index) => {
                // 选项卡名归 config.toml 管（它是用户意图，不是运行时状态）。
                // 空名没有意义，直接当取消处理，避免出现点不到的按钮。
                if trimmed.is_empty() {
                    return;
                }
                self.mutate_config_tab(index, move |tab| tab.name = trimmed);
            }
            EditTarget::Title => {
                // 标题归 config.toml 管。**空标题是允许的**，但不能真的存空 ——
                // 那样标题栏就空了，用户会以为程序坏了。空则回退到多语言默认标题。
                self.cfg.general.title = if trimmed.is_empty() {
                    String::new()
                } else {
                    trimmed
                };
                if let Err(e) = self.sync_config() {
                    eprintln!("保存标题失败: {e}");
                }
                // 三处都要更新：内存里的字符串、窗口的 OS 标题（任务栏/Alt+Tab 用）、
                // 以及界面重绘。
                self.strings.title = self.effective_title();
                self.window.set_title(&self.strings.title);
            }
            EditTarget::NewUrl => {
                // 空或只有个协议头等于没输入，什么都不做。
                if trimmed.is_empty() || trimmed == "https://" || trimmed == "http://" {
                    return;
                }
                let url = if trimmed.contains("://") {
                    trimmed
                } else {
                    // 没写协议就按 https 补 —— 直接拿 "www.bing.com" 当目标
                    // 会被系统当成相对路径，打不开。
                    format!("https://{trimmed}")
                };

                let dir = match paths::shortcuts_dir() {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("取快捷方式目录失败: {e}");
                        return;
                    }
                };
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    eprintln!("创建快捷方式目录失败: {e}");
                    return;
                }

                // 条目名用域名部分，比一整条 URL 好认（也避免文件名里出现 / 和 :）。
                let name = url_short_name(&url);
                let dest = dir.join(format!("{name}.lnk"));
                if let Err(e) =
                    crate::shell::lnk::create_link(&dest, std::path::Path::new(&url), "", &name)
                {
                    eprintln!("创建网址快捷方式失败: {e}");
                    return;
                }
                self.register_new_entry(&dest);
            }
        }
    }

    /// 当前应该显示的标题：配置里有就用配置的，否则用多语言默认值。
    fn effective_title(&self) -> String {
        if self.cfg.general.title.trim().is_empty() {
            self.i18n.t("app.title").to_string()
        } else {
            self.cfg.general.title.clone()
        }
    }

    /// 把指向**已不存在选项卡**的分组值清掉，让这些条目回落到主窗口。
    ///
    /// 不清理的后果是条目**静默消失**：`view` 拿当前选项卡的键去比对分组，
    /// 比对不上的条目不属于任何选项卡，于是既不在这里也不在那里。
    /// 用户看到的是"我的条目不见了"，而且没有任何提示。
    ///
    /// 什么时候会产生这种失效值：
    ///
    /// * 配置被重置/手改，选项卡列表变了（本次遇到的就是这种）
    /// * 删掉一个选项卡，而它当时还有条目 —— 虽然菜单里对"有内容"的选项卡
    ///   禁用了删除，但配置是用户可手改的，不能只依赖界面上的约束
    ///
    /// 随机应变地"落到主窗口"而不是丢弃记录：用户的条目是本程序里最要紧的东西，
    /// 分组对不上时宁可放错位置让他能看见，也不能让它凭空消失。
    fn normalize_groups(&mut self) {
        let valid: std::collections::HashSet<String> =
            self.tabs.iter().map(|t| t.key.clone()).collect();

        let mut fixed = 0usize;
        for ov in self.state.get_mut().entries.values_mut() {
            if let Some(group) = ov.group.as_deref() {
                if !valid.contains(group) {
                    ov.group = None;
                    fixed += 1;
                }
            }
        }

        if fixed > 0 {
            if let Err(e) = self.state.save() {
                eprintln!("修正失效分组失败: {e}");
            } else {
                eprintln!("已把 {fixed} 条条目的失效分组改回主窗口");
            }
        }
    }

    /// 重新扫描程序自己的快捷方式目录。
    ///
    /// 拖入新条目后调用。这里是**同步**扫描：目录通常只有几十个条目，
    /// 而拖放是低频操作，为此再开一条线程并把生命周期管起来不值得。
    fn rescan(&mut self) {
        let sources = own_sources(paths::shortcuts_dir().ok());
        match crate::shell::scan::scan(&sources) {
            Ok(report) => self.adopt_scan(report),
            Err(e) => eprintln!("重新扫描失败: {e}"),
        }
    }

    /// 用户把文件拖进窗口。
    ///
    /// 按需求"拖入就复制一个快捷方式进来"：把 `.lnk` **复制**到程序自己的
    /// `shortcuts` 目录，并在覆盖表里登记归当前选项卡。
    ///
    /// 复制而不是引用原路径，好处是原文件之后被移动/改名/删除都不影响本程序；
    /// 代价是磁盘上多一份几 KB 的 `.lnk`。
    fn on_files_dropped(&mut self, paths: Vec<std::path::PathBuf>) -> bool {
        let dir = match paths::shortcuts_dir() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("取快捷方式目录失败: {e}");
                return false;
            }
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("创建快捷方式目录失败: {e}");
            return false;
        }

        let group = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.key.clone())
            .unwrap_or_else(|| crate::config::model::MAIN_TAB.to_string());

        let mut added = 0usize;
        for src in &paths {
            // 只收快捷方式。拖进来一个 exe 或图片并不是"添加条目"的意图，
            // 静默忽略比自作主张地生成快捷方式更安全（后者会改动用户的文件布局）。
            if !src
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
            {
                continue;
            }
            match copy_shortcut_in(src, &dir) {
                Ok(dest) => {
                    // 登记归到当前选项卡。不登记的话它会落在默认的主窗口里，
                    // 用户拖到"程序"分组却发现条目出现在别处。
                    let key = dest.to_string_lossy().into_owned();
                    let mut ov = self.state.get().entry(&key);
                    ov.group = Some(group.clone());
                    self.state.get_mut().set_entry(&key, ov);
                    added += 1;
                }
                Err(e) => eprintln!("复制快捷方式失败 [{}]: {e}", src.display()),
            }
        }

        if added > 0 {
            if let Err(e) = self.state.save() {
                eprintln!("保存条目设置失败: {e}");
            }
            // 重新扫描自己的目录，新条目才会出现
            self.rescan();
        }
        added > 0
    }

    fn draw(&mut self) -> Result<()> {
        // 自愈保险：绘制前核对交换链尺寸是否等于窗口实际客户区。
        //
        // 为什么需要这一步：交互式拖边框时若某次 `WM_SIZE` 没能走到
        // `on_resized`（消息时序、最小化还原、DPI 切换等各种路径），
        // 渲染器与布局就会停在旧尺寸 —— 于是内容按旧宽度绘制，
        // 右对齐的元素（标题栏按钮）被推到窗口外面，缩小窗口时描边也被裁掉。
        //
        // 这里做一次 syscall 级的核对远比让画面错位划算：
        // 绘制本来就是"按需"的，一次 GetClientRect 的开销可以忽略。
        let (cw, ch) = self.window.client_size();
        if cw > 0 && ch > 0 {
            let (rw, rh) = self.renderer.pixel_size();
            if cw as u32 != rw || ch as u32 != rh {
                eprintln!(
                    "[resize] 尺寸不一致: 窗口 {cw}x{ch} vs 交换链 {rw}x{rh}，已自动重建"
                );
                self.on_resized(cw as u32, ch as u32);
            }
        }

        // 先把异步扫描的结果收进来（如果有）
        let received = self.scan_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(report) = received {
            self.scan_rx = None;
            self.adopt_scan(report);
        }

        // 收图标加载结果，转成 D2D 位图存进缓存。
        // 必须在开始绘制之前做完 —— 插入会触发 LRU 淘汰，
        // 如果边画边淘汰，可能把正要画的那张位图释放掉。
        let loaded = self.loader.poll();
        if !loaded.is_empty() {
            let ctx = self.renderer.context();
            for item in loaded {
                if let Ok(bitmap) = item.result {
                    if let Err(e) = self.icons.insert(ctx, item.key, &bitmap) {
                        eprintln!("图标转位图失败: {e}");
                    }
                }
                // 提取失败（文件没了、路径无图标）就静默跳过：
                // 方格画占位底色，不值得为每个坏图标刷一行日志。
            }
        }

        // 玻璃效果关闭时用**平面**配色：实心 + 无发光 + 无透明。
        // 否则窗口边缘仍会残留一圈透明与发光 —— 那正是"关不干净"的来源。
        let glass = self.outer_glass();
        let scheme = if glass {
            self.scheme.clone()
        } else {
            self.scheme.flat_variant()
        };
        let opacity = if glass {
            self.cfg.appearance.opacity
        } else {
            1.0
        };
        let (panel, radius) = (self.layout.panel, self.layout.radius);
        let (add, minimize, close) = (self.layout.add, self.layout.minimize, self.layout.close);
        let show_desktop = self.layout.hide_desktop_icons;
        let title_rect = self.layout.title_text.as_f32();
        let (hover_caption, hover_tab, hover_tile, hover_settings) = (
            self.hover_caption,
            self.hover_tab,
            self.hover_tile,
            self.hover_settings,
        );
        let active_tab = self.active_tab;
        // 拖动尺寸时**不再**跳过内容绘制。
        //
        // 之前为了手感做了"只画面板"的轻量模式（架构设计里的铁律 6），
        // 但实测发现：用户拖动边框时看到内容停在旧尺寸、松开才更新，观感更像卡死。
        // 现在每帧完整重绘；Present 的垂直同步本来就把频率限制在刷新率，
        // 不会失控。`resizing` 改用于决定 Present 是否等垂直同步。
        let light = false;

        // 当前可见区间的起点：tiles 对应条目 first_item..first_item+len
        let first = self.layout.first_item;

        // 为可见方格取图标。缺失的顺手向加载线程发起请求，
        // 结果就绪后加载线程会 PostMessage 唤醒重绘。
        //
        // 只请求**可见**的：142 个条目全量提取要好几秒，
        // 而用户大多数时候只看得到前十几个。
        //
        // 注意顺序：这一步要 `&mut self`，必须在下面构建 `tile_labels`
        // （它借用 `self.visible` / `self.library`）**之前**完成。
        let tile_icons = self.collect_icons(first, self.layout.tiles.len());

        // 标签借用 library 与 state；与下面要变借用的 self.text 是不同字段，不冲突。
        // 只取可见那一段：tiles 对应条目 first_item..first_item+len。
        //
        // 显示名优先用覆盖表里的**别名**（"重命名"的结果）。
        // 不查别名的话，重命名会"存下去了但界面没变"，看起来像功能坏了。
        let overrides = self.state.get().entries.clone();
        let tile_labels: Vec<String> = self
            .visible
            .iter()
            .skip(first)
            .take(self.layout.tiles.len())
            .map(|&i| {
                let Some(s) = self.library.get(i) else {
                    return String::new();
                };
                let key = crate::model::library::path_key(s);
                overrides
                    .get(&key)
                    .and_then(|o| o.alias.clone())
                    .unwrap_or_else(|| s.name.clone())
            })
            .collect();
        let tile_label_refs: Vec<&str> = tile_labels.iter().map(|s| s.as_str()).collect();

        // 与 `tile_labels` / `tile_icons` **逐项对齐**的选中标记。
        //
        // 用覆盖表键去查选中集（键由库里的路径算出，与多选时写入的完全同源），
        // 而不是拿可见下标去比：下标在一次滚动或一次重新扫描之后就换人了。
        // 这里只读 `self.selected`，不改它 —— 绘制绝不应当有副作用。
        let tile_selected: Vec<bool> = self
            .visible
            .iter()
            .skip(first)
            .take(self.layout.tiles.len())
            .map(|&i| {
                self.library.get(i).is_some_and(|s| {
                    self.selected
                        .contains(&crate::model::library::path_key(s))
                })
            })
            .collect();

        self.renderer.begin()?;
        self.panel
            .prepare(self.renderer.context(), panel, &scheme, opacity)?;
        self.panel
            .draw(self.renderer.context(), panel, radius, &scheme)?;

        if !light {
            self.caption.ensure(self.renderer.context())?;
            // 四个按钮都要画。漏掉任何一个，那块区域就是**空白但仍可点**——
            // 表现为"按钮不存在"，实际点击却能触发，最难排查。
            self.caption.draw(
                self.renderer.context(),
                show_desktop,
                CaptionButton::HideDesktopIcons,
                hover_caption == Some(CaptionButton::HideDesktopIcons),
                &scheme,
                opacity,
            )?;
            self.caption.draw(
                self.renderer.context(),
                add,
                CaptionButton::Add,
                hover_caption == Some(CaptionButton::Add),
                &scheme,
                opacity,
            )?;
            self.caption.draw(
                self.renderer.context(),
                minimize,
                CaptionButton::Minimize,
                hover_caption == Some(CaptionButton::Minimize),
                &scheme,
                opacity,
            )?;
            self.caption.draw(
                self.renderer.context(),
                close,
                CaptionButton::Close,
                hover_caption == Some(CaptionButton::Close),
                &scheme,
                opacity,
            )?;

            self.sidebar.ensure(self.renderer.context())?;
            self.sidebar.draw(
                self.renderer.context(),
                &mut self.text,
                &self.layout,
                &scheme,
                &self.strings.tab_labels,
                active_tab,
                hover_tab,
                if self.dragging_tab.is_some() {
                    self.drop_tab_target
                } else if self.tile_drag.as_ref().is_some_and(|d| d.active) {
                    // 方格拖拽的落点复用同一个高亮参数（`sidebar::draw` 的
                    // `drop_target`）：两处高亮的外观要求完全一致，
                    // 各画一套只会让"改了一处忘了另一处"变成下次的坑。
                    self.tile_drop
                } else {
                    None
                },
                hover_settings,
                opacity,
            )?;

            // 设置面板展开时不画网格：遮罩只有 90% 黑，网格的滚动条与方格
            // 仍会隐约透出来，看起来像"面板没画干净"。模态下面不该有可辨内容。
            if !self.settings_open {
                // 拖拽排序的插入位置：把**可见条目下标**换算成 `tiles` 内部的
                // 局部序号（`grid.draw` 只认识 `tiles`，不知道 `first_item` 这个偏移）。
                //
                // 只在这个分支里算：设置面板展开时网格根本不画，算了也没人看。
                //
                // `saturating_sub` 是零成本的保险：拖拽期间没有自动滚动，
                // `first_item` 不会变，落点必然不小于它。
                let drop_slot = self
                    .tile_drag
                    .as_ref()
                    .filter(|d| d.active)
                    .and_then(|d| d.over_index)
                    .map(|s| s.saturating_sub(self.layout.first_item));

                self.grid.ensure(self.renderer.context())?;
                self.grid.draw(
                    self.renderer.context(),
                    &mut self.text,
                    &self.layout,
                    &scheme,
                    &tile_label_refs,
                    &tile_icons,
                    hover_tile,
                    &tile_selected,
                    drop_slot,
                    opacity,
                )?;

                // 框选矩形画在方格**之上**（`draw_marquee` 自己会再压一次内容区
                // 裁剪 —— 选框能拖到侧栏上去，不裁剪会在侧栏上画出一条带子）。
                //
                // 只在真划出面积时画：刚按下还没拖的时候画一个 0×0 的框，
                // 会在鼠标位置闪出一个点。
                if let Some(m) = self.marquee.as_ref().filter(|m| !m.is_click()) {
                    self.grid.draw_marquee(
                        self.renderer.context(),
                        &self.layout,
                        &scheme,
                        m.rect(),
                    )?;
                }
            }
            // 标题栏底边的分隔线。**必须在方格之后画** ——
            // 方格框上边缘正好压在这条线上，先画会被方格的底色盖掉。
            self.sidebar
                .draw_title_separator(self.renderer.context(), &self.layout, &scheme)?;

            self.text.ensure(self.renderer.context())?;
            self.text.draw(
                self.renderer.context(),
                &self.strings.title,
                Role::Title,
                title_rect,
                scheme.title_text,
            )?;

            // 设置面板盖在内容区之上，最后画
            if self.settings_open {
                let glass = self.outer_glass();
                let topmost = self.always_on_top();
                let hide_after = self.hide_after_launch();
                // 热键那一行显示当前组合；录制中显示提示语。
                let hotkey_label = if self.recording_hotkey {
                    format!("{}：按下组合键…（Esc 取消）", self.strings.setting_hotkey)
                } else if self.cfg.general.hotkey.trim().is_empty() {
                    format!("{}：未设置", self.strings.setting_hotkey)
                } else {
                    format!("{}：{}", self.strings.setting_hotkey, self.cfg.general.hotkey)
                };
                let items = [
                    SettingsItem {
                        label: &self.strings.setting_outer_glass,
                        on: glass,
                        show_toggle: true,
                    },
                    SettingsItem {
                        label: &self.strings.setting_always_on_top,
                        on: topmost,
                        show_toggle: true,
                    },
                    SettingsItem {
                        label: &self.strings.setting_hide_after_launch,
                        on: hide_after,
                        show_toggle: true,
                    },
                    SettingsItem {
                        // 以**注册表实际状态**为准，不读配置：
                        // 用户可能在任务管理器里禁用了自启动，读配置会显示成开着的。
                        label: &self.strings.setting_autostart,
                        on: crate::platform::autostart::is_enabled(),
                        show_toggle: true,
                    },
                    SettingsItem {
                        label: &hotkey_label,
                        // 不是开关：点一下进入录制态。
                        on: false,
                        show_toggle: false,
                    },
                ];

                self.settings.ensure(self.renderer.context())?;
                // 色板：把常量里的十六进制文本解析成颜色。
                // 解析不了的项**跳过而不是补默认色** —— 补了会让格子数和
                // `layout.settings_swatches` 错位，点 A 却选中了 B。
                let swatches: Vec<config::Color> =
                    crate::config::theme::BASE_PALETTE
                        .iter()
                        .filter_map(|h| config::Color::from_hex(h))
                        .collect();
                // 只有当基础色**正好等于**某个色板项时才算"选中"。
                // 手改配置填了个自定义色时，没有任何格子该高亮 —— 硬找最近似的
                // 会误导用户以为他选的是那一格。
                let current_swatch = if self.cfg.appearance.base_color.trim().is_empty() {
                    None
                } else {
                    config::Color::from_hex(&self.cfg.appearance.base_color).and_then(|c| {
                        swatches.iter().position(|s| {
                            (s.r(), s.g(), s.b()) == (c.r(), c.g(), c.b())
                        })
                    })
                };
                self.settings.draw(
                    self.renderer.context(),
                    &mut self.text,
                    &self.layout,
                    &scheme,
                    &self.strings.settings_title,
                    &items,
                    &swatches,
                    current_swatch,
                    &self.strings.setting_theme_custom,
                    self.hover_setting_row,
                    self.hover_settings_close,
                    [
                        &self.strings.settings_exit_program,
                        &self.strings.settings_about_update,
                        &self.strings.settings_help,
                    ],
                    self.hover_settings_button,
                    self.layout.settings_button_level,
                )?;
            }
        }

        // ---- 就地编辑的文本框：盖在方格之上，最后画 ----
        if let Some(ed) = self.editing.as_ref() {
            if let Some(rect) = self.edit_box_rect(&ed.target) {
                let (value, caret) = (ed.text.clone(), ed.caret);
                self.grid.draw_edit_box(
                    self.renderer.context(),
                    &mut self.text,
                    rect,
                    &value,
                    caret,
                    &scheme,
                )?;
            }
        }

        // ---- 右键菜单：最上层浮层，最后画 ----
        if let Some(menu) = self.menu.as_ref() {
            self.menu_painter.ensure(self.renderer.context())?;
            self.menu_painter.draw(
                self.renderer.context(),
                &mut self.text,
                menu,
                &scheme,
                self.menu_hover,
            )?;
        }

        // 拖动尺寸期间不等垂直同步：模态尺寸循环里每帧阻塞一个刷新周期
        // 会让拖动明显发涩。此时画面在快速变化，撕裂根本看不出来。
        self.renderer
            .end(if self.resizing { 0 } else { 1 })?;
        self.composer.commit()?;
        Ok(())
    }

    /// 物理像素坐标 → 布局使用的逻辑像素。
    fn to_logical(&self, x: i32, y: i32) -> (i32, i32) {
        let scale = self.window.scale();
        ((x as f32 / scale) as i32, (y as f32 / scale) as i32)
    }

    // ---- 设置项读写 ----
    //
    // 这三个"生效值"都优先取 state.toml 里的覆盖项，没有再回落到 config.toml。
    // 这样设置面板改的是 state.toml，不会重写用户手写的配置文件。

    fn outer_glass(&self) -> bool {
        self.state
            .get()
            .outer_glass
            .unwrap_or(self.cfg.appearance.outer_glass)
    }

    /// 自检用：当前"外圈玻璃"的生效值。
    pub fn outer_glass_enabled(&self) -> bool {
        self.outer_glass()
    }

    fn always_on_top(&self) -> bool {
        self.state
            .get()
            .always_on_top
            .unwrap_or(self.cfg.general.always_on_top)
    }

    fn hide_after_launch(&self) -> bool {
        self.state
            .get()
            .hide_after_launch
            .unwrap_or(self.cfg.general.hide_after_launch)
    }

    /// 重新把背景材质应用到窗口。切换"外圈玻璃"时即时生效。
    /// 把 `self.cfg` 的改动同步进存储并落盘。
    ///
    /// **所有改配置的地方都必须走这一步，不能直接 `cfg_store.save()`。**
    ///
    /// 原因：`self.cfg` 是启动时从存储里 `clone()` 出来的一份**副本**
    ///（见 `App::new` 里的 `cfg_store.get().clone()`）。改它不会影响存储，
    /// 而 `cfg_store.save()` 写回去的是存储里那份启动时的旧值 ——
    /// 现象是"设置改了、界面上也生效了，**重启就全没了**"。
    ///
    /// 这类 bug 特别隐蔽：界面即时生效，所以测试时看不出问题，
    /// 只有重启才会发现，很容易被误判成"配置文件没写权限"。
    fn sync_config(&mut self) -> crate::error::Result<()> {
        *self.cfg_store.get_mut() = self.cfg.clone();
        self.cfg_store.save()
    }

    /// 应用一个新的全局热键：注册 → 存配置。
    ///
    /// **注册失败就不改配置**：热键被别的程序占用时，配置里若已经写上新值，
    /// 界面会显示成"已设置"，而实际按下去毫无反应 —— 用户会以为是热键功能坏了。
    /// 保持旧值至少让界面与真实状态一致。
    fn apply_hotkey(&mut self, spec: &str) {
        match crate::platform::hotkey::apply(self.window.hwnd(), spec) {
            Ok(()) => {
                self.cfg.general.hotkey = spec.to_string();
                if let Err(e) = self.sync_config() {
                    eprintln!("保存热键失败: {e}");
                }
            }
            Err(e) => eprintln!("设置热键「{spec}」失败: {e}"),
        }
    }

    /// 按 `cfg.appearance.base_color` 重算整套主题并立即生效。
    ///
    /// 基础色为空（或解析失败）时退回命名方案 —— 与启动时的取值规则**必须一致**，
    /// 否则会出现"启动是 A、点了色块之后是 B"的不一致。
    fn apply_base_color(&mut self) {
        let base = config::Color::from_hex(&self.cfg.appearance.base_color);
        self.scheme = match base {
            Some(c) => config::Scheme::from_base(c),
            None => {
                let book = SchemeBook::new(&self.cfg.schemes);
                book.resolve(&self.cfg.appearance.scheme).clone()
            }
        };
        // 背景材质与面板透明度都从 `self.scheme` 取，换色后必须重新套用，
        // 否则 DWM 那边的着色还是上一套的。
        self.apply_backdrop();
        self.rebuild_layout();
    }

    fn apply_backdrop(&mut self) {
        let requested = if self.outer_glass() {
            self.cfg.appearance.backdrop
        } else {
            Backdrop::None
        };
        self.backdrop = acrylic::apply(
            self.window.hwnd(),
            requested,
            self.scheme.acrylic_tint(self.cfg.appearance.opacity),
            self.outer_glass(),
        );
        // 布局随"是否留发光边距"变化，必须一起重算
        self.rebuild_layout();
    }

    /// 点设置面板里的第 `row` 项。返回是否需要重绘。
    fn toggle_setting(&mut self, row: usize) -> bool {
        match row {
            0 => {
                let next = !self.outer_glass();
                self.state.get_mut().outer_glass = Some(next);
                self.apply_backdrop();
            }
            1 => {
                let next = !self.always_on_top();
                self.state.get_mut().always_on_top = Some(next);
                self.window.set_topmost(next);
            }
            2 => {
                // 启动程序后是否收起面板。只改覆盖表，下一次启动条目就生效
                //（`launch_tile` 每次都经由 `hide_after_launch()` 现查）。
                let next = !self.hide_after_launch();
                self.state.get_mut().hide_after_launch = Some(next);
            }
            3 => {
                // 开机自启动。**改的是系统（注册表），不只是配置**：
                // 这一步失败必须让用户知道，否则开关看着打开了实际没生效。
                let next = !crate::platform::autostart::is_enabled();
                match crate::platform::autostart::set(next) {
                    Ok(()) => {
                        // 配置里也记一份，便于"设置与系统不一致"时排查
                        self.cfg.general.autostart = next;
                        if let Err(e) = self.sync_config() {
                            eprintln!("保存自启动配置失败: {e}");
                        }
                    }
                    Err(e) => {
                        eprintln!("设置开机自启动失败: {e}");
                        // 返回 false：界面上的开关不会变化，与真实状态一致
                        return false;
                    }
                }
            }
            4 => {
                // 「呼出热键」：点一下进入录制态，接下来按下的组合键会被捕获。
                // 见 `on_key_down` 里 `recording_hotkey` 的分支。
                self.recording_hotkey = true;
                return true;
            }
            _ => return false,
        }

        if let Err(e) = self.state.save() {
            eprintln!("保存设置失败: {e}");
        }
        true
    }

    // ------------------------------------------------------------------
    // 右键菜单
    // ------------------------------------------------------------------

    /// 标题文字的可点矩形。
    ///
    /// 宽度按字符估算 —— `TextEngine` 没有暴露测量接口，而这里只需要
    /// "文字大致占多宽"。估算规则与编辑框里光标位置那套一致
    ///（ASCII 窄、非 ASCII 按全角算）。
    ///
    /// 两个数**刻意估得比实际字号大**（`Role::Title` 是 16）：这只是点击热区，
    /// 估小了会让文字边缘点不中，估大了最多是旁边一小块空白也能点 ——
    /// 后者无害。所以改字号时**不必**同步改这里。
    ///
    /// 左右各留 6px：贴着字形边缘的像素不好点，而且估算本身有误差。
    ///
    /// 放在 `impl App` 里而不是 `impl Handler`：它是应用自己的几何计算，
    /// 不是处理器的接口方法（放错了编译器会报 "not a member of trait Handler"）。
    fn title_hit_rect(&self) -> RectI {
        let r = self.layout.title_text;
        let text_w: f32 = self
            .strings
            .title
            .chars()
            .map(|c| if c.is_ascii() { 12.0 } else { 22.0 })
            .sum();
        RectI::new(r.x - 6, r.y, (text_w + 12.0).ceil() as i32, r.h)
    }

    /// 计算编辑框该放在哪。
    ///
    /// 需求："**编辑时候临时加宽**，给他一个好看的文本框"。方格标签只有方格那么宽
    ///（缩到 16px 图标时不到 40px），原地放着根本看不清在打什么，所以编辑时
    /// 临时加宽到至少 `MIN_W`，并**向两侧对称扩张**（视觉上仍居中在原来的位置，
    /// 不会让人以为点错了条目）。
    ///
    /// 最后一定要**夹回窗口内**：靠近边缘的方格扩张后会露到窗口外，那部分看不见也点不到。
    fn edit_box_rect(&self, target: &EditTarget) -> Option<(f32, f32, f32, f32)> {
        /// 编辑框的最小宽度
        const MIN_W: f32 = 190.0;
        const HEIGHT: f32 = 26.0;

        let (win_w, win_h) = self.renderer.dip_size();
        // 统一收尾：夹回窗口内，免得靠近边缘时露到窗口外
        let clamp_rect = |left: f32, top: f32, width: f32| {
            (
                left.clamp(0.0, (win_w - width).max(0.0)),
                top.clamp(0.0, (win_h - HEIGHT).max(0.0)),
                width,
                HEIGHT,
            )
        };

        match target {
            EditTarget::Tile(key) => {
                // 找到该条目当前在可见集合里的位置
                let slot = self.visible.iter().position(|&i| {
                    self.library
                        .get(i)
                        .is_some_and(|s| crate::model::library::path_key(s) == *key)
                })?;
                let first = self.layout.first_item;
                if slot < first {
                    return None;
                }
                let rect = *self.layout.tiles.get(slot - first)?;
                let (l, t, w) = (rect.x as f32, rect.y as f32, rect.w as f32);

                // 标签所在那一行的垂直位置，与 grid 绘制标签时保持一致
                let label_top = t + TILE_PADDING + self.layout.icon_size + LABEL_GAP;

                let width = w.max(MIN_W);
                // 对称扩张：视觉上仍居中在原位置，不会让人以为点错了条目
                let left = l - (width - w) / 2.0;
                Some(clamp_rect(left, label_top, width))
            }
            EditTarget::Tab(index) => {
                let rect = *self.layout.tab_buttons.get(*index)?;
                // 侧栏只有 104px 宽，原地放不下，所以**向右延伸**到内容区上方。
                // 向左会在窗口外，向上/下会盖住相邻按钮。
                let left = rect.x as f32;
                let top = rect.y as f32 + (rect.h as f32 - HEIGHT) / 2.0;
                Some(clamp_rect(left, top, MIN_W))
            }
            EditTarget::Title => {
                // 标题原本就占着标题栏左侧一条，原地变成输入框即可。
                // 用 `max` 给它一个像样的宽度：标题短的时候原矩形很窄，
                // 变成输入框会小得没法输入。
                let rect = self.layout.title_text;
                let left = rect.x as f32;
                let top = rect.y as f32 + (rect.h as f32 - HEIGHT) / 2.0;
                Some(clamp_rect(left, top, (rect.w as f32).max(MIN_W)))
            }
            EditTarget::NewUrl => {
                // 锚在 "+" 按钮正下方，右对齐到它的右边 ——
                // 用户刚点的就是这个按钮，视线在那里。
                let r = self.layout.add;
                let left = r.x as f32 + r.w as f32 - MIN_W;
                let top = r.y as f32 + r.h as f32 + 4.0;
                Some(clamp_rect(left, top, MIN_W))
            }
        }
    }

    /// 关闭菜单并释放鼠标捕获。
    ///
    /// 捕获与菜单必须**成对**：菜单开着却不释放捕获，窗口会再也收不到
    /// 正常的外部点击，表现为整个程序"点不动"；反过来捕获没开就关闭菜单，
    /// 则点程序外部时菜单不消失。
    fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            self.menu_hover = None;
            self.window.set_capture(false);
        }
    }

    /// 弹出菜单：统一在这里加捕获，避免每个调用点各写一次。
    fn open_menu(&mut self, target: MenuTarget, items: Vec<MenuItem>, anchor: (i32, i32)) {
        self.menu = Some(Menu::new(
            target,
            items,
            anchor,
            self.logical_window_rect(),
        ));
        self.window.set_capture(true);
    }

    /// 右键按下：决定弹出哪个菜单。
    ///
    /// 命中优先级：方格 → 侧栏按钮 → 空白（空白不弹菜单）。
    fn on_right_mouse_down(&mut self, x: i32, y: i32) -> bool {
        let (lx, ly) = self.to_logical(x, y);

        // 已经开着菜单时，右键视为"换个位置重开"，先关掉
        self.close_menu();
        self.menu_anchor = (lx, ly);

        // ---- 方格 ----
        // `tile_at` 返回的是**条目下标**（已含 first_item 偏移），
        // 而菜单里要存的是 `visible` 的下标，两者不同，必须换算。
        if let Some(item) = self.layout.tile_at(lx, ly) {
            let items = self.tile_menu_items(item);
            self.open_menu(MenuTarget::Tile(item), items, (lx, ly));
            return true;
        }

        // ---- 侧栏选项卡按钮 ----
        if let Some(tab_index) = self.layout.tab_at(lx, ly) {
            let items = self.tab_menu_items(tab_index);
            self.open_menu(MenuTarget::Tab(tab_index), items, (lx, ly));
            return true;
        }

        // ---- 侧栏空白处：新建按钮 ----
        //
        // 判断放在最后：先让方格和既有按钮吃掉自己的右键，剩下的侧栏区域
        // 才算"空白处"。顺序反了会导致右键按钮时弹出"新建"。
        //
        // 排除设置齿轮：它在侧栏底部，也是"侧栏内"，但右键它谈的不是新建按钮。
        if self.layout.sidebar.contains(lx, ly) && !self.layout.settings_at(lx, ly) {
            let items = vec![MenuItem::new(MenuCommand::AddTab, "新建按钮".to_string())];
            self.open_menu(MenuTarget::Sidebar, items, (lx, ly));
            return true;
        }

        false
    }

    /// 窗口的逻辑坐标矩形，用于把菜单夹在窗口内。
    fn logical_window_rect(&self) -> RectI {
        let (w, h) = self.renderer.dip_size();
        RectI::new(0, 0, w as i32, h as i32)
    }

    /// 方格条目的菜单：打开位置 / 重命名 / 更换图标 / 移动到 / 删除。
    ///
    /// 需要 `&mut self`：右键一个**不在选中集里**的条目时，要把选中集收敛成
    /// 它自己。这不只是"菜单文字好看"的问题 —— 不收敛的话，菜单会显示
    /// "删除（5 项）"，而用户以为删的是他刚右键的那一个（见 `apply_command`）。
    fn tile_menu_items(&mut self, item_index: usize) -> Vec<MenuItem> {
        use MenuCommand::*;

        // ---- 先把选中集收敛到右键目标上 ----
        //
        // 资源管理器行为：右键一个不在选中集里的条目 = 只选中它。
        // 右键一个**已经在**选中集里的条目 = 保留整个选中集，
        // 因为用户显然是冲着"这一批"来的。
        let target_key = self.tile_key_at(item_index);
        if let Some(key) = target_key.as_deref() {
            if !self.selected.contains(key) {
                self.selected.clear();
                self.selected.insert(key.to_string());
            }
        }
        // 批量操作的条目数。目标键取不到时按 1 处理（菜单照常弹，只是不写数量）。
        let batch = match target_key.as_deref() {
            Some(key) if self.selected.contains(key) => self.selected.len(),
            _ => 1,
        };

        let tabs = self.tab_choices();
        let label = |c: &MenuCommand| c.label(&tabs);

        let mut items: Vec<MenuItem> = Vec::new();

        // 壳项专属动词，排在最前面 —— 它们是针对"此电脑/回收站"本身的动作，
        // 和下面那些通用的条目操作不是一类。
        //
        // 判断靠**解析名**（`::{CLSID}`）而不是显示名：显示名是本地化文本，
        // 用户还能重命名，拿它做判断一改就失效。
        if let Some(name) = self.tile_shell_name(item_index) {
            if name.eq_ignore_ascii_case(crate::render::painters::menu::CLSID_THIS_PC) {
                items.push(MenuItem::new(ManageComputer, label(&ManageComputer)));
            } else if name.eq_ignore_ascii_case(crate::render::painters::menu::CLSID_RECYCLE_BIN) {
                items.push(MenuItem::new(EmptyRecycleBin, label(&EmptyRecycleBin)));
            }
        }

        items.extend(
            [OpenLocation, Rename, ChangeIcon, OpenMoveMenu, Delete]
                .into_iter()
                .map(|c| {
                    // 只有**批量命令**（删除、移动到）才写数量。
                    //
                    // 重命名/更换图标/打开位置仍然只作用于右键的那一条，
                    // 给它们加数量就是撒谎 —— 用户会以为那一下能改 5 个名字。
                    let text = if batch > 1 {
                        match &c {
                            Delete => format!("删除（{batch} 项）"),
                            OpenMoveMenu => format!("移动到（{batch} 项）"),
                            _ => label(&c),
                        }
                    } else {
                        label(&c)
                    };
                    MenuItem::new(c, text)
                }),
        );
        items
    }

    /// 取某个可见条目指向的壳项解析名；不是壳项则 `None`。
    fn tile_shell_name(&self, item_index: usize) -> Option<String> {
        let &visible_index = self.visible.get(item_index)?;
        let shortcut = self.library.get(visible_index)?;
        shortcut.shell_name.clone()
    }

    /// 侧栏选项卡的菜单：重命名 / 更换图标 / 删除。
    ///
    /// **"有内容时无法删除"**：该选项卡下还有条目就置灰，并把条目数写进原因 ——
    /// 只置灰不给原因，用户只会以为点了没反应。
    fn tab_menu_items(&self, tab_index: usize) -> Vec<MenuItem> {
        use MenuCommand::*;
        let overrides = self.state.get().entries.clone();

        let deletable = match self.tabs.get(tab_index) {
            // 主界面（全部）不允许删除：它是"显示全部"的视图，删了就无处可看。
            Some(tab) if tab.key != "all" => {
                let count = self.library.count_in(tab, &overrides);
                if count == 0 {
                    None
                } else {
                    Some("该选项卡还有条目")
                }
            }
            Some(_) => Some("主界面不可删除"),
            None => Some("选项卡不存在"),
        };

        let mut items: Vec<MenuItem> = [Rename]
            .into_iter()
            .map(|c| {
                let text = c.label(&[]);
                MenuItem::new(c, text)
            })
            .collect();

        let delete_text = "删除".to_string();
        items.push(match deletable {
            None => MenuItem::new(Delete, delete_text),
            Some(reason) => MenuItem::disabled(Delete, delete_text, reason),
        });

        items
    }

    /// "移动到"的二级菜单：现有选项卡 + 主界面。
    ///
    /// 当前所在的分组要打勾，否则用户不知道"移动"有没有生效。
    fn move_menu_items(&self, item_index: usize) -> Vec<MenuItem> {
        use MenuCommand::*;
        let Some(&visible_index) = self.visible.get(item_index) else {
            return Vec::new();
        };
        let Some(shortcut) = self.library.get(visible_index) else {
            return Vec::new();
        };
        let key = crate::model::library::path_key(shortcut);
        let current = self.state.get().entry(&key).group;

        let tabs = self.tab_choices();
        let mut items: Vec<MenuItem> = tabs
            .iter()
            .map(|(k, name)| {
                MenuItem::new(AssignGroup(Some(k.clone())), name.clone())
                    .checked(current.as_deref() == Some(k.as_str()))
            })
            .collect();

        // 主界面 = 清掉分组，回到按来源归属
        items.push(
            MenuItem::new(AssignGroup(None), "主界面".to_string())
                .checked(current.is_none()),
        );
        items
    }

    /// 可选的分组：所有选项卡的（键, **已解析的显示名**）。
    ///
    /// 必须用 `strings.tab_labels`（已经过 i18n 解析）而不是 `TabDef::name`：
    /// 后者存的是词条键（如 `tab.all`），直接显示会是一串英文键名，
    /// 而配置里若没写 name 就干脆是空的 —— 菜单里就是"没有文字"。
    fn tab_choices(&self) -> Vec<(String, String)> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let label = self
                    .strings
                    .tab_labels
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| t.key.clone());
                (t.key.clone(), label)
            })
            .collect()
    }

    /// 执行菜单里第 `index` 项。
    fn run_menu_command(&mut self, index: usize) -> bool {
        let (target, command) = match self.menu.as_ref() {
            Some(m) => match (m.target.clone(), m.items.get(index)) {
                (t, Some(item)) => (t, item.command.clone()),
                _ => return false,
            },
            None => return false,
        };

        match command {
            // 二级菜单：不关闭，直接换成分组列表
            MenuCommand::OpenMoveMenu => {
                let MenuTarget::Tile(item_index) = target else {
                    return false;
                };
                let anchor = self.menu_anchor;
                let items = self.move_menu_items(item_index);
                if items.is_empty() {
                    return false;
                }
                // 二级菜单沿用一级菜单的捕获（已经开着），所以这里直接替换
                self.menu = Some(Menu::new(
                    MenuTarget::Tile(item_index),
                    items,
                    anchor,
                    self.logical_window_rect(),
                ));
                self.menu_hover = None;
                true
            }
            _ => {
                self.close_menu();
                self.apply_command(target, command)
            }
        }
    }

    /// 把命令落到覆盖表上。
    ///
    /// 注意：这里**只改 `state.toml` 里的覆盖**，绝不触碰 `.lnk` 文件 ——
    /// 见架构文档 §9。所以每个操作都是可逆的。
    fn apply_command(&mut self, target: MenuTarget, command: MenuCommand) -> bool {
        let key = match target {
            MenuTarget::Tile(item_index) => {
                let Some(&visible_index) = self.visible.get(item_index) else {
                    return false;
                };
                let Some(s) = self.library.get(visible_index) else {
                    return false;
                };
                crate::model::library::path_key(s)
            }
            MenuTarget::Tab(_) | MenuTarget::Sidebar => {
                // 选项卡级与侧栏级操作都不走覆盖表
                return self.apply_tab_command(target, command);
            }
        };

        // ---- 批量命令：删除 / 移动到 ----
        //
        // 这两条作用在**整个选中集**上，不是右键点中的那一个。
        //
        // 判断里必须同时检查"目标在选中集里"：`tile_menu_items` 已经会在
        // 右键目标不在选中集时收敛成单条，但菜单弹出与点击之间选中集仍可能
        // 被别的事件改掉。这里的检查是最后一道闸 —— 只看选中集大小而不看目标，
        // 就会出现"右键 A、删掉的却是 B 那一批"。
        if matches!(command, MenuCommand::Delete | MenuCommand::AssignGroup(_)) {
            if self.selected.contains(&key) && self.selected.len() > 1 {
                return match command {
                    // 只写覆盖表的 `hidden = true`，绝不删除或移动用户的真实文件。
                    MenuCommand::Delete => self.hide_selection(),
                    MenuCommand::AssignGroup(group) => self.move_selection_to_group(group),
                    // 外层 `matches!` 已经限定过，这里只是让 match 穷尽
                    _ => false,
                };
            }
        }

        let mut ov = self.state.get().entry(&key);
        match command {
            MenuCommand::Delete => ov.hidden = true,
            MenuCommand::AssignGroup(group) => ov.group = group,
            MenuCommand::OpenLocation => {
                // 只读操作，不写覆盖表
                diag(&format!("OpenLocation 开始: {key}"));
                if let Some(s) = self.state_path_shortcut(&key) {
                    diag(&format!("OpenLocation: 已定位 {}", s.display()));
                    if let Err(e) = crate::shell::launch::reveal(&s) {
                        diag(&format!("OpenLocation: reveal 失败 {e}"));
                    }
                    diag("OpenLocation: reveal 已返回");
                }
                diag("OpenLocation 结束");
                return true;
            }
            MenuCommand::Rename => {
                // 就地编辑，不弹窗。需求："就在把文字部分变为可编辑框"。
                let current = ov
                    .alias
                    .clone()
                    .unwrap_or_else(|| shortcut_display_name(&key));
                self.editing = Some(Editing {
                    target: EditTarget::Tile(key.clone()),
                    // 初值就是要改的字。光标放末尾：用户多半是想续写或删几个字，
                    // 全选状态下一个误触就会把整段清掉。
                    caret: current.chars().count(),
                    text: current,
                });
                return true;
            }
            MenuCommand::ChangeIcon => {
                match crate::shell::picker::pick_icon_file(Some(self.window.hwnd())) {
                    Ok(Some(path)) => match self.import_icon(&path) {
                        Ok(stored) => ov.icon = Some(stored),
                        Err(e) => {
                            eprintln!("导入图标失败: {e}");
                            return false;
                        }
                    },
                    Ok(None) => return false,
                    Err(e) => {
                        eprintln!("选择图标失败: {e}");
                        return false;
                    }
                }
            }
            // 这几条只在选项卡/侧栏那一侧有意义，走到这里说明目标搞错了
            // 这两条只在选项卡/侧栏那一侧有意义，走到这里说明目标搞错了
            MenuCommand::OpenMoveMenu
            | MenuCommand::AddTab
            | MenuCommand::AddShellLocation(_)
            | MenuCommand::AddApp(_)
            | MenuCommand::AddUrl => {
                return false;
            }

            // 壳项专属动词：不改覆盖表，直接调系统。
            MenuCommand::ManageComputer => {
                // 打开"计算机管理"。它需要管理员权限，`ShellExecuteW` 会自己弹 UAC，
                // 所以这里用普通 `open` 而不是 `runas` —— 用 runas 会先弹一次
                // 我们发起的提权确认，再弹系统的，变成两次。
                let _ = crate::shell::launch::open(std::path::Path::new("compmgmt.msc"));
                return true;
            }
            MenuCommand::EmptyRecycleBin => {
                // **必须用 SHEmptyRecycleBinW 而不是删文件**：回收站是壳命名空间，
                // 每个驱动器的 $Recycle.Bin 结构不能手工删。这个 API 还会走系统
                // 自己的确认流程（flags = 0 表示保留确认对话框）。
                let rc = unsafe { windows::Win32::UI::Shell::SHEmptyRecycleBinW(None, None, 0) };
                if rc.is_err() {
                    eprintln!("清空回收站失败: {rc:?}");
                }
                return true;
            }
        }

        self.state.get_mut().set_entry(&key, ov);
        if let Err(e) = self.state.save() {
            eprintln!("保存条目设置失败: {e}");
        }
        // 隐藏/分组会改变可见集合，必须重算
        self.refresh_view();
        true
    }

    /// 从覆盖表键反查 `Shortcut`。键就是 `.lnk` 路径，所以直接找。
    fn state_path_shortcut(&self, key: &str) -> Option<std::path::PathBuf> {
        (0..self.library.total())
            .filter_map(|i| self.library.get(i))
            .find(|s| crate::model::library::path_key(s) == key)
            .map(|s| s.lnk_path.clone())
    }

    /// "添加"按钮的菜单：**就是这五项**（此电脑、回收站、控制面板、网络、电脑设置）。
    ///
    /// 不要往这里加别的东西：这个菜单的语义是"添加一个 shell 位置"，
    /// 混入其它动作会让它变得难以预期。
    fn add_shell_menu_items(&self) -> Vec<MenuItem> {
        use crate::render::painters::menu::{APP_SHORTCUTS, SHELL_LOCATIONS};

        let mut items: Vec<MenuItem> = SHELL_LOCATIONS
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                MenuItem::new(MenuCommand::AddShellLocation(i), (*name).to_string())
            })
            .collect();

        items.extend(
            APP_SHORTCUTS
                .iter()
                .enumerate()
                .map(|(i, (name, _, _))| {
                    MenuItem::new(MenuCommand::AddApp(i), (*name).to_string())
                }),
        );

        // "网址"放最后：它需要用户再输入一次，是这一组里唯一有后续交互的。
        items.push(MenuItem::new(MenuCommand::AddUrl, "网址".to_string()));
        items
    }

    /// 添加一个系统自带程序（记事本 / 计算器 / 画图）的快捷方式。
    fn add_app(&mut self, index: usize) -> bool {
        let Some((name, exe, aumid)) = crate::render::painters::menu::APP_SHORTCUTS
            .get(index)
            .copied()
        else {
            return false;
        };

        let dir = match paths::shortcuts_dir() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("取快捷方式目录失败: {e}");
                return false;
            }
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("创建快捷方式目录失败: {e}");
            return false;
        }
        let dest = dir.join(format!("{name}.lnk"));

        if !aumid.is_empty() {
            // Store 应用：用 `shell:AppsFolder\<AUMID>` 解析成 PIDL 再建快捷方式，
            // 这样图标就是应用自己的。
            //
            // 解析失败时退回"目标 = explorer.exe + 参数 = 该路径" —— 图标会变成
            // explorer 的，但**至少能打开**。宁可图标不对也不要一个点了没反应的条目。
            let target = format!("shell:AppsFolder\\{aumid}");
            if let Err(e) = crate::shell::lnk::create_shell_shortcut(&dest, &target, name) {
                eprintln!("用 PIDL 创建「{name}」失败（{e}），退回 explorer 参数方式");
                let explorer = std::env::var_os("SystemRoot")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
                    .join("explorer.exe");
                if let Err(e) =
                    crate::shell::lnk::create_link(&dest, &explorer, &target, name)
                {
                    eprintln!("创建「{name}」快捷方式失败: {e}");
                    return false;
                }
            }
        } else {
            // 经典桌面程序。用 `SystemRoot` 拼路径而不是写死 `C:\Windows`：
            // 系统装在别的盘符时写死会失效。
            let system_root = std::env::var_os("SystemRoot")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
            let target = system_root.join("System32").join(exe);

            // **先确认文件真的在**。不检查的话会建出一个目标不存在的 `.lnk`：
            // 扫描会把它判为失效而丢掉，接着 `prune_overrides` 又把刚登记的分组
            // 记录当孤儿清掉，用户看到的是一句莫名其妙的"已清理失效条目"，
            // 而不是"这个程序在这台机器上不存在"。
            if !target.exists() {
                eprintln!(
                    "无法添加「{name}」：{} 不存在（这台机器上可能是 Store 版应用）",
                    target.display()
                );
                return false;
            }

            if let Err(e) = crate::shell::lnk::create_link(&dest, &target, "", name) {
                eprintln!("创建「{name}」快捷方式失败: {e}");
                return false;
            }
        }

        self.register_new_entry(&dest);
        true
    }

    /// 把一个刚生成的快捷方式登记到当前选项卡，并重新扫描让它出现。
    fn register_new_entry(&mut self, dest: &std::path::Path) {
        let group = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.key.clone())
            .unwrap_or_else(|| crate::config::model::MAIN_TAB.to_string());
        let key = dest.to_string_lossy().into_owned();
        let mut ov = self.state.get().entry(&key);
        ov.group = Some(group);
        self.state.get_mut().set_entry(&key, ov);
        if let Err(e) = self.state.save() {
            eprintln!("保存条目设置失败: {e}");
        }
        self.rescan();
    }

    /// 创建一个指向 shell 位置的快捷方式，放进程序自己的目录。
    ///
    /// 生成的是**标准 `.lnk`**（目标 `explorer.exe`，参数是 `shell:` 解析名），
    /// 所以它能被现有扫描链路解析、点击后用 `ShellExecuteW` 打开 ——
    /// 不需要为这五类项目引入独立的条目类型。
    fn add_shell_location(&mut self, index: usize) -> bool {
        let Some((name, parsing_name)) = crate::render::painters::menu::SHELL_LOCATIONS
            .get(index)
            .copied()
        else {
            return false;
        };

        let dir = match paths::shortcuts_dir() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("取快捷方式目录失败: {e}");
                return false;
            }
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("创建快捷方式目录失败: {e}");
            return false;
        }

        let dest = dir.join(format!("{name}.lnk"));
        if let Err(e) = crate::shell::lnk::create_shell_shortcut(&dest, parsing_name, name) {
            eprintln!("创建「{name}」快捷方式失败: {e}");
            return false;
        }

        self.register_new_entry(&dest);
        true
    }

    /// 新建一个侧栏按钮，并**立刻进入就地编辑**改名字。
    ///
    /// 直接落在编辑态而不是让用户再右键一次「重命名」：新建出来的按钮
    /// 一定是想改名的，多一步操作没有意义。
    fn add_tab(&mut self) -> bool {
        use crate::config::model::TabDef;

        // 键要唯一。用 `tab2`、`tab3`… 递增找一个没被占用的 ——
        // 键重复会让两个按钮抢同一批条目（分组是按键匹配的）。
        let mut n = 2usize;
        let key = loop {
            let candidate = format!("tab{n}");
            if !self.tabs.iter().any(|t| t.key == candidate) {
                break candidate;
            }
            n += 1;
            if n > 999 {
                return false;
            }
        };

        self.tabs.push(TabDef {
            key,
            name: "新按钮".to_string(),
            ..Default::default()
        });

        let index = self.tabs.len() - 1;
        self.persist_tabs();
        self.strings = Strings::resolve(&self.i18n, &self.tabs);
        self.rebuild_layout();

        // 立刻进入编辑态，光标在末尾
        let label = self
            .strings
            .tab_labels
            .get(index)
            .cloned()
            .unwrap_or_else(|| "新按钮".to_string());
        self.editing = Some(Editing {
            target: EditTarget::Tab(index),
            caret: label.chars().count(),
            text: label,
        });
        true
    }

    /// 选项卡级命令：改配置里的选项卡定义。
    fn apply_tab_command(&mut self, target: MenuTarget, command: MenuCommand) -> bool {
        // 侧栏空白处的"新建按钮"没有具体按钮作目标，单独处理
        if matches!(target, MenuTarget::Sidebar) {
            return match command {
                MenuCommand::AddTab => self.add_tab(),
                MenuCommand::AddShellLocation(i) => self.add_shell_location(i),
                MenuCommand::AddApp(i) => self.add_app(i),
                MenuCommand::AddUrl => {
                    // 网址必须先问用户要内容，所以这里不建条目，只进编辑态。
                    // 锚在 "+" 按钮下方 —— 用户刚点的就是它，视线在那里。
                    self.editing = Some(Editing {
                        target: EditTarget::NewUrl,
                        text: "https://".to_string(),
                        caret: 8,
                    });
                    return true;
                }
                _ => false,
            };
        }

        let MenuTarget::Tab(index) = target else {
            return false;
        };
        match command {
            MenuCommand::Rename => {
                // 就地编辑，和方格条目走同一套。侧栏只有 104px 宽，
                // 编辑框会向右延伸到内容区上方（见 `edit_box_rect`）。
                let Some(label) = self.strings.tab_labels.get(index).cloned() else {
                    return false;
                };
                self.editing = Some(Editing {
                    target: EditTarget::Tab(index),
                    caret: label.chars().count(),
                    text: label,
                });
                return true;
            }
            // 选项卡不支持更换图标：侧栏已改为**纯文字按钮**（需求），
            // 存了图标也不会显示。这条分支留在这里只会让人以为功能还在，
            // 所以连同 `TabDef::icon` 字段一起删掉了。
            MenuCommand::ChangeIcon => return false,
            MenuCommand::Delete => {
                self.tabs.remove(index);
                // 删掉的可能是当前选项卡，索引要跟着收
                if self.active_tab >= self.tabs.len() {
                    self.active_tab = self.tabs.len().saturating_sub(1);
                }
                self.persist_tabs();
                self.refresh_view();
            }
            _ => return false,
        }
        true
    }

    /// 改某个选项卡的定义并落盘。
    fn mutate_config_tab(&mut self, index: usize, f: impl FnOnce(&mut TabDef)) {
        if let Some(tab) = self.tabs.get_mut(index) {
            f(tab);
        }
        self.persist_tabs();
        self.strings = Strings::resolve(&self.i18n, &self.tabs);
        self.rebuild_layout();
    }

    /// 把选项卡列表写回 `config.toml`。
    ///
    /// 说明：TOML 写回会丢掉该文件里的注释。用户的 `config.toml` 是首次运行时
    /// 由程序生成的、没有注释，所以现在这样做没有实际损失；
    /// 将来若改成"用户手写配置"，这里需要换成写入 `state.toml` 的覆盖列表。
    fn persist_tabs(&mut self) {
        self.cfg.tabs = self.tabs.clone();
        if let Err(e) = self.sync_config() {
            eprintln!("保存选项卡配置失败: {e}");
        }
        // 选项卡列表变了（增删改），原来指向被删选项卡的分组会失效，
        // 必须当场修掉，否则要等到下一次扫描才恢复，中间这段时间条目是"消失"的。
        self.normalize_groups();
    }

    fn on_mouse_down(&mut self, x: i32, y: i32) -> bool {
        let (lx, ly) = self.to_logical(x, y);

        // Ctrl 状态**现读**，不自己记按键序列。
        //
        // 用户完全可能先按住 Ctrl、再从别的窗口切回来点我们 ——
        // 那些 Ctrl 的按下动作发生在我们窗口之外，我们收不到，
        // 自己记账就会把"想多选"判成"想启动"，一点就弹出一串程序。
        //
        // 在函数入口读一次就够：下面方格分支与空白分支都在**同一次按下**里用，
        // 中途不可能变。
        let ctrl = crate::platform::hotkey::current_modifiers().ctrl;

        // 任何一次**新的按下**都终结上一次的方格拖拽状态。
        //
        // 第二道保险（第一道在 `Event::MouseLeave`）：方格拖拽没有 `SetCapture`，
        // 鼠标在窗口外松手时我们收不到 `WM_LBUTTONUP`，状态会一直挂着。
        // 残留的 active 状态会让**下一次**松手（哪怕是点侧栏按钮）被当成拖拽落点，
        // 悄悄把选中集挪到别的分组去。这里在每次按下时清一次，
        // 保证"拖拽状态的生命周期绝不跨过两次按下"。
        self.tile_drag = None;
        self.tile_drop = None;
        // 框选同理，而且风险更大：残留的框会在下一次松手时按**过期的几何**
        // 重算选中集，把用户没碰过的条目选进来。
        self.marquee = None;

        // 点击别处 = 提交。
        //
        // **不能吞掉这次点击**：用户点另一个方格时，期望"边提交边切换过去"，
        // 而不是"第一次点击只负责关闭编辑框"。所以提交后继续往下走。
        //
        // `committed` 必须参与最终返回值：`emit` 只在处理器返回 `true` 时
        // 才请求重绘（见 `platform::window::emit`）。点击落在空白处时下面的
        // 分支会返回 `false`，于是**没有任何重绘被请求** —— 编辑框会一直留在
        // 屏幕上，直到鼠标移动之类的事件顺带重绘。用户看到的就是
        // "点了别处，文字要等很久才变"。
        let mut committed = false;
        if self.editing.is_some() {
            self.commit_editing();
            committed = true;
        }

        // 右键菜单是**最上层**的浮层，优先吃点击：否则点到的是被它盖住的内容。
        // 点在菜单外任意处 = 关闭（轻量浮层的通行做法）。
        if self.menu.is_some() {
            let hit = self.menu.as_ref().and_then(|m| m.hit(lx, ly));
            match hit {
                Some(index) => {
                    let enabled = self
                        .menu
                        .as_ref()
                        .and_then(|m| m.items.get(index))
                        .is_some_and(|i| i.enabled);
                    if enabled {
                        return self.run_menu_command(index);
                    }
                    // 点置灰项：不关闭菜单。关闭会让用户以为"点到了"，
                    // 而实际上什么都没发生 —— 让菜单留着更能说明"这里不可用"。
                    return true;
                }
                None => {
                    self.close_menu();
                    return true;
                }
            }
        }

        // 设置面板展开时优先吃点击事件，否则点到的是被它盖住的方格
        if self.settings_open {
            if self.layout.settings_close.contains(lx, ly) {
                self.settings_open = false;
                return true;
            }
            // 底部一排按钮（左→右：退出程序 / 关于更新 / 使用说明）。
            // **都不关闭面板**，除了退出：看完文档、点了关于，多半还要接着调设置；
            // 而"退出程序"本来就要把整个界面收掉。
            if let Some(i) = self
                .layout
                .settings_buttons
                .iter()
                .position(|r| r.contains(lx, ly))
            {
                match i {
                    0 => self.request_exit(),
                    1 => self.open_about(),
                    _ => self.open_help_doc(),
                }
                return true;
            }
            if let Some(row) = self
                .layout
                .settings_rows
                .iter()
                .position(|r| r.contains(lx, ly))
            {
                return self.toggle_setting(row);
            }

            // 基础色色块：点一格就换整套配色。
            if let Some(i) = self
                .layout
                .settings_swatches
                .iter()
                .position(|r| r.contains(lx, ly))
            {
                if let Some(hex) = crate::config::theme::BASE_PALETTE.get(i) {
                    self.cfg.appearance.base_color = (*hex).to_string();
                    if let Err(e) = self.sync_config() {
                        eprintln!("保存基础色失败: {e}");
                    }
                    self.apply_base_color();
                }
                // 点色块**不关闭面板**：用户多半想连着试几个颜色。
                // 换完立刻能看到整个界面变色，这才是选色的正常节奏。
                return true;
            }

            // 其余位置一律收起 —— **包括面板内部的空白区域**。
            // 轻量浮层的惯例是"只有控件吃点击"，空白处即视为关闭意图。
            // 之前写成"只有面板矩形之外才收起"，导致面板里那一大片空白点了没反应。
            self.settings_open = false;
            return true;
        }

        if let Some(button) = self.layout.caption_button_at(lx, ly) {
            match button {
                // 走正常 WM_CLOSE 流程，让 on_close_request 有机会改用隐藏
                CaptionButton::Close => self.window.close(),
                CaptionButton::Minimize => {
                    // 「最小化」直接收进托盘（需求）。
                    //
                    // 用 `SW_HIDE` 而不是 `SW_MINIMIZE`：最小化会在任务栏留一个
                    // 占位按钮，而"进托盘"的语义是**完全交给托盘**托管，
                    // 任务栏上不该再占一格。用户从托盘左键单击即可召回。
                    //
                    // 没开托盘时仍走真正的最小化，否则窗口就再也叫不出来了。
                    if self.cfg.general.show_tray {
                        self.window.hide();
                    } else {
                        self.window.minimize();
                    }
                }
                CaptionButton::HideDesktopIcons => {
                    // 隐藏 / 显示桌面上的文件与快捷方式。
                    //
                    // 是**切换**语义：再点一次恢复显示。判断交给 shell
                    //（它自己记得当前状态），我们不猜。
                    crate::platform::window::toggle_desktop_icons();
                    return true;
                }
                CaptionButton::Add => {
                    // 弹出"添加 shell 位置"菜单。锚在按钮左下角下方，
                    // 而不是鼠标位置 —— 那个按钮很小，从鼠标位置弹会和按钮重叠。
                    let add = self.layout.add;
                    let anchor = (add.x, add.y + add.h);
                    let items = self.add_shell_menu_items();
                    self.menu_anchor = anchor;
                    self.open_menu(MenuTarget::Sidebar, items, anchor);
                    return true;
                }
            }
            return committed;
        }

        if self.layout.settings_at(lx, ly) {
            self.settings_open = !self.settings_open;
            return true;
        }

        // 标题文字：点它进入就地编辑。
        //
        // 用 `title_hit_rect()` 而不是布局给的整条 `title_text` ——
        // 后者一直延伸到 "+" 按钮左侧，点标题右边一大片空白也会进编辑，
        // 范围大得反常（用户报的就是这个）。
        if self.title_hit_rect().contains(lx, ly) {
            let current = self.strings.title.clone();
            self.editing = Some(Editing {
                target: EditTarget::Title,
                caret: current.chars().count(),
                text: current,
            });
            return true;
        }

        if let Some(index) = self.layout.tab_at(lx, ly) {
            self.activate_tab(index);
            // 同时记下"按下的是这个按钮"—— 若鼠标随后被拖到别的按钮上，
            // 松手时就按拖动排序处理，而不是当作普通点击。
            // 这样单击切换和拖动排序**共用同一个按下动作**，不需要额外的长按或手柄。
            self.dragging_tab = Some(index);
            self.drop_tab_target = Some(index);
            return true;
        }

        if let Some(index) = self.layout.tile_at(lx, ly) {
            if let Some(key) = self.tile_key_at(index) {
                if ctrl {
                    // Ctrl + 点击 = 切换这一条的选中态（资源管理器行为）。
                    // `insert` 返回 false 说明本来就在集合里，那就是取消选中。
                    //
                    // 不设"至少要留一条"的下限：右键菜单自己会把不在选中集里的
                    // 目标收敛成单条，不需要额外的兜底；而硬留一条会让用户
                    // 想全部取消时只能靠切换选项卡。
                    if !self.selected.insert(key.clone()) {
                        self.selected.remove(&key);
                    }
                    // **多选不启动**：Ctrl 点击若同时启动程序，
                    // 用户每多选一条就弹出一个窗口，这个功能就没法用了。
                    return true;
                }

                // 无 Ctrl：**先不启动**，把这次按下记成"拖拽候选"，
                // 等松手时按有没有越过阈值来区分单击与拖拽。
                //
                // 在按下时就启动的话，拖拽的第一次移动之前程序已经起来了，
                // 拖一次就等于启动一次 —— 这是必须付的代价。
                //
                // 目标不在选中集里时，把选中集收敛成它自己：这同样是
                // 资源管理器行为。不做这一步的话，"先多选了 3 项、又去点第 4 项"
                // 之后拖拽会把那 3 项一起拖走，而用户以为自己拖的是第 4 项。
                if !self.selected.contains(&key) {
                    self.selected.clear();
                    self.selected.insert(key.clone());
                }
                self.tile_drag = Some(TileDrag {
                    key,
                    index,
                    start: (lx, ly),
                    active: false,
                    over_index: None,
                });
                // 记下起点之后才算"按下完成"。这里返回 true 让界面重绘一次：
                // 收敛选中集可能让别的方格从选中态变回普通态。
                return true;
            }
        }
        // ---- 空白处：单击取消选择，按住拖动则框选 ----
        //
        // 走到这里说明没有命中任何控件（方格、选项卡、标题栏按钮、设置按钮、
        // 菜单项都没中）。这条分支就是"点击空白处"的落地处。
        //
        // **按下即清**，不是等松手 —— 这是资源管理器的语义，也让拖框时立刻
        // 能看到"从空白开始重选"。按着 Ctrl 则不清：那是"在现有选择上追加"，
        // 清掉就变成替换了。
        let base = if ctrl {
            self.selected.clone()
        } else {
            std::collections::HashSet::new()
        };
        let cleared = !ctrl && !self.selected.is_empty();
        self.selected = base.clone();

        // 只在**网格内容区**里起框。在侧栏空白处或标题栏下沿拖拽不该拉出选框 ——
        // 那里没有方格可选，拉出来的框只会盖住界面。
        if self.layout.content.contains(lx, ly) {
            self.marquee = Some(Marquee {
                start: (lx, ly),
                current: (lx, ly),
                base,
            });
        }

        // 清空过选中集就必须重绘：否则方格上的选中态要等到下一次鼠标移动
        // 才顺带消失，看着像"点了空白没反应"。
        cleared || committed
    }

    /// 鼠标抬起。
    ///
    /// 三件事：结束方格拖拽（或把没拖动的这次当成单击启动）、结束侧栏按钮拖动排序。
    ///
    /// **启动时机从"按下"挪到了"抬起"**，这是拖拽必须付的代价：
    /// 按下与拖拽的起点是同一个动作（在方格上按下左键），
    /// 在按下时就启动的话，任何一次拖拽都会顺带启动一整个选中集里的程序。
    /// 代价是单击的响应晚了"松开鼠标"这一下 —— 实测无感，而拖拽从"不能用"
    /// 变成"能用"，这笔交易是划算的。
    fn on_mouse_up(&mut self, x: i32, y: i32) -> bool {
        // ---- 空白处框选：抬起即定稿 ----
        //
        // 每一帧的 `selected` 已经算成「基线 ∪ 框内」了（见 `on_mouse_move`），
        // 所以这里**不用**再算一次。只有"其实是一次点击"那种情况要补一下：
        // 拖出去又拖回来时，最后算出来的选中集还停在鼠标划过的位置，
        // 恢复成基线才对。
        if let Some(m) = self.marquee.take() {
            if m.is_click() {
                self.selected = m.base;
            }
            // 无论如何都要重绘：松手后那条选框线必须消失。
            return true;
        }

        // ---- 方格：拖拽落点 或 单击启动 ----
        //
        // 先 `take` 出来处理：下面任何一条路径都不该留着这个状态，
        // 留下来的话，下一次松手会重复执行这次的动作（重复改分组/重复启动）。
        if let Some(drag) = self.tile_drag.take() {
            self.tile_drop = None;

            if !drag.active {
                // 没越过阈值 = 一次普通单击 → 这才是"点图标启动程序"的地方
                self.launch_tile(drag.index);
                return true;
            }

            let (lx, ly) = self.to_logical(x, y);

            // ---- 落在侧栏选项卡上 = 改分组 ----
            if let Some(tab) = self.layout.tab_at(lx, ly) {
                // 兜底核对：选中集**理应**包含被拖的那一条（按下时就写进去了），
                // 但只要中间任何一个环节出错（比如选中集被别的事件清掉、
                // 而拖拽状态没跟着清），结果就是"拖了半天一项都没动"，
                // 而且没有任何提示。用按下时记下的键核对一次，缺了就补上 ——
                // 宁可只挪这一条，也不能让用户的拖动无声无息地丢掉。
                if !self.selected.contains(&drag.key) {
                    self.selected.clear();
                    self.selected.insert(drag.key.clone());
                }
                if let Some(group) = self.tabs.get(tab).map(|t| t.key.clone()) {
                    self.move_selection_to_group(Some(group));
                }
                // 这次松手**必须吞掉**（返回 true）：不吞的话它会继续走到下面的
                // 侧栏逻辑里，落点上的选项卡会被当成"点了一下侧栏"，
                // 于是拖拽顺带把视图切走了。
                return true;
            }

            // ---- 落在网格里 = 调整排序 ----
            //
            // `over_index` 是**可见集合**里的缝隙下标；`apply_reorder` 自己会
            // 把它换算到当前选项卡的完整顺序上 —— 搜索过滤时这两者不是一回事。
            if let Some(slot) = drag.over_index {
                self.apply_reorder(&drag.key, slot);
                return true;
            }

            // ---- 落在两者之外的空白处 = 放弃这次拖动 ----
            //
            // 这里**不能**退化成"移到主界面"：用户只是把条目拖到空白处
            // 想放弃这次拖动，把它挪到主界面是一个他没要求的改动。
            //
            // 同样必须**吞掉**这次松手：不吞的话它会继续走到下面的侧栏逻辑里，
            // 落点上的选项卡会被当成"点了一下侧栏"，于是拖拽顺带把视图切走了。
            return true;
        }

        // ---- 侧栏按钮：拖动排序 ----
        //
        // 落点和起点相同（或没有落点）时什么都不做 —— 那就是一次普通点击，
        // 切换动作已经在按下时完成了。
        let Some(from) = self.dragging_tab.take() else {
            return false;
        };
        let to = self.drop_tab_target.take();
        let Some(to) = to else {
            return false;
        };
        if to == from || from >= self.tabs.len() || to >= self.tabs.len() {
            return false;
        }

        // 重排：把 from 处的选项卡搬到 to 处
        let moved = self.tabs.remove(from);
        self.tabs.insert(to, moved);

        // 选中的选项卡要跟着走。不跟着的话，用户拖完会发现
        // "选中的还是原来那个位置上的另一个选项卡"，像拖错了。
        if self.active_tab == from {
            self.active_tab = to;
        } else if from < self.active_tab && to >= self.active_tab {
            self.active_tab -= 1;
        } else if from > self.active_tab && to <= self.active_tab {
            self.active_tab += 1;
        }

        self.persist_tabs();
        // 按钮顺序变了，标签表也要重排，否则文字和按钮对不上
        self.strings = Strings::resolve(&self.i18n, &self.tabs);
        self.rebuild_layout();
        true
    }

    /// 切换选项卡。`NewWindow` 模式要等多窗口支持，目前先都按内部切换处理。
    fn activate_tab(&mut self, index: usize) {
        if index >= self.tabs.len() || index == self.active_tab {
            return;
        }
        if self.tabs[index].open == TabOpen::NewWindow {
            eprintln!("弹出新窗口尚未实现，已改为内部切换");
        }
        self.active_tab = index;
        // 换选项卡后内容全变了，滚动位置留着会让用户以为"卡在中间"
        self.scroll = 0.0;
        self.refresh_view();
    }

    /// `Ctrl+滚轮` 调整图标尺寸。
    ///
    /// **量化档位**而不是逐像素连续变化：每个不同的边长都意味着一份新的图标位图，
    /// 连续变化会让缓存瞬间塞满几十种尺寸，内存不可控（见架构设计 §7）。
    fn step_icon_size(&mut self, bigger: bool) -> bool {
        /// 允许的图标边长档位。
        ///
        /// 最低 16：再小图标就看不出是什么了，但用户明确要求能缩到当前最小的一半，
        /// 用来在一屏里塞下尽量多的条目。
        const STEPS: &[u32] = &[16, 24, 32, 48, 64, 96, 128, 192, 256];

        let max = self.cfg.layout.icon_size_max.max(32);
        let current = self.icon_size;

        let next = if bigger {
            STEPS.iter().copied().find(|&s| s > current && s <= max)
        } else {
            STEPS.iter().rev().copied().find(|&s| s < current)
        };

        match next {
            Some(value) => {
                self.icon_size = value;
                self.rebuild_layout();
                true
            }
            // 已经到顶/到底，不动也不重绘
            None => false,
        }
    }

    /// 处理滚轮：`Ctrl` 改图标尺寸，否则滚动内容。
    fn on_wheel(&mut self, x: i32, y: i32, delta: i32, ctrl: bool) -> bool {
        if ctrl {
            return self.step_icon_size(delta > 0);
        }

        // 只在内容区上滚，侧栏与标题栏不响应
        let (lx, ly) = self.to_logical(x, y);
        if !self.layout.content.contains(lx, ly) {
            return false;
        }

        // 一格滚轮 = 120，映射成 60 DIP 的位移
        let step = 60.0;
        let next = (self.scroll - delta as f32 / 120.0 * step).clamp(0.0, self.layout.max_scroll);
        if (next - self.scroll).abs() < 0.01 {
            return false;
        }
        self.scroll = next;
        self.rebuild_layout();
        true
    }

    /// 启动一个方格对应的快捷方式。
    fn launch_tile(&mut self, index: usize) {
        let Some(&item) = self.visible.get(index) else {
            return;
        };
        let Some(shortcut) = self.library.get(item) else {
            return;
        };

        let target = shortcut.lnk_path.clone();
        // 已解析好的目标。文件夹要据此绕开 shell（见 `launch::open_shortcut`）。
        let resolved = shortcut.target.clone();
        let name = shortcut.name.clone();
        let hide_after = self.hide_after_launch();

        diag(&format!("launch_tile 开始: {name} -> {}", target.display()));
        // 启动失败不能崩，只报错；用户点错一个坏快捷方式不该让整个程序挂掉
        match crate::shell::launch::open_shortcut(&target, &resolved) {
            Ok(()) => {
                diag("launch_tile: shell 已返回成功");
                if hide_after {
                    self.window.hide();
                    diag("launch_tile: 已收起窗口");
                }
            }
            Err(e) => diag(&format!("launch_tile: 启动失败 {e}")),
        }
        diag("launch_tile 结束");
    }

    /// 鼠标移动：计时外壳。
    ///
    /// 单独包一层只是为了量"每次移动的处理耗时"，逻辑全在
    /// [`App::on_mouse_move_inner`] 里。之所以要量：拖动时的每一帧
    /// **都必须处理完当前积压的全部移动消息才绘制一次**（见 `window::message_loop`），
    /// 所以单次移动的耗时会被放大成"每帧的额外开销"，是拖动卡顿
    /// 最可能的藏身处，而它恰恰不包含在"绘制耗时"里。
    fn on_mouse_move(&mut self, x: i32, y: i32) -> bool {
        let t = std::time::Instant::now();
        let result = self.on_mouse_move_inner(x, y);
        self.frame_stat
            .note_move(t.elapsed().as_secs_f32() * 1000.0);
        result
    }

    fn on_mouse_move_inner(&mut self, x: i32, y: i32) -> bool {
        let (lx, ly) = self.to_logical(x, y);

        // ---- 方格拖拽中 ----
        //
        // 放在所有悬停处理**之前**并独占这一步：拖拽期间鼠标会划过一格又一格
        // 方格，若让下面的悬停逻辑照常跑，途经的格子会一个个亮起来，
        // 看起来像"拖拽把经过的东西都选中了"。
        if let Some(drag) = self.tile_drag.as_mut() {
            if !drag.active {
                let dx = (lx - drag.start.0).abs();
                let dy = (ly - drag.start.1).abs();
                if dx < TILE_DRAG_THRESHOLD && dy < TILE_DRAG_THRESHOLD {
                    // 还没越过阈值：保持"拖拽候选"状态，什么都不更新。
                    //
                    // 这里**不能**顺手更新悬停高亮 —— 鼠标只在按下那一格里
                    // 抖动了几像素，点亮别的格子会让人以为点错了位置。
                    // 也正因为保持未激活，松手时它仍然会退化成一次普通单击（启动）。
                    return false;
                }
                // 越过阈值 = 真的在拖。
                //
                // 用**距离**而不是时间判断：用户按住不放、手却几乎不动时，
                // 他想要的是启动；只有真的移动了才算拖拽。
                drag.active = true;
            }

            // 落点分两种，**互斥**：
            // * 停在某个选项卡按钮上 = 改分组；
            // * 停在网格里 = 改排序。
            //
            // 不在按钮上时记 `None`，而不是像侧栏排序那样保留上一次的落点 ——
            // 分组是**破坏性更强**的操作（条目会从当前视图里消失），
            // "鼠标已经划走却还能落在上一次那个按钮上"会造成误改分组。
            // 排序同样不保留：鼠标移出去还留着插入线，用户会以为松手一定生效。
            let target = self.layout.tab_at(lx, ly);
            let over = if target.is_some() {
                None
            } else {
                self.layout.drop_slot_at(lx, ly)
            };
            let changed = target != self.tile_drop || over != drag.over_index;
            drag.over_index = over;
            self.tile_drop = target;
            return changed;
        }

        // ---- 空白处框选中 ----
        //
        // 与方格拖拽一样放在悬停处理**之前**并独占这一步：拖框时鼠标会划过
        // 一个又一个方格，让下面的悬停逻辑照常跑会点亮途经的格子，
        // 看起来像"框选把经过的东西都选中了"。
        if let Some(m) = self.marquee.as_mut() {
            if (lx, ly) == m.current {
                return false;
            }
            m.current = (lx, ly);
        }
        if let Some(m) = self.marquee.as_ref() {
            // 还没划出面积就当点击处理：此时`selected` 已经等于基线
            //（按下时设好的），不必每移动一像素就重算一遍。
            if m.is_click() {
                return false;
            }
            let rect = m.rect();
            // 每帧从**基线**重算，而不是往选中集里追加：
            // 追加的话框划过再划回来，格子会一直亮着，没法"取消划中"。
            let mut next = m.base.clone();
            let first = self.layout.first_item;
            for (i, tile) in self.layout.tiles.iter().enumerate() {
                if tile.intersects(&rect) {
                    if let Some(key) = self.tile_key_at(first + i) {
                        next.insert(key);
                    }
                }
            }
            self.selected = next;

            // **必须无条件请求重绘**，不能按"选中集有没有变"来决定。
            //
            // 选框是**跟着鼠标走**的图形，位置每一帧都在变；而"选中集变没变"
            // 完全是另一回事。按后者决定重绘的话，鼠标在同一片区域内移动时
            // 选框会**停在原地**，直到划到下一个方格才跳一下 ——
            // 实测每秒只画 12 帧，而每次移动的处理耗时仅 0.01ms：
            // 既不忙、也不动，那就是"不流畅"。
            //
            // 这条也曾把上一次"拖动卡顿"的排查带偏：日志里那 95% 的空闲时间
            // 就是"根本没请求重绘"造成的，并不是真的省下了什么。
            return true;
        }

        // 正在拖动侧栏按钮：只更新落点提示，不做别的悬停处理。
        // 否则拖动过程中会顺带点亮途经的方格，看起来像误触。
        if let Some(from) = self.dragging_tab {
            // 鼠标离开所有按钮时保留上一次的落点，而不是清空 ——
            // 手抖划出侧栏一下就丢失排序目标会很烦。
            let to = self.layout.tab_at(lx, ly).unwrap_or(from);
            let changed = Some(to) != self.drop_tab_target;
            self.drop_tab_target = Some(to);
            return changed;
        }

        // 菜单展开时，悬停只关心菜单内部 —— 否则鼠标划过菜单时会同时
        // 点亮下面的方格，看起来像两个东西同时被选中。
        if let Some(menu) = self.menu.as_ref() {
            // 置灰项不高亮：高亮会让人以为能点
            let hover = menu
                .hit(lx, ly)
                .filter(|&i| menu.items.get(i).is_some_and(|item| item.enabled));
            let changed = hover != self.menu_hover;
            self.menu_hover = hover;
            return changed;
        }

        // 设置面板展开时，悬停只关心面板内部
        if self.settings_open {
            let row = self
                .layout
                .settings_rows
                .iter()
                .position(|r| r.contains(lx, ly));
            let close = self.layout.settings_close.contains(lx, ly);
            let button = self
                .layout
                .settings_buttons
                .iter()
                .position(|r| r.contains(lx, ly));
            let changed = row != self.hover_setting_row
                || close != self.hover_settings_close
                || button != self.hover_settings_button;
            self.hover_setting_row = row;
            self.hover_settings_close = close;
            self.hover_settings_button = button;
            return changed;
        }

        let caption = self.layout.caption_button_at(lx, ly);
        let tab = self.layout.tab_at(lx, ly);
        let tile = self.layout.tile_at(lx, ly);
        let settings = self.layout.settings_at(lx, ly);

        let changed = caption != self.hover_caption
            || tab != self.hover_tab
            || tile != self.hover_tile
            || settings != self.hover_settings;

        if changed {
            self.hover_caption = caption;
            self.hover_tab = tab;
            self.hover_tile = tile;
            self.hover_settings = settings;
        }
        changed
    }
}

impl Handler for App {
    fn on_event(&mut self, event: Event) -> bool {
        match event {
            Event::Resized { width, height } => {
                self.on_resized(width, height);
                true
            }
            Event::DpiChanged { .. } => {
                self.rebuild_layout();
                true
            }
            Event::SizeMove { entering } => {
                self.resizing = entering;
                true
            }
            Event::MouseDown { x, y } => self.on_mouse_down(x, y),
            Event::KeyDown(vk) => self.on_key_down(vk),
            Event::Char(c) => self.on_char(c),
            Event::FilesDropped { paths } => self.on_files_dropped(paths),
            Event::ExternalDrop => {
                // 落盘已经在拖放目标里做完了，这里只需重新扫描让它出现。
                // 归属分组也由那边按"当前选项卡"登记过了。
                self.rescan();
                true
            }
            Event::Hotkey => {
                // 全局热键 = 呼出 / 隐藏窗口
                self.window.toggle_visible();
                true
            }
            Event::TrayLeftClick => {
                // 左键单击托盘 = 显示 / 隐藏切换
                self.window.toggle_visible();
                true
            }
            Event::TrayRightClick => {
                let visible = self.window.is_visible();
                match crate::platform::tray::show_context_menu(self.window.hwnd(), visible) {
                    Some(crate::platform::tray::command::EXIT) => self.request_exit(),
                    Some(crate::platform::tray::command::SHOW) => self.window.show(),
                    Some(crate::platform::tray::command::HIDE) => self.window.hide(),
                    _ => {}
                }
                true
            }
            Event::FocusLost => {
                // 用户点了桌面或别的程序 —— 消息到不了我们这里，
                // 只能靠失去激活来关菜单。见 `Event::FocusLost` 的说明。
                let mut changed = false;
                // 编辑中的文本也要在这里提交：用户点别的程序正是"点其他地方"，
                // 不提交的话编辑框会一直留着，而且他以为已经改好了。
                if self.editing.is_some() {
                    self.commit_editing();
                    changed = true;
                }
                if self.menu.is_some() {
                    self.close_menu();
                    changed = true;
                }
                changed
            }
            Event::MouseMove { x, y } => self.on_mouse_move(x, y),
            Event::MouseLeave => {
                // 方格拖拽在鼠标离开窗口时**取消**，而不是留着。
                //
                // 方格拖拽没有 `SetCapture`（与侧栏按钮排序同一套取舍：范围本就在
                // 窗口内，移出去即视为放弃）。没有捕获时，鼠标在窗口**外面**松手
                // 我们收不到 `WM_LBUTTONUP` —— 状态会一直挂着，接下来在窗口里的
                // 任何一次松手都会被当成"落点在这里"，把一整个选中集悄悄挪到
                // 别的分组去。取消掉是唯一不会出错的处理。
                let drag = self.tile_drag.take().is_some();
                // 框选同样取消。留着的话选框会跟着光标在窗口外乱飘，
                // 而且下一次在窗口里松手会按一个过期的框去选条目。
                let marquee = self.marquee.take().is_some();
                self.tile_drop = None;

                let changed = drag
                    || marquee
                    || self.hover_caption.is_some()
                    || self.hover_tab.is_some()
                    || self.hover_tile.is_some()
                    || self.hover_settings
                    || self.hover_setting_row.is_some()
                    || self.hover_settings_close
                    || self.hover_settings_button.is_some();
                self.hover_caption = None;
                self.hover_tab = None;
                self.hover_tile = None;
                self.hover_settings = false;
                self.hover_setting_row = None;
                self.hover_settings_close = false;
                self.hover_settings_button = None;
                changed
            }
            Event::Destroyed => {
                self.save_state();
                false
            }
            Event::RightMouseDown { x, y } => self.on_right_mouse_down(x, y),
            Event::MouseUp { x, y, .. } => self.on_mouse_up(x, y),
            Event::Wheel { x, y, delta, ctrl } => self.on_wheel(x, y, delta, ctrl),
        }
    }

    fn hit_test(&self) -> HitTest {
        let mut cfg = self.layout.hit_test(self.maximized);
        // 标题文字可点击，必须排除出标题栏拖动区 —— 不排除的话点它会被判成
        // HTCAPTION，系统发 WM_NCLBUTTONDOWN 去拖窗口，应用层收不到点击，
        // 表现就是"标题点了没反应"。
        //
        // **必须和 `on_mouse_down` 用同一个矩形**：两处若不一致，
        // 就会出现"看起来能点却没反应"或"点到空白却进了编辑"。
        cfg.exclude(self.title_hit_rect());
        cfg
    }

    fn min_size(&self) -> (i32, i32) {
        // 至少要放得下侧栏、一个方格和标题栏按钮
        (420, 300)
    }

    fn paint(&mut self) {
        let t = std::time::Instant::now();
        let result = self.draw();
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        // 先取标志再调 `sample`：`sample` 要可变借用 `frame_stat`，
        // 而 `self.outer_glass()` 借用整个 `self`，直接写在实参里会冲突。
        let glass = self.outer_glass();
        self.frame_stat.sample(ms, glass);
        if let Err(e) = result {
            eprintln!("绘制失败: {e}");
        }
    }

    /// 是否需要托盘图标。窗口层在**资源管理器重启**时要用它决定要不要重挂。
    fn wants_tray(&self) -> bool {
        self.cfg.general.show_tray
    }

    fn on_close_request(&mut self) -> CloseAction {
        // **明确要求退出时一律真退出**，不再走"收进托盘"。
        //
        // 托盘右键菜单的「退出」和设置面板的「退出程序」都会先置上这个标志；
        // 少了它，这两个入口都会退化成"把窗口藏起来" ——
        // 用户点了退出、窗口消失、进程还在，只能去任务管理器结束，
        // 这正是先前的实际表现。
        if self.exit_requested {
            return CloseAction::Exit;
        }

        // 有托盘图标时，关闭只是**收进托盘**，不是退出 ——
        // 这是"最小化进托盘"这类程序的通行语义。真正退出走托盘右键菜单
        // 或设置面板的「退出程序」（两者都会置 `exit_requested`）。
        //
        // 没有托盘图标时（用户在配置里关了）必须真的退出，
        // 否则窗口一藏就再也叫不出来了。
        if self.cfg.general.show_tray {
            self.window.hide();
            return CloseAction::Hide;
        }
        CloseAction::Exit
    }

    fn on_destroyed(&mut self) {
        self.save_state();
    }
}
