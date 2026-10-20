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
    ///
    /// **这是"最后一个用过的尺寸"的兜底值**。真正生效的值按选项卡分开记在
    /// [`Self::icon_sizes`] 里（使用者 2026-10-20：不同选项卡之间必须分开）。
    /// 保留这个字段是为了兼容旧版 `state.toml`：老文件里只有它，
    /// 迁移时把它当作所有选项卡的初值。
    pub icon_size: u32,

    /// **每个选项卡各自的**图标尺寸。键是选项卡 key，值与 [`Self::icon_size`] 同口径
    ///（`0` 或缺失 = 用兜底值）。
    ///
    /// # 为什么按选项卡而不是按窗口（使用者 2026-10-20 明确）
    ///
    /// > 主窗口内选项卡和对应弹出一样尺寸可以，但是不同的选项卡之间必须分开。
    ///
    /// 也就是说"主窗口切到 tab2"与"tab2 的弹出子窗口"是**同一份**尺寸 ——
    /// 它们是同一个分组的两种显示方式；而 tab2 与 tab3 之间必须互不干扰。
    ///
    /// 早先按**窗口**存（`popout.windows.*.icon_size`），结果是主窗口一个值、
    /// 每个弹窗各一个值，切选项卡时主窗口不会跟着切 —— 与需求不符。
    /// 现在统一按选项卡 key 存，主窗口与它的弹窗读同一个键。
    ///
    /// 用 `BTreeMap` 与 `entries` 同理：只为让 `state.toml` 字节稳定。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub icon_sizes: BTreeMap<String, u32>,
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

    /// 取某个选项卡的图标尺寸；没单独设过就返回兜底值。
    ///
    /// 兜底顺序：该选项卡自己的值 → [`Self::icon_size`]（旧版全局值）→ 0（用配置）。
    pub fn icon_size_of(&self, tab_key: &str) -> u32 {
        self.icon_sizes
            .get(tab_key)
            .copied()
            .filter(|&v| v != 0)
            .unwrap_or(self.icon_size)
    }

    /// 设置某个选项卡的图标尺寸。
    ///
    /// `0` 与兜底值相同的话**删掉这条**，避免 `state.toml` 越写越长
    ///（与 [`Self::set_entry`] 同一个思路）。
    pub fn set_icon_size(&mut self, tab_key: &str, size: u32) {
        if size == 0 || size == self.icon_size {
            self.icon_sizes.remove(tab_key);
        } else {
            self.icon_sizes.insert(tab_key.to_string(), size);
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
        // 每个选项卡的尺寸同样夹紧。`0` 是"没设过"，不能被夹成 16。
        for size in self.icon_sizes.values_mut() {
            if *size != 0 {
                *size = (*size).clamp(16, 384);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 图标尺寸必须**按选项卡分开**（使用者 2026-10-20 原话）：
    ///
    /// > 主窗口内选项卡和对应弹出一样尺寸可以，但是不同的选项卡之间必须分开。
    ///
    /// 这条是回归锁。之前按**窗口**存，于是"主窗口一个值、各弹窗各一个值"，
    /// 切选项卡时主窗口不跟着换 —— 表现就是"还是全部同步大小"。
    #[test]
    fn icon_size_is_per_tab() {
        let mut s = AppState {
            // 旧版全局兜底值
            icon_size: 64,
            ..Default::default()
        };

        // 没设过的选项卡 -> 用兜底值
        assert_eq!(s.icon_size_of("tab2"), 64);
        assert_eq!(s.icon_size_of("tab3"), 64);

        // 给 tab2 设一个
        s.set_icon_size("tab2", 128);
        assert_eq!(s.icon_size_of("tab2"), 128);
        // **别的选项卡不许受影响** —— 这就是那条 bug
        assert_eq!(s.icon_size_of("tab3"), 64, "改 tab2 波及了 tab3");
        assert_eq!(s.icon_size_of("main"), 64, "改 tab2 波及了主界面");

        // 再给 tab3 设另一个值，两者互不干扰
        s.set_icon_size("tab3", 32);
        assert_eq!(s.icon_size_of("tab2"), 128);
        assert_eq!(s.icon_size_of("tab3"), 32);
    }

    /// 设成与兜底值相同（或 0）时应当**删掉记录**，避免状态文件越写越长。
    #[test]
    fn setting_back_to_default_removes_entry() {
        let mut s = AppState {
            icon_size: 64,
            ..Default::default()
        };
        s.set_icon_size("tab2", 128);
        assert!(s.icon_sizes.contains_key("tab2"));

        // 改回兜底值 -> 记录应当消失
        s.set_icon_size("tab2", 64);
        assert!(
            !s.icon_sizes.contains_key("tab2"),
            "改回默认值后记录还在，状态文件会越积越长"
        );

        // 设 0 同样等于清除
        s.set_icon_size("tab3", 128);
        s.set_icon_size("tab3", 0);
        assert!(!s.icon_sizes.contains_key("tab3"));
    }

    /// 手改状态文件写进的荒谬值要被夹紧，`0` 不能被夹成 16。
    #[test]
    fn normalize_clamps_per_tab_sizes() {
        let mut s = AppState::default();
        s.icon_sizes.insert("tab2".into(), 99_999);
        s.icon_sizes.insert("tab3".into(), 1);
        s.icon_sizes.insert("tab4".into(), 0);
        s.normalize();

        assert_eq!(s.icon_sizes["tab2"], 384, "上限没夹住");
        assert_eq!(s.icon_sizes["tab3"], 16, "下限没夹住");
        assert_eq!(s.icon_sizes["tab4"], 0, "0 被夹成了 16，会丢掉'没设过'的语义");
    }
}
