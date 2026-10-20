//! 弹出子窗口的**持久化状态**。
//!
//! 只放数据与取值规则，不放任何窗口/绘制逻辑 —— 这样它能在没有窗口
//!（`--selftest`、`--diagnose`）的环境里被完整测试。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 默认尺寸（逻辑像素）。比主窗口窄一圈：它是"某个选项卡的独立视图"。
pub const DEFAULT_W: i32 = 320;
pub const DEFAULT_H: i32 = 420;
/// 尺寸下限。再小连一个方格都放不下，拖动句柄也没地方按。
pub const MIN_W: i32 = 200;
pub const MIN_H: i32 = 160;
/// 尺寸上限。防止手改状态文件写进荒谬值导致布局算出天量矩形。
pub const MAX_W: i32 = 3840;
pub const MAX_H: i32 = 2160;

/// 图标边长的合法区间（DIP）。与主窗口 `Ctrl+滚轮` 的档位范围一致：
/// 最小 16（再小认不出是什么），最大 256（再大图标位图就开始吃内存）。
pub const MIN_ICON: u32 = 16;
pub const MAX_ICON: u32 = 256;

/// 图标边长的档位。**与主窗口 `App::step_icon_size` 必须是同一张表。**
///
/// 两处各写一份的后果不是立刻出错，而是"主窗口能调到 24、弹窗跳不到"这种
/// 说不清的差异 —— 所以这里把它抽成常量，主窗口也引用它。
pub const ICON_STEPS: &[u32] = &[16, 24, 32, 48, 64, 96, 128, 192, 256];

/// 在档位表里走一格。`bigger` 为真就往大走。已经到顶/到底时返回 `None`。
///
/// `max` 是配置里的上限（`config.layout.icon_size_max`）。主窗口与弹窗
/// 都调这一个函数，于是"能调到多大"这件事只有一处定义。
pub fn step_icon(current: u32, bigger: bool, max: u32) -> Option<u32> {
    let max = max.max(32);
    if bigger {
        ICON_STEPS.iter().copied().find(|&s| s > current && s <= max)
    } else {
        ICON_STEPS.iter().rev().copied().find(|&s| s < current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个窗口的图标大小**互不干扰**（使用者 2026-10-20）。
    ///
    /// 这条守的是"弹窗调大小会波及主窗口/其它弹窗"那个 bug：
    /// 现在每个窗口只读自己记录里的 `icon_size`，`0` 才回落到主窗口。
    #[test]
    fn icon_size_is_per_window() {
        let mut a = PopoutWindow::default();
        let mut b = PopoutWindow::default();
        assert_eq!(a.effective_icon_size(64), 64, "没设过应当跟随主窗口");

        a.icon_size = 128;
        // A 自己设过 → 用自己那份；B 没设过 → 仍跟随主窗口。
        assert_eq!(a.effective_icon_size(64), 128);
        assert_eq!(b.effective_icon_size(64), 64, "别的窗口不该被波及");

        // 主窗口改了大小：设过的不动，没设过的跟着走。
        assert_eq!(a.effective_icon_size(32), 128, "设过的窗口不该被主窗口改动");
        assert_eq!(b.effective_icon_size(32), 32);

        b.icon_size = 16;
        assert_eq!(b.effective_icon_size(32), 16);
    }

    /// `0` 必须能穿过 `normalize` —— 否则每个窗口一启动就"自己设过"，
    /// 从此再也不跟随主窗口。
    #[test]
    fn normalize_preserves_follow_sentinel() {
        let mut w = PopoutWindow::default();
        w.icon_size = 0;
        w.normalize();
        assert_eq!(w.icon_size, 0, "0 是'跟随主窗口'，不能被夹成 16");

        // 越界的真实值要收进合法区间
        w.icon_size = 9999;
        w.normalize();
        assert_eq!(w.icon_size, MAX_ICON);
        w.icon_size = 1;
        w.normalize();
        assert_eq!(w.icon_size, MIN_ICON);
    }

    /// 档位表：往上/往下都只走一格，到顶到底返回 `None`。
    #[test]
    fn step_icon_walks_the_table() {
        assert_eq!(step_icon(64, true, 256), Some(96));
        assert_eq!(step_icon(64, false, 256), Some(48));
        assert_eq!(step_icon(16, false, 256), None, "已经最小");
        assert_eq!(step_icon(256, true, 256), None, "已经最大");
        // 上限小于当前档位时，往上走不动
        assert_eq!(step_icon(64, true, 64), None);
        // 上限不小于 32（与主窗口同一条夹紧）
        assert_eq!(step_icon(24, true, 0), Some(32));
    }
}

/// 一个弹出子窗口的状态。
///
/// 字段全部 `pub`：这是纯粹的数据结构，读写都在 `window.rs` 里，
/// 加 getter/setter 只会让那一层变成纯粹的转发。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PopoutWindow {
    /// 上次退出时这个窗口是开着还是关着（需求："记录打开或关闭状态"）。
    ///
    /// 启动时**据此自动恢复**：开着的就重新开出来。注意它和
    /// [`Self::pinned`] 是两件事：`open` 说"要不要存在"，`pinned` 说"主窗口
    /// 收起时它要不要留着"。
    pub open: bool,

    /// 窗口左上角位置。`None` = 还没有过位置，由吸附逻辑或屏幕居中决定。
    ///
    /// 存 `Option` 而不是默认 `(0,0)`：`(0,0)` 是一个**合法位置**，
    /// 用 0 当"没设置"的哨兵会让用户真的把它拖到左上角后位置记不住。
    pub x: Option<i32>,
    pub y: Option<i32>,

    /// 尺寸
    pub width: i32,
    pub height: i32,

    /// 是否吸附在主窗口侧面（需求："默认吸附在主窗口侧面"）。
    ///
    /// `true` 时位置由主窗口的位置推导，[`Self::x`]/[`Self::y`] 不参与；
    /// 用户一拖动窗口就自动变成 `false`（脱离吸附），松手后按自由位置保存。
    pub docked: bool,

    /// 吸附时贴在主窗口的哪一侧。
    ///
    /// 单独存而不是每次现算：用户把主窗口拖到屏幕最右边时，
    /// 右侧放不下，程序会把它翻到左侧 —— 那一翻不该改掉用户"本来想贴右边"
    /// 的意图，所以"期望的侧"与"实际算出来的位置"要分开。
    pub dock_side: DockSide,

    /// "钉住"（需求："定在桌面"）。
    ///
    /// ## 它的确切含义（使用者 2026-10-18 明确收窄）
    ///
    /// **只有一件事：主窗口最小化到托盘时，它不跟着隐藏。**
    ///
    /// 原设计是挂到桌面的 `WorkerW`（最底层、不可移动），使用者已放弃这个方案：
    /// "不用 WorkerW 留在桌面，按照普通窗口处理就行。钉的作用就只是留在桌面
    /// 不随主体最小化。" 所以**不要**再引入 `SetParent` 到 `WorkerW`、
    /// 桌面层、或"不可移动"这些行为 —— 它就是一个普通顶层窗口。
    ///
    /// 副作用是它与 [`Self::docked`] 变成**正交**的两件事：`docked` 管位置
    ///（跟着主窗口摆），`pinned` 管可见性（主窗口收起时留不留着）。
    pub pinned: bool,

    /// 左上角的自命名文字。`None` = 用所属选项卡的名字。
    ///
    /// 存 `Option` 而不是空串：空串是有意义的（用户可能就想让它没有标题），
    /// 而"没设过"和"设成空"在显示上完全不同。
    pub name: Option<String>,

    /// 独立配色：基础色 `#RRGGBB`。空串 = 跟随主窗口的配色。
    ///
    /// 只存一个基础色，整套配色仍由 `config::Scheme::from_base` 派生 ——
    /// 混色规则只该有一处定义（架构文档 §16 同理）。
    pub base_color: String,

    /// 这个窗口**自己的**图标边长（DIP）。`0` = 还没有过，用主窗口的。
    ///
    /// # 为什么必须每个窗口一份（使用者 2026-10-20）
    ///
    /// > 把滚轮调节图标大小分开判断，每个窗口各自设定互不干扰。
    ///
    /// 原先弹窗的方格尺寸是**从主窗口推过来的**（`Content::icon_size`），
    /// 于是在弹窗上 `Ctrl+滚轮` 会改主窗口、连带着改掉所有窗口。
    ///
    /// 用 `0` 当"没设过"的哨兵而不是 `Option<u32>`：`state.toml` 里少一层
    /// 嵌套，而且 0 本来就不是合法的图标边长（最小 16），不会与真实值撞车。
    /// 第一次在弹窗上滚轮时，以**当时主窗口的值**为起点，用户看到的是
    /// "接着现在的大小继续调"，而不是突然跳回默认值。
    pub icon_size: u32,
}

impl PopoutWindow {
    /// 这个窗口该用的图标边长：自己设过就用自己那份，否则用主窗口的。
    ///
    /// 这是"谁优先"的**规范定义**。窗口层 `Popout` 手里只有
    /// `own_icon_size: Option<u32>`（没有整个状态结构），所以它实现的是同一条
    /// 规则的等价形式；`--selftest` 里有检查钉住两者一致。
    ///
    /// 生产路径上这个函数目前只有自检调用（窗口层走的是等价形式）——
    /// 留着它是为了**把规则写在一个能脱离窗口测试的地方**。
    #[allow(dead_code)]
    pub fn effective_icon_size(&self, main_icon_size: i32) -> i32 {
        if self.icon_size == 0 {
            main_icon_size
        } else {
            self.icon_size as i32
        }
    }
}

/// 吸附方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockSide {
    #[default]
    Right,
    Left,
}

impl Default for PopoutWindow {
    fn default() -> Self {
        Self {
            open: false,
            x: None,
            y: None,
            width: DEFAULT_W,
            height: DEFAULT_H,
            // 需求：**默认吸附**
            docked: true,
            dock_side: DockSide::Right,
            pinned: false,
            name: None,
            base_color: String::new(),
            icon_size: 0,
        }
    }
}

impl PopoutWindow {
    /// 收敛手改状态文件带来的非法值。
    ///
    /// 和 `AppState::normalize` 同一个思路：不报错、只夹紧。
    /// 状态文件是人可能手改的，一个荒谬的尺寸不该让界面算出天量矩形。
    pub fn normalize(&mut self) {
        self.width = self.width.clamp(MIN_W, MAX_W);
        self.height = self.height.clamp(MIN_H, MAX_H);
        // 图标边长与主窗口同一个合法区间。`0` 是"没设过"的哨兵，
        // 不能被夹成 16 —— 那样每个窗口一启动就"自己设过"了，
        // 从此再也不跟随主窗口。
        if self.icon_size != 0 {
            self.icon_size = self.icon_size.clamp(MIN_ICON, MAX_ICON);
        }
        // **这里刻意没有"pinned 压过 docked"那条互斥。**
        //
        // 它当年成立的依据是"钉 = 常驻桌面层"（WorkerW），与"吸附在主窗口侧面"
        // 是两种互斥的**落位方式**。使用者 2026-10-18 把"钉"收窄成只管可见性
        //（见 `Self::pinned`），于是 `docked` 管位置、`pinned` 管可见性，
        // 两者正交：**"钉住同时吸附"是合法状态**，不该被这里改掉。
        //
        // 删掉它而不是留着"反正不影响显示"，是因为它会**静默改写用户的记录**：
        // 手改成 `pinned = true, docked = true` 的话，一次 `normalize` 就把
        // `docked` 抹成 false —— 表现为"我明明没拖它，怎么就不吸附了"。
    }

    /// 应当显示的文字。
    pub fn title<'a>(&'a self, tab_label: &'a str) -> &'a str {
        match self.name.as_deref() {
            Some(n) => n,
            None => tab_label,
        }
    }

    /// 把"窗口此刻的状态"与记录里**归装配层管的那部分**合并。
    ///
    /// 抽成纯函数是为了能被 `--selftest` 钉住：自检建不出第二个窗口，
    /// 而这条规则出错的表现（"选了色、当时生效、重启就没了"）只有真的
    /// 重启一次才会暴露 —— 那是最难查的一类。
    ///
    /// 分工的判据是**"窗口层手里有没有这个值"**：
    ///
    /// * `name`：窗口层只有解析好的显示文字（`title`），拿不到原始的
    ///   `Option<String>`（"没设过"与"设成空"是两种意思），所以**必须**用
    ///   记录里的值，否则用户起的名字会被窗口那份默认值抹掉。
    /// * `base_color`：窗口层持有权威值 —— 创建时从记录里取的，
    ///   之后用户每次选色都改它。所以**用窗口的**。
    ///   （早先这里也保留记录值，理由是"窗口那份是默认值"；那个理由在
    ///   窗口开始持有它之后就消失了，而保留会让选色被静默丢弃。）
    /// * 位置 / 尺寸 / 吸附 / 钉住：只有窗口自己知道，用窗口的。
    pub fn merge_persisted(fresh: Self, old: &Self) -> Self {
        Self {
            name: old.name.clone(),
            ..fresh
        }
    }
}

/// 全部弹出子窗口的状态。
///
/// 键是选项卡的 `key`（配置里的稳定标识），所以增删选项卡、调整顺序都不会
/// 让已有窗口的状态错位。用 `BTreeMap` 与 `AppState::entries` 同理：
/// 只为让 `state.toml` 的字节稳定。
///
/// 落盘形态是一层嵌套表（`[popout.windows.'<tab key>']`），
/// 而不是把键平铺到顶层 —— 顶层是 `AppState` 的字段名空间，
/// 平铺进去会让 `state.toml` 里"字段"和"选项卡键"混在一起。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PopoutState {
    pub windows: BTreeMap<String, PopoutWindow>,
}

impl PopoutState {
    /// 取某个选项卡的窗口状态；没有就按默认值建一个（**不写入**）。
    ///
    /// 不在这里写入：读取方可能只是想知道"要不要在启动时打开它"，
    /// 顺手往状态表里塞一条会让 `state.toml` 随访问而长大。
    pub fn get(&self, tab_key: &str) -> PopoutWindow {
        self.windows.get(tab_key).cloned().unwrap_or_default()
    }

    /// 写入并归一。
    pub fn set(&mut self, tab_key: &str, mut value: PopoutWindow) {
        value.normalize();
        self.windows.insert(tab_key.to_string(), value);
    }

    /// 启动时应当打开的窗口（按选项卡 key 排序，顺序稳定）。
    ///
    /// 排序是刻意的：多个窗口同时创建时，若按 `HashMap` 那种随机顺序建，
    /// Z 序与吸附翻边会每次不同，看起来像"位置自己在变"。
    pub fn to_open(&self) -> Vec<(&str, &PopoutWindow)> {
        self.windows
            .iter()
            .filter(|(_, w)| w.open)
            .map(|(k, w)| (k.as_str(), w))
            .collect()
    }

    /// 清掉指向已不存在选项卡的记录。
    ///
    /// 与 `App::prune_overrides` 同一个理由：选项卡被删掉后，它的窗口状态
    /// 永远不会再被匹配到，却一直留在文件里。调用方传入当前有效的 key 集合。
    pub fn retain_tabs(&mut self, live: &std::collections::HashSet<String>) -> usize {
        let before = self.windows.len();
        self.windows.retain(|k, _| live.contains(k));
        before - self.windows.len()
    }

    /// 是否一条记录都没有。`state.toml` 的 `skip_serializing_if` 用它。
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
}
