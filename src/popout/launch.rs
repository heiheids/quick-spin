//! 弹出子窗口里"点一格 = 启动它"。
//!
//! ## 为什么不在这里自己调 `ShellExecuteW`
//!
//! 启动一个条目有几种容易踩的坑，主窗口那条路（`App::launch_tile`）都已经处理过了：
//!
//! * **文件夹不能走 shell**：`ShellExecuteW` 打开文件夹走的是 DDE，Explorer 的
//!   DDE 服务卡住时会把调用方（也就是我们的 **UI 线程**）一起卡住；
//! * `.lnk` 本身与它解析出来的目标要分开传，才能按类型选路径。
//!
//! 所以这里**复用** [`crate::shell::launch::open_shortcut`]，一行都不另写。
//! 另写一条启动路径的代价不是"多几行"，而是"同一件事以后要在两处修"。
//!
//! ## 失败绝不能崩
//!
//! 用户点到一条目标已经不在的快捷方式（U 盘拔了、程序卸载了）是很正常的事，
//! 那时 `open_shortcut` 返回 `Err`。这里只记账到诊断日志，
//! 不 panic、也不把异常往上传 —— 一个坏条目不该让弹窗变砖。
//!
//! ## 启动后**不隐藏**弹窗（使用者已确认）
//!
//! 主窗口的"启动程序后收起面板"（`hide_after_launch`）指的是**主面板**：
//! 它挡在桌面上，启动完就该让开。弹窗是使用者主动开的独立视图
//!（"弹出只是多了一种显示方式"），在这里点一格通常意味着还要接着点下一格，
//! 收起它反而是干扰。所以这一条**刻意不接** `hide_after_launch`。

use super::icons::Item;

/// 启动一条。
///
/// 与主窗口 `App::launch_tile` 的差别只有"要不要收起窗口"——
/// 启动本身走的是同一条 `open_shortcut`。
pub fn launch(item: &Item) {
    crate::diag::log(&format!(
        "弹窗启动: {} -> {}",
        item.name,
        item.lnk.display()
    ));
    let target = item.lnk.clone();
    let resolved = item.target.clone();
    match crate::shell::launch::open_shortcut(&target, &resolved) {
        Ok(()) => crate::diag::log("弹窗启动: shell 已返回成功"),
        // 坏快捷方式只记一行：用户点错一个不该有任何可见后果。
        Err(e) => crate::diag::log(&format!("弹窗启动失败: {e}")),
    }
}
