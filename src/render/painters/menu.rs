//! 右键菜单：模型、几何、绘制。
//!
//! ## 为什么几何和绘制放在同一个文件
//!
//! 主界面的几何放在 `ui::layout`，因为布局是全局耦合的（方格要填满行、
//! 滚动要影响行数）。右键菜单不是 —— 它是一个**自包含的浮层**：
//! 位置由鼠标锚点决定，尺寸由条目文字长度决定，和主布局没有任何关系。
//! 把它拆到两个文件只会让"改一处忘一处"的概率翻倍，所以放一起。
//!
//! ## 状态机
//!
//! `Menu` 一旦存在就表示菜单已展开。命中判断（[`Menu::hit`]）既用于
//! 绘制时的悬停高亮，也用于点击时决定执行哪一项 —— 两处共用同一份矩形，
//! 保证"看到的位置"和"点到位置"永远一致。

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Direct2D::*;

use crate::config::theme::Scheme;
use crate::error::{Result, Win32Ext};
use crate::platform::hittest::RectI;
use crate::render::{Role, TextEngine};

use super::{rounded_i, to_d2d};

/// 菜单项被点击后要执行的动作。
///
/// 用带数据的枚举而不是纯动作名：二级菜单（"移动到"的目标分组）需要
/// 把选中的选项卡键带回来。纯动作名就得靠"记住了第几个菜单项"来推断，
/// 那种间接映射一旦菜单项顺序变了就会执行错动作。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MenuCommand {
    /// 在资源管理器里打开所在位置
    OpenLocation,
    /// 重命名（改本程序里的显示别名）
    Rename,
    /// 更换图标（改本程序里的显示图标）
    ChangeIcon,
    /// 展开"移动到"的二级菜单
    OpenMoveMenu,
    /// 把条目移到指定分组；`None` 表示主界面
    AssignGroup(Option<String>),
    /// 删除（仅本程序隐藏）
    Delete,
    /// 新建一个侧栏按钮
    AddTab,
    /// 添加一个 shell 位置（此电脑、回收站…）。
    ///
    /// 参数是 `SHELL_LOCATIONS` 里的下标，而不是字符串：
    /// 字符串是 `&'static str`，放进枚举会让枚举带生命周期；用下标最省事，
    /// 而这张表是本文件里的常量，下标不会失效。
    AddShellLocation(usize),
    /// "此电脑"专属：打开计算机管理。
    ManageComputer,
    /// "回收站"专属：清空回收站。
    EmptyRecycleBin,
    /// 添加一个系统自带程序（记事本 / 计算器 / 画图）。参数是下标。
    AddApp(usize),
    /// 添加一个自定义网址（需要用户输入，见 `App` 里的处理）。
    AddUrl,
    /// 侧栏按钮专属：把该选项卡弹成独立窗口 / 关掉它的独立窗口。
    ///
    /// **一个命令而不是两个**（打开 / 关闭）：菜单里到底显示哪一个，
    /// 取决于该选项卡当前有没有弹出窗口，而那要读运行时状态；
    /// 命令本身只表达"切换"，把判断留在 `App` 里。
    /// 参数是选项卡 key。
    TogglePopout(String),
}

/// 可添加的系统自带程序：`(菜单文字, System32 下的可执行文件名, Store 应用 AUMID)`。
///
/// 两个启动标识至少要有一个：
///
/// * **exe** 用于经典桌面程序。只写文件名不写完整路径 ——
///   `%SystemRoot%\System32` 每台机器都一样，在 App 里用 `SystemRoot` 环境变量
///   拼出来比硬编码 `C:\Windows` 稳。
/// * **aumid** 用于 Store 应用。Win11 里**画图已经不是 `System32\mspaint.exe` 了**
///   （该文件根本不存在，是 Store 包 `Microsoft.Paint`），只能通过
///   `shell:AppsFolder\<AUMID>` 启动。计算器虽然还留着 `calc.exe`，
///   但那只是个转发存根，同样属于 Store 应用。
pub const APP_SHORTCUTS: &[(&str, &str, &str)] = &[
    ("记事本", "notepad.exe", ""),
    ("计算器", "calc.exe", ""),
    ("画图", "", "Microsoft.Paint_8wekyb3d8bbwe!App"),
];

/// "此电脑"的 CLSID 解析名。
pub const CLSID_THIS_PC: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";
/// "回收站"的 CLSID 解析名。
pub const CLSID_RECYCLE_BIN: &str = "::{645FF040-5081-101B-9F08-00AA002F954E}";

/// 可添加的 shell 位置：`(菜单文字, 传给 explorer 的解析名)`。
///
/// 用 `explorer.exe` + `shell:` 参数生成**标准 `.lnk`**，
/// 这样它能被现有扫描链路解析、点击后由 `ShellExecuteW` 打开，
/// 不需要为这五类项目另立一套条目类型。
pub const SHELL_LOCATIONS: &[(&str, &str)] = &[
    ("此电脑", "shell:MyComputerFolder"),
    ("回收站", "shell:RecycleBinFolder"),
    ("控制面板", "shell:ControlPanelFolder"),
    ("网络", "shell:NetworkPlacesFolder"),
    // 这是 URI 协议而不是 shell 文件夹，所以同样走 explorer 的参数形式。
    // 若某些系统上拉不起来，改成以 `ms-settings:` 为快捷方式目标即可。
    ("电脑设置", "ms-settings:"),
];

impl MenuCommand {
    pub fn label(&self, tabs: &[(String, String)]) -> String {
        match self {
            Self::OpenLocation => "打开位置".into(),
            Self::Rename => "重命名".into(),
            Self::ChangeIcon => "更换图标".into(),
            Self::OpenMoveMenu => "移动到".into(),
            Self::Delete => "删除".into(),
            Self::AddTab => "新建按钮".into(),
            Self::ManageComputer => "管理".into(),
            Self::EmptyRecycleBin => "清空回收站".into(),
            Self::AddApp(i) => APP_SHORTCUTS
                .get(*i)
                .map(|(name, _, _)| (*name).to_string())
                .unwrap_or_else(|| "（未知程序）".into()),
            Self::AddUrl => "网址".into(),
            // 文案由 `App` 按"该选项卡当前有没有弹出窗口"填进 `MenuItem::text`，
            // 所以这里的兜底值基本用不到；留着是为了 `label()` 的穷尽匹配。
            Self::TogglePopout(_) => "弹出子窗口".into(),
            Self::AddShellLocation(i) => SHELL_LOCATIONS
                .get(*i)
                .map(|(name, _)| (*name).to_string())
                .unwrap_or_else(|| "（未知位置）".into()),
            Self::AssignGroup(None) => "主界面".into(),
            Self::AssignGroup(Some(key)) => tabs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| key.clone()),
        }
    }

    /// 是否具有破坏性（会让条目在本程序里消失）。绘制时用警示色区分。
    pub fn is_destructive(&self) -> bool {
        matches!(self, Self::Delete)
    }
}

/// 右键的目标。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MenuTarget {
    /// 内容方格里的条目下标（指向 `App::visible` 的下标）
    Tile(usize),
    /// 侧栏选项卡下标
    Tab(usize),
    /// 侧栏的空白处。目前只有"新建按钮"一项。
    Sidebar,
}

/// 菜单里的一项。
pub struct MenuItem {
    pub command: MenuCommand,
    /// 已经渲染好的文案。在这里存字符串而不是每次现算：
    /// 分组菜单的文案依赖选项卡列表，绘制时再算会引入一次不必要的借用。
    pub text: String,
    /// 是否可点击。不可点时置灰，并显示 `reason`。
    pub enabled: bool,
    /// 置灰的原因（画在右侧的浅色小字）。
    /// 不给原因的话用户只会觉得"点了没反应"，而不是"这里不允许"。
    pub reason: &'static str,
    /// 是否标记为"当前所在"（分组菜单里用）。
    pub checked: bool,
}

impl MenuItem {
    pub fn new(command: MenuCommand, text: String) -> Self {
        Self {
            command,
            text,
            enabled: true,
            reason: "",
            checked: false,
        }
    }

    pub fn disabled(command: MenuCommand, text: String, reason: &'static str) -> Self {
        Self {
            command,
            text,
            enabled: false,
            reason,
            checked: false,
        }
    }

    pub fn checked(mut self, on: bool) -> Self {
        self.checked = on;
        self
    }
}

/// 浮层尺寸参数。
const ROW_HEIGHT: i32 = 30;
const PADDING_X: i32 = 14;
const PADDING_Y: i32 = 6;
/// 文字宽度按"每个字符约 13px"估算菜单宽度。
///
/// 不用 DirectWrite 精确测量：那需要先建 `IDWriteTextLayout`，
/// 而菜单宽度必须在**鼠标按下的那一刻**就定下来（要判断点击落在哪一项）。
/// 估算宽一点无害（只是右边多留些空白），估窄了才会把字裁掉 ——
/// 所以这里按中文字符的宽度上限取值。
const CHAR_WIDTH: i32 = 14;
/// 菜单与鼠标锚点的偏移，避免菜单盖住光标本身
const ANCHOR_OFFSET: i32 = 2;
/// 置灰原因与主文字之间的间隔
const REASON_GAP: i32 = 18;
/// 对勾标记占的宽度（比实际大一点，让文字起点统一）
const CHECK_WIDTH: i32 = 18;

pub struct Menu {
    pub target: MenuTarget,
    pub items: Vec<MenuItem>,
    /// 每一项的矩形，与 `items` 一一对应
    pub rows: Vec<RectI>,
    /// 整个菜单的外框
    pub frame: RectI,
}

impl Menu {
    /// 在锚点处构造菜单。会自动把菜单收进窗口边界内 ——
    /// 否则在窗口右下角右键时菜单会有一半在窗口外面，够不着。
    pub fn new(
        target: MenuTarget,
        items: Vec<MenuItem>,
        anchor: (i32, i32),
        bounds: RectI,
    ) -> Self {
        let widest = items
            .iter()
            .map(|item| {
                // 前面可能有一个对勾标记的位置，宽度上留出来
                let main = item.text.chars().count() as i32 * CHAR_WIDTH + CHECK_WIDTH;
                let reason = if item.enabled {
                    0
                } else {
                    item.reason.chars().count() as i32 * CHAR_WIDTH + REASON_GAP
                };
                main + reason
            })
            .max()
            .unwrap_or(80);

        let width = widest + PADDING_X * 2;
        let height = items.len() as i32 * ROW_HEIGHT + PADDING_Y * 2;

        // 先按锚点右下展开，再夹回窗口内
        let mut x = anchor.0 + ANCHOR_OFFSET;
        let mut y = anchor.1 + ANCHOR_OFFSET;
        if x + width > bounds.x + bounds.w {
            // 右边放不下就翻到光标左侧
            x = anchor.0 - width - ANCHOR_OFFSET;
        }
        if y + height > bounds.y + bounds.h {
            y = anchor.1 - height - ANCHOR_OFFSET;
        }
        // 夹紧：窗口太小时翻边也放不下，宁可贴边也不要露到外面
        x = x.clamp(bounds.x, (bounds.x + bounds.w - width).max(bounds.x));
        y = y.clamp(bounds.y, (bounds.y + bounds.h - height).max(bounds.y));

        let frame = RectI::new(x, y, width, height);
        let rows = (0..items.len())
            .map(|i| {
                RectI::new(
                    x + PADDING_X / 2,
                    y + PADDING_Y + i as i32 * ROW_HEIGHT,
                    width - PADDING_X,
                    ROW_HEIGHT,
                )
            })
            .collect();

        Self {
            target,
            items,
            rows,
            frame,
        }
    }

    /// 命中第几项。返回 `None` 表示点在菜单之外。
    pub fn hit(&self, x: i32, y: i32) -> Option<usize> {
        if !self.frame.contains(x, y) {
            return None;
        }
        self.rows.iter().position(|r| r.contains(x, y))
    }
}

pub struct MenuPainter {
    brush: Option<ID2D1SolidColorBrush>,
}

impl MenuPainter {
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

    pub fn draw(
        &self,
        ctx: &ID2D1DeviceContext,
        text: &mut TextEngine,
        menu: &Menu,
        scheme: &Scheme,
        hover: Option<usize>,
    ) -> Result<()> {
        let Some(brush) = &self.brush else {
            return Ok(());
        };
        let no_style: Option<&ID2D1StrokeStyle> = None;
        let frame = menu.frame;

        // SAFETY: COM 调用；结构体都是本函数栈上的局部量。
        unsafe {
            // 菜单必须**完全不透明**：它是浮在内容之上的操作面板，
            // 背后透出方格图标会让文字读不清。
            brush.SetColor(&to_d2d(scheme.panel.midpoint().with_alpha(255)));
            ctx.FillRoundedRectangle(&rounded_i(frame, 8.0), brush);

            // 描边：用派生出的"同色系更深色"（`tab_active_border`），
            // 且**不压透明度**。
            //
            // 菜单的填充色**就是面板色本身**（上面那行 `panel.midpoint()`），
            // 背后又是同一块面板 —— 于是"哪里是菜单的边界"完全落在这条线上，
            // 它是唯一的分界信号，不能被削弱。
            //
            // 原来这里是 `with_alpha(0x66)`（40%）。叠在面板上只差十几个色阶，
            // 实测反馈正是"不容易看清边界"：菜单看着像没有边框，和背后的面板
            // 糊成一片。
            brush.SetColor(&to_d2d(scheme.tab_active_border));
            ctx.DrawRoundedRectangle(&rounded_i(frame, 8.0), brush, 1.0, no_style);

            // 外侧再补一圈描边，与内侧的深色线并排成"双线"。
            //
            // 颜色取 `overlay_ring` —— 与"鼠标划过方格图标时那圈浅色高亮"
            // 同色（需求明确要求两者一致，见该字段的说明）。它是半透明叠色，
            // 画在这里会与背后的面板混合，混出来的正是那圈高亮的颜色。
            //
            // 往外让出**整整一个线宽**（1.0）：D2D 的描边以**路径**为中心，
            // 内侧线因此占 `[-0.5, +0.5]`，外圈路径放到 `+1.0` 正好占
            // `[+0.5, +1.5]` —— 两条线紧贴而不重叠。只让 0.5 的话它们会各占
            // 一半、互相盖掉一层，两种颜色混在一起就脏了。
            //
            // 圆角跟着一起放大（8.0 → 9.0）：只放大矩形而不放大圆角的话，
            // 外圈的四个角会比内圈"更方"，转角处会露出内圈的痕迹。
            let outer = RectI::new(frame.x - 1, frame.y - 1, frame.w + 2, frame.h + 2);
            brush.SetColor(&to_d2d(scheme.overlay_ring));
            ctx.DrawRoundedRectangle(&rounded_i(outer, 9.0), brush, 1.0, no_style);

            // 悬停高亮只画在可用项上：置灰项高亮会让人以为能点
            if let Some(i) = hover {
                if let Some(row) = menu.rows.get(i) {
                    if menu.items.get(i).is_some_and(|item| item.enabled) {
                        brush.SetColor(&to_d2d(scheme.tile_hover));
                        ctx.FillRoundedRectangle(&rounded_i(*row, 6.0), brush);
                    }
                }
            }
        }

        // ---- 文字 ----
        for (i, row) in menu.rows.iter().enumerate() {
            let Some(item) = menu.items.get(i) else {
                continue;
            };
            let color = if !item.enabled {
                // 置灰：用次要色再压一半不透明度，和可用项拉开明显差距
                scheme.text_secondary.with_alpha(0x70)
            } else if item.command.is_destructive() {
                // 破坏性操作用警示色，避免误点
                scheme.accent
            } else {
                scheme.text_primary
            };

            let text_rect = (
                row.x as f32 + CHECK_WIDTH as f32,
                row.y as f32,
                (row.w as f32 - CHECK_WIDTH as f32 - 8.0).max(20.0),
                row.h as f32,
            );
            text.draw(ctx, &item.text, Role::Body, text_rect, color)?;

            // 当前所在分组的对勾。不标出来的话用户不知道"移动"有没有生效。
            if item.checked {
                // SAFETY: COM 调用；坐标都是栈上局部量。
                unsafe {
                    if let Some(brush) = &self.brush {
                        brush.SetColor(&to_d2d(scheme.accent));
                        let cx = row.x as f32 + CHECK_WIDTH as f32 / 2.0 + 2.0;
                        let cy = row.y as f32 + row.h as f32 / 2.0;
                        ctx.DrawLine(
                            windows_numerics::Vector2 { X: cx - 4.0, Y: cy },
                            windows_numerics::Vector2 {
                                X: cx - 1.0,
                                Y: cy + 3.5,
                            },
                            brush,
                            1.6,
                            no_style,
                        );
                        ctx.DrawLine(
                            windows_numerics::Vector2 {
                                X: cx - 1.0,
                                Y: cy + 3.5,
                            },
                            windows_numerics::Vector2 {
                                X: cx + 4.5,
                                Y: cy - 3.5,
                            },
                            brush,
                            1.6,
                            no_style,
                        );
                    }
                }
            }

            if !item.enabled && !item.reason.is_empty() {
                // 原因贴在同一行右侧
                text.draw(
                    ctx,
                    item.reason,
                    Role::Caption,
                    (
                        text_rect.0 + text_rect.2 * 0.45,
                        text_rect.1,
                        text_rect.2 * 0.55,
                        text_rect.3,
                    ),
                    scheme.text_secondary.with_alpha(0x70),
                )?;
            }
        }

        Ok(())
    }
}

impl Default for MenuPainter {
    fn default() -> Self {
        Self::new()
    }
}
