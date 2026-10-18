//! 单实例：用命名互斥量保证只有一个进程在跑。
//!
//! # 为什么用互斥量而不是"找窗口"
//!
//! 只靠 `FindWindowW` 判断"有没有在跑"是不可靠的：窗口可能正处于创建中
//! （两个进程同时启动时会双双通过检查），也可能因为卡住而暂时找不到。
//! 互斥量由内核保证**只有一个获得者**，不存在这个竞态。
//!
//! # 句柄为什么不用"守卫"式管理
//!
//! 互斥量句柄必须由**首个实例**一直持有到进程结束。这里刻意不做 RAII：
//! `HANDLE` 是 `Copy` 类型、没有 `Drop` 实现，只要不显式 `CloseHandle`
//! 它就一直有效，进程退出时由系统释放 —— 这正是我们想要的时机。
//! 反过来，若用一个会被 drop 的守卫（比如存进某个会被重建的结构），
//! 单实例就会静默失效。

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, PostMessageW, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
};

use crate::error::{Result, Win32Ext};
use windows::core::w;

/// 本程序的窗口类名。必须在 `window.rs` 注册时用的是同一个。
const WINDOW_CLASS: &str = "QuickSpinWindow";

/// 互斥量名字。带 `Local\` 前缀表示只在本用户会话内生效 ——
/// 用 `Global\` 的话，同一台机器上另一个用户的实例会互相顶掉。
const MUTEX_NAME: &str = "Local\\QuickSpin.SingleInstance";

/// 请求已有实例把自己显示出来。
///
/// 与 `window.rs` 里的自定义消息号错开（那里用了 `WM_APP + 1/2/3`）。
pub const SHOW_MESSAGE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 5;

/// 尝试成为唯一实例。
///
/// * `Ok(true)` —— 当前是唯一实例，继续启动
/// * `Ok(false)` —— 已有实例在跑，已经把它叫到前面，**调用方应当直接退出**
pub fn acquire_or_activate() -> Result<bool> {
    // SAFETY: 传入带 NUL 的宽字符串；返回的句柄下面刻意不释放。
    let handle = unsafe { CreateMutexW(None, true, w!("Local\\QuickSpin.SingleInstance")) };

    // 名字写死在上面的 `w!` 里，这里只做一次一致性说明：
    // 改名字时两处都要改（`MUTEX_NAME` 是给文档和测试看的）。
    debug_assert!(MUTEX_NAME.ends_with("QuickSpin.SingleInstance"));

    let handle = handle.ctx("CreateMutexW(单实例)")?;

    // SAFETY: `GetLastError` 只读当前线程的错误码，无副作用。
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if !already {
        // 我们是第一个。
        //
        // **这里什么都不用做**：`HANDLE` 是 `Copy` 类型、没有 `Drop` 实现，
        // 所以它不会在离开作用域时被自动关闭；只要我们不显式调用
        // `CloseHandle`，这个互斥量就一直被本进程持有，直到进程退出时
        // 由系统释放。
        //
        // （原先这里写了 `mem::forget(handle)`，编译器警告它对 `Copy` 类型
        // 没有意义 —— 确实没有。真正的保证是"不调用 CloseHandle"。）
        return Ok(true);
    }

    // 已有实例：把它的窗口叫到前面，然后让调用方退出。
    activate_existing();
    // 这个句柄我们不需要，正常关闭即可。
    // SAFETY: 句柄有效且未被转交。
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(handle);
    }
    Ok(false)
}

/// 找到已有实例的窗口并请求它显示 / 前置。
fn activate_existing() {
    let class = to_wide(WINDOW_CLASS);
    // SAFETY: 只做窗口查找与消息投递。
    unsafe {
        if let Ok(hwnd) = FindWindowW(
            windows::core::PCWSTR(class.as_ptr()),
            windows::core::PCWSTR::null(),
        ) {
            if !hwnd.is_invalid() {
                let _ = PostMessageW(Some(hwnd), SHOW_MESSAGE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// 响应 [`SHOW_MESSAGE`]：显示并前置窗口。
///
/// 窗口可能处于最小化或隐藏状态，两种都要处理。
pub fn reveal(hwnd: HWND) {
    // SAFETY: 只是改变窗口显示状态。
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = ShowWindow(hwnd, SW_RESTORE);
        // 前置**必须**配合 `SetForegroundWindow`：`ShowWindow` 只会把窗口显示出来，
        // 但焦点可能还在用户当前操作的程序上，看起来像"没反应"。
        let _ = SetForegroundWindow(hwnd);
    }
}

fn to_wide(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(std::iter::once(0)).collect()
}
