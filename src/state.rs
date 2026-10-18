//! 程序运行时状态。
//!
//! 与 `config.toml` 的分工（见 `docs/ARCHITECTURE.md` §5）：
//!
//! * `config.toml` 是**用户写的意图**，程序只在首次生成、以及用户在设置面板
//!   显式点"保存为默认配置"时才写它；
//! * 本模块对应的 `state.toml` 是**程序自己的记忆**（窗口尺寸、当前选项卡、
//!   图标大小），随用随写。
//!
//! 之所以要分开：TOML 写入会丢失注释，程序若反复重写 `config.toml`，
//! 用户手写的注释和排版会被冲干净。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::store::TomlStore;
use crate::error::Result;
use crate::paths;

/// 单个条目的**程序内覆盖**。
///
/// 见 `docs/ARCHITECTURE.md` §9：右键菜单里的删除 / 重命名 / 更换图标 / 移动到
/// 全部只改这份覆盖，**绝不动用户的 `.lnk` 文件**。
/// 因此全部操作可逆 —— 清掉这张表就恢复原样。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EntryOverride {
    /// 在本程序里隐藏该条目（"删除"）。
    /// 用 `skip_serializing_if` 是为了让状态文件里只留下真正改过的字段，
    /// 否则几百个条目会把 `state.toml` 撑得很难读。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// 显示用的别名（"重命名"）。`None` 表示用快捷方式原本的名字。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// 覆盖显示图标（"更换图标"）。存图标文件路径，不改 `.lnk` 本身。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<PathBuf>,
    /// 归属的选项卡键（"移动到"）。`None` 表示跟来源走。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// 显示顺序。`None` 表示"从未被排过序"。
    ///
    /// 排序规则（见 `Library::view`）：**没有 `order` 的按扫描顺序排在前面，
    /// 有 `order` 的按数值升序排在后面**。
    ///
    /// 这样定是为了同时满足两件事：
    /// 1. 新添加的条目取"现有最大值 + 1"，于是天然落在末尾 —— 用户加进来的
    ///    东西总在最后，不会插进已有的排列里；
    /// 2. 用户第一次拖动排序时，程序会把当时**该选项卡下全部**条目的顺序落成
    ///    显式数值（见 `App::apply_reorder`），此后顺序就完全由这张表决定，
    ///    不再依赖扫描顺序 —— 否则拖动一个"没有 order"的条目会把它甩到最后，
    ///    而不是落到用户松手的那个位置。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<u32>,
}

impl EntryOverride {
    /// 是否什么都没改。
    ///
    /// 全默认的覆盖应当从表里删掉，否则 `state.toml` 会随着
    /// "改回去"的操作越积越长。
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// 程序自管状态，落在 `%APPDATA%\QuickSpin\state.toml`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    pub window: WindowState,
    /// 当前选中的选项卡键。空表示用第一个。
    pub active_tab: String,
    /// 当前图标尺寸（`Ctrl+滚轮` 的结果）。`0` 表示跟随 `config.layout.icon_size`。
    pub icon_size: u32,
    /// 当前配色方案名。空表示跟随 `config.appearance.scheme`。
    pub scheme: String,

    // ---- 设置面板产生的覆盖项 ----
    //
    // `None` 表示"跟随 config.toml"。之所以不让设置面板直接改 config.toml：
    // TOML 写回会丢掉用户手写的注释，程序不该动用户的文件。
    // 用户想要固化为默认值时，另有显式的"保存为默认"动作（后续实现）。
    /// 外圈玻璃效果
    pub outer_glass: Option<bool>,
    /// 窗口置顶
    pub always_on_top: Option<bool>,
    /// 启动程序后收起面板
    pub hide_after_launch: Option<bool>,

    /// 条目覆盖表。键是 `.lnk` 的**完整路径**（见架构文档 §9.2 的取舍说明）。
    ///
    /// 用 `BTreeMap` 而不是 `HashMap`：**只为让文件内容稳定**。
    /// `HashMap` 的迭代顺序由每个进程随机的哈希种子决定，于是每保存一次
    /// `state.toml` 里 `[entries.*]` 各小节的**书写顺序**都可能变，
    /// 内容没变而字节全变 —— 自己 diff、外部工具比对、增量备份都会误报。
    /// `BTreeMap` 按 key 排序输出，改了什么就只动那一处。
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub entries: BTreeMap<String, EntryOverride>,

    /// 弹出子窗口的状态（每个选项卡最多一个，可同时开多个）。
    ///
    /// 键是选项卡的 `key`，与 `entries` 里 `group` 用的是同一套标识。
    /// 为空时不写进文件（`skip_serializing_if`），保持 `state.toml` 干净。
    #[serde(default, skip_serializing_if = "crate::popout::PopoutState::is_empty")]
    pub popout: crate::popout::PopoutState,
}

impl AppState {
    /// 取某条路径的覆盖；没有就返回默认值。
    pub fn entry(&self, path: &str) -> EntryOverride {
        self.entries.get(path).cloned().unwrap_or_default()
    }

    /// 写入某条路径的覆盖。**全默认的覆盖会被删除**，避免状态文件膨胀。
    pub fn set_entry(&mut self, path: &str, value: EntryOverride) {
        if value.is_empty() {
            self.entries.remove(path);
        } else {
            self.entries.insert(path.to_string(), value);
        }
    }
}

impl AppState {
    /// 收敛明显非法的窗口尺寸。
    ///
    /// 来源可能是手改的状态文件，也可能是"上次在 4K 副屏关窗、这次只插了一块 1080p 屏"。
    /// 完整的"是否落在某块屏幕内"检查要等窗口层接入后做，这里先保证数值不离谱。
    pub fn normalize(&mut self) {
        if self.window.width > 0 {
            self.window.width = self.window.width.clamp(320, 7680);
        }
        if self.window.height > 0 {
            self.window.height = self.window.height.clamp(240, 4320);
        }
        if self.icon_size != 0 {
            self.icon_size = self.icon_size.clamp(16, 384);
        }
        // 弹出子窗口的每条记录同样收敛。
        // **必须在这里调用**：`PopoutWindow::normalize` 只在 `PopoutState::set`
        // 里被调用，而状态文件是可能被手改的 —— 手改进来的荒谬尺寸根本不经过
        // `set`，不在这里兜住就会直接进布局算矩形。
        for w in self.popout.windows.values_mut() {
            w.normalize();
        }
    }
}

/// 窗口几何。关闭时记录，下次启动还原。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    /// 客户区宽度。`0` 表示还没记录过。
    pub width: i32,
    /// 客户区高度。`0` 表示还没记录过。
    pub height: i32,
    /// 窗口左上角坐标。`None` 表示还没记录过，交给系统摆放。
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// 关闭时是否处于最大化状态。
    pub maximized: bool,
}

/// 状态仓库。
pub type StateStore = TomlStore<AppState>;

/// 打开状态仓库。载入时自动调用 [`AppState::normalize`]。
pub fn open() -> Result<StateStore> {
    TomlStore::open(paths::state_file()?, AppState::normalize)
}
