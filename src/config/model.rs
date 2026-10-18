//! 配置数据模型。
//!
//! 这个文件只描述"配置长什么样"，不含任何读写逻辑（那是 [`super::store`] 的事）。
//! 每个字段都带默认值，因此用户只写想改的那几行即可，
//! 将来新增字段也不会让旧配置解析失败。

// 部分字段（窗口位置、置顶、热键等）要等窗口层接入后才会被用到。
// 窗口落地后请删掉本行。
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::theme::{Backdrop, Scheme};

/// 顶层配置。对应 `%APPDATA%\QuickSpin\config.toml`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub general: General,
    /// 外观：当前配色方案、模糊方式、透明度、圆角
    pub appearance: Appearance,
    /// 布局：尺寸与网格
    pub layout: Layout,
    /// 快捷方式来源
    pub sources: Sources,
    /// 全局热键
    pub hotkey: Hotkey,
    /// 多语言
    pub language: Language,

    /// 侧栏选项卡。数组为空时使用 [`TabDef::defaults`]。
    ///
    /// 放在数组里而不是写死在代码里，是因为需求里明确要求
    /// "可以增加或删除新的按钮、修改按钮图标"。
    pub tabs: Vec<TabDef>,

    /// 用户自定义配色方案。**同名会覆盖内置方案**。
    ///
    /// 内置方案（`classic_blue` / `midnight` / `violet` / `light`）以代码分发、
    /// 不写进配置文件，避免配置臃肿；想改内置方案就在 `[schemes.<同名>]` 里覆盖。
    pub schemes: BTreeMap<String, Scheme>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            general: General::default(),
            appearance: Appearance::default(),
            layout: Layout::default(),
            sources: Sources::default(),
            hotkey: Hotkey::default(),
            language: Language::default(),
            tabs: TabDef::defaults(),
            schemes: BTreeMap::new(),
        }
    }
}

impl AppConfig {
    /// 把外部输入的配置收敛到安全区间。
    ///
    /// 用户手改配置时可能填出离谱的值（比如 `icon_size = 4096`，直接吃掉几百 MB）。
    /// 这类越界值在这里一次性夹紧，渲染代码里就不用到处判错。
    pub fn normalize(&mut self) {
        self.appearance.opacity = self.appearance.opacity.clamp(0.05, 1.0);
        self.appearance.corner_radius = self.appearance.corner_radius.clamp(0.0, 64.0);
        if self.appearance.scheme.trim().is_empty() {
            self.appearance.scheme = super::theme::DEFAULT_SCHEME.to_string();
        }

        // 注意下界是 **0**：`columns = 0` 是"按窗口宽度自动分列"的约定值。
        // 若写成 clamp(1, 16) 会把它夹成 1 列，用户看到的就是单列方格 ——
        // 一个看起来像"布局坏了"、实际是取值域写错的问题。
        self.layout.columns = self.layout.columns.min(16);
        self.layout.columns = self.layout.columns.min(16);
        self.layout.tile_gap = self.layout.tile_gap.min(64);
        self.layout.icon_size = self.layout.icon_size.clamp(16, 256);
        self.layout.padding = self.layout.padding.min(96);
        self.layout.panel_width = self.layout.panel_width.clamp(240, 3840);
        self.layout.panel_height = self.layout.panel_height.clamp(240, 2160);

        // 自定义方案的显示名不能为空，否则设置界面会出现空白项
        for (key, scheme) in self.schemes.iter_mut() {
            if scheme.name.trim().is_empty() {
                scheme.name = key.clone();
            }
        }
    }
}

/// 通用行为开关。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// 跟随系统启动（写 `HKCU\...\Run`）
    pub autostart: bool,
    /// 面板置顶
    pub always_on_top: bool,
    /// 启动一个快捷方式后自动收起面板
    pub hide_after_launch: bool,
    /// 只允许一个实例（重复启动时唤起已有实例）
    pub single_instance: bool,
    /// 显示托盘图标
    pub show_tray: bool,
    /// 点击面板外部时自动隐藏
    pub hide_on_focus_lost: bool,
    /// 标题文字。**留空则用多语言里的默认标题**（`app.title`）。
    ///
    /// 用户自定义的内容放 `config.toml`（用户意图），不放 `state.toml`（运行时状态）——
    /// 与选项卡名同理。
    pub title: String,
    /// 全局热键，形如 `"Ctrl+Alt+Q"`。留空表示不注册。
    ///
    /// 存文本而不是位掩码：这个值用户会想手改，而位掩码得在脑子里反解。
    pub hotkey: String,
}

impl Default for General {
    fn default() -> Self {
        Self {
            autostart: false,
            always_on_top: true,
            hide_after_launch: true,
            single_instance: true,
            show_tray: true,
            hide_on_focus_lost: false,
            title: String::new(),
            hotkey: String::new(),
        }
    }
}

/// 外观设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// 当前配色方案名。可以是内置名，也可以是 `[schemes]` 里的自定义名。
    pub scheme: String,
    /// 模糊方式。选 `Acrylic` 才能自定义色调。
    pub backdrop: Backdrop,
    /// 外圈玻璃效果开关。
    ///
    /// 关掉之后面板变成**实心不透明**，而且不再向系统申请背景模糊 ——
    /// 模糊与透明一起消失，得到一块干净的实心面板。
    /// 与 `backdrop`、`opacity` 的关系：`false` 时这两项都被忽略。
    pub outer_glass: bool,
    /// 全局透明度倍率，乘在面板底色渐变的 alpha 上。
    /// 想整体调透直接用这个，不必去改配色里的每个颜色。
    pub opacity: f32,
    /// 面板圆角，逻辑像素。
    pub corner_radius: f32,
    /// 自定义**基础色**，`#RRGGBB` 形式。留空表示不用自定义配色。
    ///
    /// 非空时它优先于 [`Self::scheme`]：整套主题（侧栏、边框、文字色）
    /// 都由这一个颜色派生出来。存文本而不是数值是为了让人能直接读懂、手改。
    pub base_color: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            scheme: super::theme::DEFAULT_SCHEME.to_string(),
            backdrop: Backdrop::default(),
            outer_glass: true,
            opacity: 1.0,
            corner_radius: 16.0,
            // 空 = 不用自定义配色，走 `scheme` 指定的命名方案
            base_color: String::new(),
        }
    }
}

/// 窗口外框的圆角：**系统侧偏好**与**面板绘制半径**必须同源。
///
/// 为什么要把两者绑成一个值：它们不一致时会出现最难看的一种组合 ——
/// 我们自己把面板画成直角、而 DWM 仍按系统偏好把窗口角部裁圆，
/// 于是四个角上露出后面的桌面（见 `platform::acrylic::set_round_corners` 的说明）。
/// 主窗口与弹出子窗口都必须用同一份，所以这个推导只能有一处（架构文档 §17.9）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CornerStyle {
    /// 是否要求 DWM 给窗口加系统圆角。
    pub rounded: bool,
    /// 面板**期望**的圆角半径（逻辑像素）。平面模式下是 `0`。
    ///
    /// 注意这**不等于**弹窗面板实际该画的半径 —— 见 [`Self::drawn_radius`]。
    /// 主窗口用它（它的面板内缩了发光边距，圆角是自己画出来的、不与 DWM 打架）。
    pub radius: f32,
}

impl CornerStyle {
    /// 弹窗**自己画**的面板该用的半径。**DWM 负责圆角时必须是 `0`。**
    ///
    /// ## 为什么不能沿用 [`Self::radius`]（2026-10-18）
    ///
    /// 窗口的可见轮廓 = **DWM 的裁剪形状 ∩ 我们画的东西**。我们画半径 `R` 的形状
    /// 能盖住 DWM 半径 `r_D` 的裁剪区，**当且仅当 `R ≤ r_D`**（角上我们缩得不能比
    /// 它多）。而 `r_D` **DWM 不提供查询接口** —— 所以任何 `R > 0` 都只是
    /// "在这台机器上碰对了"：换系统版本、换主题、换 `DWMWCP_ROUNDSMALL`
    /// 都可能重新在角上露出月牙（露出来的是桌面，就是使用者报的那道浅色边）。
    ///
    /// **`R = 0` 是唯一对任意 `r_D` 都成立的取值**，所以这里不做折中、
    /// 不去逼近一个查不到的常数（架构文档 §17.14 记了前两次按这个方向修为什么没修好）。
    ///
    /// DWM 不负责圆角时（平面模式，窗口已是直角）裁剪区就是整个矩形，
    /// 那时可以照常画我们自己的圆角。
    pub fn drawn_radius(&self) -> f32 {
        if self.rounded { 0.0 } else { self.radius }
    }
}

impl Appearance {
    /// 由"外圈玻璃是否生效"推出外框圆角，**这是唯一出处**。
    ///
    /// 依据是 `outer_glass`：这是主窗口的**实际**行为（`acyclic::apply` 的第 4 个
    /// 参数一直传的就是它）。2026-10-18 之前这里还有一个 `system_round_corners`
    /// 配置项，但它在全项目里只出现在定义与默认值处、**从未被读取** ——
    /// 一个"看起来有用、实际毫无作用"的开关比没有更糟，已连同 `general.start_hidden`
    /// 一起删除（使用者决定："删掉"）。
    ///
    /// `outer_glass` 为假是"平面模式"：窗口外不申请模糊、不留任何透明区域，
    /// 所以系统圆角也必须一起关掉。
    pub fn corner_style(&self, outer_glass: bool) -> CornerStyle {
        CornerStyle {
            rounded: outer_glass,
            radius: if outer_glass { self.corner_radius } else { 0.0 },
        }
    }
}

/// 布局尺寸。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    /// 网格列数。`0` 表示按窗口宽度自动计算（`Ctrl+滚轮` 缩放时推荐用自动）。
    pub columns: u32,
    /// 方格间距
    pub tile_gap: u32,

    /// 图标边长。
    ///
    /// **内存提示**：位图图标约占 `边长² × 4` 字节。
    /// 40px ≈ 6 KB/个，256px ≈ 256 KB/个。
    /// 因为图标只在可见时按需加载（见架构设计 §7），内存由缓存预算而非条目数决定；
    /// `normalize()` 把上限定在 256 是为了挡住配置被误改到离谱值。
    pub icon_size: u32,
    /// `Ctrl+滚轮` 可调到的最大图标尺寸。缩到比它更大会显著增加单张位图开销。
    pub icon_size_max: u32,
    /// 面板内边距
    pub padding: u32,
    pub panel_width: u32,
    pub panel_height: u32,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            columns: 0,
            tile_gap: 12,
            icon_size: 128,
            icon_size_max: 256,
            padding: 20,
            panel_width: 640,
            panel_height: 880,
        }
    }
}

/// 快捷方式来源。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Sources {
    /// 当前用户开始菜单
    pub user_start_menu: bool,
    /// 所有用户开始菜单
    pub common_start_menu: bool,
    /// 当前用户桌面
    pub user_desktop: bool,
    /// 公共桌面
    pub public_desktop: bool,
    /// 额外扫描的目录
    pub extra_dirs: Vec<PathBuf>,
    /// 是否递归子目录。开始菜单里的文件夹就是分类，建议开启。
    pub recursive: bool,
    /// 是否监视来源目录的变化（实时感知新增/删除快捷方式）。
    /// 需要常驻一个线程加目录句柄，CPU 近 0、内存约 0.1MB。
    pub watch: bool,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            user_start_menu: true,
            common_start_menu: true,
            user_desktop: true,
            public_desktop: true,
            extra_dirs: Vec::new(),
            recursive: true,
            watch: true,
        }
    }
}

/// 全局热键。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkey {
    pub enabled: bool,
    /// 形如 `"Alt+Q"`、`"Ctrl+Shift+Space"`。
    /// 解析成 `RegisterHotKey` 的修饰符 + 虚拟键在 `platform::hotkey`。
    pub toggle_panel: String,
}

impl Default for Hotkey {
    fn default() -> Self {
        Self {
            enabled: true,
            toggle_panel: "Alt+Q".to_string(),
        }
    }
}

/// 多语言。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Language {
    /// 语言代码，如 `"zh-CN"` / `"en-US"`。**留空表示跟随系统**。
    pub locale: String,
    /// 额外语言文件目录。留空则只用 `%APPDATA%\QuickSpin\lang`。
    pub dir: Option<PathBuf>,
}

impl Default for Language {
    fn default() -> Self {
        Self {
            locale: String::new(),
            dir: None,
        }
    }
}

/// 单击选项卡时的行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabOpen {
    /// 在面板内部切换内容（默认）
    InPlace,
    /// 弹出新窗口
    NewWindow,
}

impl Default for TabOpen {
    fn default() -> Self {
        Self::InPlace
    }
}

/// 一个侧栏选项卡。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TabDef {
    /// 唯一键。同时用作多语言回退名（`tab.<key>`）。
    pub key: String,
    /// 显示名。**留空**则依次尝试 `tab.<key>`、`key`。
    pub name: String,
    /// 单击行为：内部切换，还是弹出新窗口
    pub open: TabOpen,
}

impl Default for TabDef {
    fn default() -> Self {
        Self {
            key: String::new(),
            name: String::new(),
            open: TabOpen::InPlace,
        }
    }
}

/// 主窗口选项卡的键。
///
/// 它是**默认分组**：覆盖表里 `group` 为空的条目都归到这里。
/// 抽成常量是因为"默认归哪个分组"在多处（默认配置、视图过滤、拖入）都要用到，
/// 各写各的字面量迟早会不一致。
pub const MAIN_TAB: &str = "main";

impl TabDef {
    /// 默认选项卡：**只有一个主窗口**。
    ///
    /// 需求是"默认保留一个主窗口按钮，自动扫描先不做，全部手动添加"。
    /// 所以不再预置"全部 / 程序 / 桌面"三个按来源自动归类的选项卡 ——
    /// 条目从哪里来由用户拖入决定，不由来源决定，预置那几个只会是空的。
    pub fn defaults() -> Vec<Self> {
        vec![Self {
            key: MAIN_TAB.into(),
            name: "主窗口".into(),
            ..Default::default()
        }]
    }

    /// 侧栏上显示的标题。
    ///
    /// `tr` 负责多语言查表，之所以传进来而不是在这里查：
    /// 本模块属于配置层，不该依赖 i18n 层。
    pub fn label<'a>(&'a self, tr: &'a dyn Fn(&str) -> String) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        // 先试 tab.<key>，命中不了再退回 key 本身
        let keyed = format!("tab.{}", self.key);
        let text = tr(&keyed);
        if text == keyed {
            self.key.clone()
        } else {
            text
        }
    }
}
