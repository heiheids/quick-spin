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

/// 装上 panic 钩子：把 panic 的**消息与位置**写进 `诊断.log`。
///
/// ## 为什么必须有它
///
/// 本程序是 `panic = "abort"`（见 `Cargo.toml`），而 GUI 子系统下标准错误被
/// 接到 `NUL`（见 `main.rs` 的文件头）—— 也就是说**一次 panic 什么都不留**。
///
/// 这不是推测：2026-10-18 使用者报告过一次崩溃，查遍 `诊断.log`、
/// `%LOCALAPPDATA%\CrashDumps`、Application 事件日志**三处都没有记录**。
/// 原因不是"难查"，而是**根本没记**。装了钩子之后，这类事故至少留下
/// "消息 + `文件:行`"。
///
/// `panic = "abort"` 下钩子**仍会执行**（运行时先调钩子、再 abort），
/// 所以这条路是通的。
///
/// ## 只用消息与位置，不做栈回溯
///
/// 发布版是 `strip = "symbols"`，没有符号的栈回溯意义有限。
/// **`文件:行`** 才是能一眼定位的东西。
///
/// ## 钩子自己绝不能 panic
///
/// 否则就是双重 panic。所以这里**不索引、不 unwrap**，所有可能失败的分支
/// 都退化成一句占位文本；连 `log` 内部也是失败即返回。
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        // payload 通常是 `&str`（`panic!("...")`）或 `String`（格式化过的），
        // 但 `panic_any` 允许任何类型，所以两种都试、再兜底。
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<非字符串 payload>".to_string());

        // 位置是这一行里最有价值的部分：它直接指向出问题的代码。
        let loc = match info.location() {
            Some(l) => format!("{}:{}", l.file(), l.line()),
            None => "<无位置信息>".to_string(),
        };

        // 线程：窗口过程与消息循环都在 UI 线程上，所以能确认是不是那条线程。
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<未命名线程>").to_string();

        log(&format!("panic: {msg} @ {loc} （线程 {name}）"));
    }));
}

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
