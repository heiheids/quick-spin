//! 用系统 shell 启动快捷方式。
//!
//! **为什么把 `.lnk` 交给 shell，而不是解析出目标再 `CreateProcess`**：
//!
//! * "文件资源管理器""控制面板""运行"这类是 **shell 命名空间项**，根本没有文件系统
//!   目标，`IShellLinkW::GetPath` 返回空串。早期实现据此把它们当坏条目丢掉了 —— 是误伤。
//! * UWP / 商店应用、`.msc` 管理单元、需要提权的程序、`.url` 网页快捷方式，
//!   也都必须经由 shell 才能正确解析与启动。
//!
//! 代价只是多一次进程外调用（在启动路径上完全可忽略），换来的是兼容性。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::Path;

use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

use crate::error::{AppError, Result};

/// `ShellExecuteW` 的返回值 ≤ 此值时表示失败。
const SHELL_ERROR_MAX: isize = 32;

/// 打开一个快捷方式，或任意可执行项、文档、URL。
pub fn open(target: &Path) -> Result<()> {
    execute(target.as_os_str(), "open", None, None)
}

/// 用系统默认浏览器打开一个网址。
///
/// **单独一个函数而不是复用 `open`**：`open` 收的是 `&Path`，把网址塞进
/// `Path` 在这条路径上"恰好也能用"（字符串最终都是交给 `ShellExecuteW`），
/// 但读代码的人会以为这里在操作文件系统。
pub fn open_url(url: &str) -> Result<()> {
    execute(OsStr::new(url), "open", None, None)
}

/// 打开一个 `.lnk`。`resolved_target` 是它解析出的目标路径（shell 命名空间项为空）。
///
/// **目标是文件系统文件夹时必须绕开 shell。**
///
/// `ShellExecuteW` 打开文件夹走的是 DDE：由 Explorer 的 DDE 服务完成打开动作。
/// 这条路径在真实环境下会**长时间不返回**，实测日志停在这一步就再也没有下一步，
/// Windows 事件日志留下 `AppHangB1`（挂起后被系统结束）：
///
/// ```text
/// [    0 ms] launch_tile 开始: deps -> ...\shortcuts\deps.lnk      ← 没有下一步
/// [    0 ms] launch_tile 开始: 此电脑 -> ...\shortcuts\此电脑.lnk
/// [   86 ms] launch_tile: shell 已返回成功                        ← 命名空间项正常
/// ```
///
/// 命名空间项（此电脑、回收站、控制面板）没有文件系统目标，不走 DDE，所以一直正常 ——
/// 这就是这个 bug 只在"指向文件夹的快捷方式"上出现的原因。
///
/// 对文件夹改用 `explorer.exe <目录>`：那是普通进程启动，不经过 DDE。
/// 其余情况（命名空间项、exe、文档、UWP、`.url`）仍然交给 shell —— 那些正是
/// 没有 shell 就解析不了的。
pub fn open_shortcut(lnk: &Path, resolved_target: &Path) -> Result<()> {
    if resolved_target.is_dir() {
        return open_directory(resolved_target);
    }
    open(lnk)
}

/// 用资源管理器打开一个目录。
///
/// 直接起进程而不是 `ShellExecuteW`：这样不经过 DDE，调用会立刻返回。
/// 不等待子进程结束 —— `spawn` 出来的 `Child` 被丢弃只是关掉句柄，
/// 进程会继续跑（Windows 的进程句柄没有 close-kill 语义）。
fn open_directory(dir: &Path) -> Result<()> {
    std::process::Command::new("explorer.exe")
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| AppError::io(dir, e))
}

/// 在资源管理器中定位并选中某个文件。
///
/// # 为什么不能用 `Command::arg` 拼 `/select,"路径"`
///
/// `std::process::Command` 在 Windows 上**不是"把这段文字原样放进命令行"**：
/// 它按 CommandLineToArgvW 的规则给每个参数加引号、并转义参数里的引号。
/// 于是 `arg(format!("/select,\"{path}\""))` 得到的实际命令行是：
///
/// ```text
/// explorer.exe "/select,\"D:\...\blivechat.exe - 快捷方式.lnk\""
/// ```
///
/// explorer 反解出来是**四个**参数：
///
/// ```text
/// [0] /select,\
/// [1] D:\quick-spin\shortcuts\blivechat.exe
/// [2] -
/// [3] 快捷方式.lnk"
/// ```
///
/// 它认不出 `/select,`（多了个反斜杠、还带引号），于是**退回默认位置** ——
/// 实测打开的是「桌面」，**不管右击的是哪一条**。使用者看到的就是
/// "打开文件位置全都不正常"。
///
/// 所以这里必须**自己拼整条命令行**（`raw_arg`，不转义），让引号原样到 explorer。
/// 见 [`quote_arg`]：路径里的引号按 CommandLineToArgvW 的规则用 `\"` 转义，
/// 那正是 explorer 期望收到的形式。
pub fn reveal(path: &Path) -> Result<()> {
    // 同样不经 shell：`explorer.exe /select,` 是普通进程启动。
    // 走 `ShellExecuteW` 的话和文件夹是同一个风险（DDE 卡住 UI 线程）。
    let command = format!("/select,{}", quote_arg(&path.to_string_lossy()));

    std::process::Command::new("explorer.exe")
        // `raw_arg`：原样拼进命令行，不再加引号/转义。这正是 `/select,` 需要的。
        // 用 `std::os::windows::process::CommandExt` 提供。
        .raw_arg(command)
        .spawn()
        .map(|_| ())
        .map_err(|e| AppError::io(path, e))
}

/// 按 `CommandLineToArgvW` 的规则给一个参数加引号（含内嵌引号的转义）。
///
/// 单独抽出来是因为这条规则有反直觉的地方：反斜杠**只在引号前面**才有转义
/// 意义，所以路径里的 `\` 一个都不用动，只有 `"` 要变成 `\"`。
fn quote_arg(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => {
                backslashes += 1;
                out.push('\\');
            }
            '"' => {
                // 引号前面的反斜杠数要翻倍，再补一个转义引号用的
                for _ in 0..backslashes {
                    out.push('\\');
                }
                out.push('\\');
                out.push('"');
                backslashes = 0;
            }
            _ => {
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    // 结尾的反斜杠要翻倍，否则会把收尾的引号转义掉
    for _ in 0..backslashes {
        out.push('\\');
    }
    out.push('"');
    out
}

/// 统一的 `ShellExecuteW` 调用外壳。
fn execute(
    file: &OsStr,
    operation: &str,
    params: Option<&str>,
    work_dir: Option<&OsStr>,
) -> Result<()> {
    let file_w = to_wide(file);
    let op_w = to_wide(OsStr::new(operation));
    let params_w = params.map(|p| to_wide(OsStr::new(p)));
    let dir_w = work_dir.map(to_wide);

    // SAFETY: 所有字符串缓冲都活到本次调用结束；hwnd 传 None 表示不挂父窗口。
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(op_w.as_ptr()),
            PCWSTR(file_w.as_ptr()),
            params_w
                .as_ref()
                .map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
            dir_w
                .as_ref()
                .map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
            SW_SHOWNORMAL,
        )
    };

    check(result)
}

/// 把返回值翻译成可读错误。
fn check(result: HINSTANCE) -> Result<()> {
    let code = result.0 as isize;
    if code > SHELL_ERROR_MAX {
        return Ok(());
    }
    Err(AppError::ShellExecute {
        message: describe(code),
        code: code as i32,
    })
}

/// `ShellExecuteW` 的约定错误码。这些不是 HRESULT，含义见 Win32 文档。
fn describe(code: isize) -> &'static str {
    match code {
        0 => "系统内存或资源不足",
        2 => "找不到指定的文件",
        3 => "找不到指定的路径",
        5 => "访问被拒绝（目标可能需要管理员权限）",
        8 => "系统内存不足",
        26 => "文件共享冲突",
        27 => "文件关联不完整或无效",
        28 => "DDE 操作超时",
        29 => "DDE 事务失败",
        30 => "DDE 事务失败（另一个程序正忙）",
        31 => "没有关联的应用程序",
        32 => "找不到所需的动态链接库",
        _ => "未知的 ShellExecute 错误",
    }
}

/// 转成 NUL 结尾的 UTF-16。用 `OsStr` 而不是 `&str`，
/// 避免路径含非法 Unicode 时被有损转换改坏。
fn to_wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `quote_arg` 的输出，放在**真实的命令行形态**里必须能被
    /// `CommandLineToArgvW` 还原成原来的路径。
    ///
    /// 这是"打开文件位置"那条 bug 的回归锁：之前用 `Command::arg` 拼，
    /// 引号被二次转义，explorer 收到 4 个参数、认不出 `/select,`，
    /// 于是不管点哪条都打开同一个默认位置。
    ///
    /// **必须带上 `explorer.exe` 前缀**：`CommandLineToArgvW` 对第一个参数
    /// 有特殊规则 —— 命令行以引号开头时，argv[0] 读到下一个引号为止且
    /// 不做反斜杠转义。拿裸的 `"路径"` 去解析会得到错误结果，
    /// 而真实命令行里第一个参数是未加引号的 `explorer.exe`。
    #[test]
    fn quote_arg_roundtrips_through_command_line_to_argv() {
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        use windows::Win32::UI::Shell::CommandLineToArgvW;
        use windows::core::PCWSTR;

        let cases = [
            r"C:\简单\记事本.lnk",
            r"D:\quick-spin\shortcuts\blivechat.exe - 快捷方式.lnk",
            r"E:\工程-快启桌面管理\target\release\a b c.lnk",
            // 含内嵌引号的路径（Windows 文件名里不允许 `"`，但它可能来自
            // 命令行/配置，转义规则必须仍然正确）
            "C:\\带\"引号\"的\\路径.lnk",
            r"C:\结尾是反斜杠\",
        ];

        for path in cases {
            let command_line = format!("explorer.exe /select,{}", quote_arg(path));
            let wide: Vec<u16> = command_line.encode_utf16().chain(std::iter::once(0)).collect();

            let mut count = 0i32;
            let argv = unsafe { CommandLineToArgvW(PCWSTR(wide.as_ptr()), &mut count) };
            assert!(!argv.is_null(), "CommandLineToArgvW 失败: {path}");

            // 期望恰好两个参数：explorer.exe 与 /select,<路径>
            assert_eq!(
                count, 2,
                "路径应当只占一个参数（得到 {count} 段）: {command_line}"
            );

            let arg_at = |i: usize| unsafe {
                let ptr = *((argv as *const *const u16).add(i));
                let mut len = 0;
                while *ptr.add(len) != 0 {
                    len += 1;
                }
                String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
            };
            let select_arg = arg_at(1);
            unsafe {
                let _ = LocalFree(Some(HLOCAL(argv as *mut core::ffi::c_void)));
            }

            assert_eq!(
                select_arg,
                format!("/select,{path}"),
                "还原出来的参数与原路径不一致"
            );
        }
    }

    /// `/select,` 与路径之间**不能有空格**，且路径必须在引号里。
    #[test]
    fn reveal_command_shape() {
        let path = Path::new(r"D:\a b\c.lnk");
        let command = format!("/select,{}", quote_arg(&path.to_string_lossy()));
        assert_eq!(command, r#"/select,"D:\a b\c.lnk""#);
    }
}
