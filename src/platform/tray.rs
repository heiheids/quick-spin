//! 托盘图标：图标注册、右键菜单、以及"资源管理器重启后重新挂上"。
//!
//! # 状态不落在这里
//!
//! `NOTIFYICONDATAW` 每次都**现构**（窗口句柄 + 固定 uID + 图标），不缓存。
//! 它是个纯数据表，构造一次只有几次赋值；而缓存就意味着要在退出、重挂、
//! 改提示文字等每一条路径上记得同步它 —— 那正是漏掉 `NIM_DELETE` 的常见来源。
//!
//! # 两个必须处理的坑
//!
//! 1. **幽灵图标**：退出时漏掉 `NIM_DELETE`，托盘里会留一个点不动的死图标，
//!    直到用户把鼠标划过它才消失。看起来像"程序没退干净"，很难归因。
//! 2. **资源管理器重启**：Explorer 崩溃或重启后系统会重建托盘，
//!    **所有图标都不会自动回来**。必须监听广播消息 `TaskbarCreated` 并
//!    重新 `NIM_ADD`，否则用户重启一次 Explorer 就永远看不到图标了。

use std::sync::OnceLock;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, MF_SEPARATOR, MF_STRING, PostMessageW,
    SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_LBUTTONUP, WM_NULL,
    WM_RBUTTONUP,
};

use crate::error::Result;

/// 托盘回调消息。取值必须避开已经用掉的 `WM_APP + 1`（重绘）与 `WM_APP + 2`（拖放）。
pub const CALLBACK_MESSAGE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 3;

/// 托盘图标在本程序内的编号。只有一个图标，固定值即可。
const ICON_ID: u32 = 1;

/// 右键菜单的命令号。用自定义区间，避免和系统命令号撞车。
pub mod command {
    pub const SHOW: usize = 0x1001;
    pub const HIDE: usize = 0x1002;
    pub const EXIT: usize = 0x1003;
}

/// 应用图标，**编译期嵌进 exe**。
///
/// 嵌入而不是放在 exe 旁边：这样只有一个文件要分发，用户把 exe 拷到别处
/// 也不会变成空白图标。
const APP_ICON: &[u8] = include_bytes!("../../assets/app.ico");

/// 取托盘用的图标句柄。
///
/// 走"把嵌入的 `.ico` 落到临时文件、再交给 `LoadImageW`"这条路，而不是
/// `CreateIconFromResourceEx`：后者要的是**单个图标的资源位**（BITMAPINFOHEADER
/// 加掩码位），而我们的 `.ico` 里各尺寸是 PNG 载荷 —— `CreateIconFromResourceEx`
/// 对 PNG 载荷只保证支持 256×256。交给 shell 自己的 ICO 解析器最稳。
///
/// 每次启动都覆盖写：文件只有两百来 KB，而"判断要不要重写"的逻辑
/// 一旦出错就会用着上一版的图标，那种问题很难察觉。
fn load_tray_icon() -> windows::Win32::UI::WindowsAndMessaging::HICON {
    use windows::Win32::UI::WindowsAndMessaging::{HICON, IMAGE_ICON, LR_LOADFROMFILE, LoadImageW};

    let path = std::env::temp_dir().join("quick_spin_tray.ico");
    if std::fs::write(&path, APP_ICON).is_ok() {
        let wide = to_wide(&path.to_string_lossy());
        // SAFETY: 传入带 NUL 的宽字符串路径；`LR_LOADFROMFILE` 表示按文件名加载。
        unsafe {
            if let Ok(handle) = LoadImageW(
                None,
                windows::core::PCWSTR(wide.as_ptr()),
                IMAGE_ICON,
                0,
                0,
                LR_LOADFROMFILE,
            ) {
                if !handle.is_invalid() {
                    return HICON(handle.0);
                }
            }
        }
    }

    // 兜底：加载失败也要有个图标，否则托盘里是一块空白。
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::LoadIconW(
            None,
            windows::Win32::UI::WindowsAndMessaging::IDI_APPLICATION,
        )
    }
    .unwrap_or_default()
}

/// 组装 `NOTIFYICONDATAW`。
///
/// `cbSize` **必须**填成结构体自身的字节数：这个结构在历史上多次加长，
/// 系统靠它判断调用方认识哪个版本。填错会导致 `Shell_NotifyIconW` 直接失败，
/// 而且失败是静默的（只返回 FALSE，不抛错）。
fn make_data(hwnd: HWND) -> NOTIFYICONDATAW {
    // SAFETY: `NOTIFYICONDATAW` 是 POD，全零是合法的初始状态。
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = ICON_ID;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = CALLBACK_MESSAGE;
    // 用嵌入的应用图标（去掉白底后的圆角方块）。
    data.hIcon = load_tray_icon();

    let tip: Vec<u16> = "Quick spin"
        .encode_utf16()
        .take(127)
        .chain(std::iter::once(0))
        .collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    data
}

/// 挂上托盘图标。重复调用是安全的（系统按 `hWnd + uID` 去重）。
pub fn install(hwnd: HWND) -> Result<()> {
    let data = make_data(hwnd);
    // SAFETY: `data` 在本次调用期间存活，且 `cbSize` 已按结构体实际大小填好。
    let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
    if !ok.as_bool() {
        // 这个 API 不返回具体错误码，只能给出"哪一步失败"。
        return Err(crate::error::AppError::Win32 {
            context: "Shell_NotifyIconW(NIM_ADD)",
            code: 0,
        });
    }
    Ok(())
}

/// 摘掉托盘图标。**退出前必须调用**，否则留下幽灵图标。
pub fn remove(hwnd: HWND) {
    let data = make_data(hwnd);
    // SAFETY: 同 `install`。这里忽略失败：进程马上要退出了，没别的可做。
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

/// `TaskbarCreated` 广播消息的编号。
///
/// 由 `RegisterWindowMessageW` 在运行时分配，**只能问一次**（每次调用返回同一个值，
/// 但没必要反复查询），所以用 `OnceLock` 缓存。
pub fn taskbar_created_message() -> u32 {
    static CACHE: OnceLock<u32> = OnceLock::new();
    *CACHE.get_or_init(|| {
        // SAFETY: 传入以 NUL 结尾的静态宽字符串。
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(windows::core::w!(
                "TaskbarCreated"
            ))
        }
    })
}

/// 从托盘回调消息的 `lParam` 里取出鼠标事件。
pub fn mouse_event(lparam: LPARAM) -> u32 {
    // 经典 `NOTIFYICON_VERSION` 下，lParam 低字就是鼠标消息。
    (lparam.0 as u32) & 0xFFFF
}

/// 该鼠标事件是否要弹右键菜单。
pub fn is_right_click(mouse: u32) -> bool {
    mouse == WM_RBUTTONUP
}

/// 该鼠标事件是否要切换窗口显示。
pub fn is_left_click(mouse: u32) -> bool {
    mouse == WM_LBUTTONUP
}

/// 弹出托盘右键菜单，返回用户选择的命令号（点了别处返回 `None`）。
///
/// `window_visible` 决定菜单里显示"显示"还是"隐藏"。
pub fn show_context_menu(hwnd: HWND, window_visible: bool) -> Option<usize> {
    // SAFETY: 菜单句柄在下面每条返回路径上都会被销毁。
    unsafe {
        let menu = CreatePopupMenu().ok()?;

        let show_text = to_wide(if window_visible {
            "隐藏窗口"
        } else {
            "显示窗口"
        });
        let show_cmd = if window_visible {
            command::HIDE
        } else {
            command::SHOW
        };
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            show_cmd,
            windows::core::PCWSTR(show_text.as_ptr()),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
        let exit_text = to_wide("退出");
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            command::EXIT,
            windows::core::PCWSTR(exit_text.as_ptr()),
        );

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);

        // `SetForegroundWindow` 是 MSDN 明确要求的：不调用的话，
        // 菜单弹出后点别处不会收起，会一直挂在那里。
        let _ = SetForegroundWindow(hwnd);

        // `TPM_RETURNCMD` 让返回值直接是命令号，省掉 `WM_COMMAND` 的往返。
        let chosen = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );

        // 菜单关掉之后往自己窗口投一条空消息，把焦点要回来 ——
        // 这也是 MSDN 的既有配方，否则菜单可能残留一小块没重绘。
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);

        let id = chosen.0 as usize;
        if id == 0 { None } else { Some(id) }
    }
}

fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
