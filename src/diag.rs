//! 诊断日志。
//!
//! ## 为什么要有它
//!
//! 本程序编成 GUI 子系统（见 `main.rs`），**图形界面模式下没有控制台**：
//! 标准输出被接到 `NUL`，`println!` / `eprintln!` 的内容直接消失。
//! 于是"界面看起来不对"这类问题只能靠文件留下线索 —— 排查时唯一的入口
//! 就是 exe 旁边的 `诊断.log`。
//!
//! ## 为什么是独立模块而不是挂在 `app` 上
//!
//! 弹出子窗口也要写同一份日志（图标提取失败之类"用户看得见、我们看不见"
//! 的问题）。让 `popout` 去调 `crate::app::diag` 的话，依赖方向就反了 ——
//! 装配层依赖窗口层，而不是反过来。日志是两边的公共工具，单独放一层最省事。

use std::io::Write;
use std::sync::OnceLock;

/// 追加一行诊断日志（带相对进程启动的毫秒数，便于看间隔）。
///
/// **只在动作路径上调用**（启动、打开、拖入、图标提取失败）。
/// 每帧都调会变成"每帧一次开文件 + 写盘"，那才是真正的卡顿来源。
pub fn log(msg: &str) {
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
