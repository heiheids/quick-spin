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
