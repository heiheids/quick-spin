//! 快捷方式集合：筛选、搜索、排序。
//!
//! ## 为什么搜索不做索引
//!
//! 规模决定做法。实测本机全部来源合计 **142 条**，即使每条都做一次
//! `to_lowercase()` 再 `contains`，总耗时也在微秒量级 —— 比建立并维护
//! 索引（还要处理增删同步）便宜得多。等条目数真的上到几万条再说。

use std::collections::HashMap;

use crate::config::model::{MAIN_TAB, TabDef};
use crate::shell::lnk::Shortcut;
use crate::state::EntryOverride;

/// 已扫描到的全部快捷方式，以及从它派生的视图。
pub struct Library {
    all: Vec<Shortcut>,
}

impl Library {
    pub fn new(all: Vec<Shortcut>) -> Self {
        Self { all }
    }

    pub fn total(&self) -> usize {
        self.all.len()
    }

    /// 取出某个选项卡当前该显示的条目**下标**。
    ///
    /// 返回下标而不是 `&Shortcut`：调用方（`App`）既要持有集合又要持有过滤结果，
    /// 返回引用会让它自己借用自己，Rust 不允许。下标是普通整数，没有这个问题。
    ///
    /// `overrides` 是程序内覆盖表（见架构文档 §9）。
    ///
    /// **归属只看覆盖表，不看来源**：按需求"自动扫描先不做、全部手动添加"，
    /// 条目在哪个分组完全由用户拖入/移动决定。`group` 为空的条目归到
    /// [`MAIN_TAB`]（主窗口）。
    pub fn view(
        &self,
        tab: &TabDef,
        query: &str,
        overrides: &HashMap<String, EntryOverride>,
    ) -> Vec<usize> {
        let needle = query.trim().to_lowercase();

        self.all
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let ov = overrides.get(&path_key(s));
                // 被隐藏的条目在任何视图里都不出现
                if ov.is_some_and(|o| o.hidden) {
                    return false;
                }
                // 没有显式分组的一律算主窗口
                let group = ov
                    .and_then(|o| o.group.as_deref())
                    .unwrap_or(MAIN_TAB);
                group == tab.key
            })
            .filter(|(_, s)| {
                if needle.is_empty() {
                    return true;
                }
                // 搜索同时匹配显示名与原名：用户重命名之后，
                // 按原名也应该还能搜到，否则会以为条目丢了
                let ov = overrides.get(&path_key(s));
                let alias = ov.and_then(|o| o.alias.as_deref()).unwrap_or("");
                s.name.to_lowercase().contains(&needle)
                    || alias.to_lowercase().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// 某个选项卡下的条目数。侧栏"有内容时无法删除"用它判断。
    pub fn count_in(
        &self,
        tab: &TabDef,
        overrides: &HashMap<String, EntryOverride>,
    ) -> usize {
        self.view(tab, "", overrides).len()
    }

    pub fn get(&self, index: usize) -> Option<&Shortcut> {
        self.all.get(index)
    }
}

/// 覆盖表的键。抽成函数是为了让"键的构造方式"只有一处定义 ——
/// 读写两侧不一致会导致改动莫名其妙地不生效。
///
/// 用 `to_string_lossy` 而不是 `to_str`：路径可能含 Windows 允许但 UTF-8
/// 表示不了的字符（不成对的代理项），`to_str` 会返回 `None` 让条目直接丢键。
/// 有损转换至少能让它有个稳定的键。
pub fn path_key(shortcut: &Shortcut) -> String {
    shortcut.lnk_path.to_string_lossy().into_owned()
}
