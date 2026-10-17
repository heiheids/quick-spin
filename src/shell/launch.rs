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
    execute(
        target.as_os_str(),
        "open",
        None,
        None,
    )
}

/// 在资源管理器中定位并选中某个文件。
// 供右键菜单"打开文件所在位置"使用，界面尚未接入。
#[allow(dead_code)]
pub fn reveal(path: &Path) -> Result<()> {
    // explorer 的 /select 参数要求路径带引号，否则含空格的路径会被截断
    let params = format!("/select,\"{}\"", path.display());
    execute(
        OsStr::new("explorer.exe"),
        "open",
        Some(&params),
        None,
    )
}

/// 统一的 `ShellExecuteW` 调用外壳。
fn execute(file: &OsStr, operation: &str, params: Option<&str>, work_dir: Option<&OsStr>) -> Result<()> {
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
            dir_w.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
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
