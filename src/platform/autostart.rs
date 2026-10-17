//! 开机自启动：写 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`。
//!
//! # 为什么用 `Run` 键而不是"启动"文件夹里的快捷方式
//!
//! 两者都能开机启动。`Run` 键的优势是**写入原子、读取可查**：一条注册表值
//! 就能表达"启动哪个程序"，而快捷方式要走 COM 建 `.lnk` 文件。
//! 而且 `Run` 键是系统给这个用途预留的正规位置，用户在"任务管理器 → 启动"
//! 里能看到并管理它 —— 用启动文件夹反而会让用户在两处找。
//!
//! 写在 `HKCU` 而不是 `HKLM`：只影响当前用户，**不需要管理员权限**。
//!
//! # 与配置的关系
//!
//! 注册表是"系统层的当前事实"，`config.toml` 里的是"用户的意图"。
//! 两者可能不一致（用户在任务管理器里禁用了它）。所以读取时**以注册表为准**，
//! 界面上的开关反映真实状态，而不是配置文件里记的那个值。

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SAM_FLAGS, REG_SZ, RegCloseKey,
    RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::PCWSTR;

use crate::error::{AppError, Result};

/// 自启动项的注册表键路径（`HKCU` 下）。
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// 自启动项的值名。用它把自己和别的程序区分开。
const VALUE_NAME: &str = "QuickSpin";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 打开 `Run` 键。
///
/// 用 `RegOpenKeyExW` 而**不是** `RegCreateKeyExW`：后者被门控在 `Win32_Security`
/// 特性上（它要 `SECURITY_ATTRIBUTES` 参数），为一个不存在的情况引入整个
/// Security 模块不值得。`Run` 键在每台 Windows 上都存在，真取不到就明确报错。
fn open_run_key(sam: REG_SAM_FLAGS) -> Result<HKEY> {
    let path = wide(RUN_KEY);
    let mut key = HKEY::default();
    // SAFETY: 不涉及 COM；`path` 是带 NUL 的宽字符串，`key` 是本地变量。
    //
    // 这个 crate 里所有 `Reg*` 返回的是 `WIN32_ERROR` 而**不是** `Result`，
    // 所以不能用 `?`，必须自己比对 `ERROR_SUCCESS`。
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            None,
            sam,
            &mut key,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(AppError::Win32 {
            context: "RegOpenKeyExW(Run)",
            code: rc.0 as i32,
        });
    }
    Ok(key)
}

/// 设置（`enabled` 为真）或移除（假）自启动。
pub fn set(enabled: bool) -> Result<()> {
    let key = open_run_key(KEY_SET_VALUE)?;
    let name = wide(VALUE_NAME);

    // SAFETY: `key` 由 `open_run_key` 保证有效；字符串在本次调用期间存活。
    let rc = unsafe {
        if enabled {
            let exe = std::env::current_exe().map_err(|e| AppError::Win32 {
                context: "current_exe（无法确定自身路径）",
                code: e.raw_os_error().unwrap_or(0),
            })?;
            // 路径要**带引号**：`C:\Program Files\...` 含空格，不加引号系统会把
            // 空格前的部分当程序名、后面的当参数，于是启动失败。
            let value = wide(&format!("\"{}\"", exe.display()));
            let bytes = std::slice::from_raw_parts(
                value.as_ptr() as *const u8,
                value.len() * std::mem::size_of::<u16>(),
            );
            RegSetValueExW(key, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes))
        } else {
            let rc = RegDeleteValueW(key, PCWSTR(name.as_ptr()));
            if rc == ERROR_FILE_NOT_FOUND {
                ERROR_SUCCESS
            } else {
                rc
            }
        }
    };

    // SAFETY: key 用完即关。失败也关，不然泄漏句柄。
    unsafe {
        let _ = RegCloseKey(key);
    }

    if rc != ERROR_SUCCESS {
        return Err(AppError::Win32 {
            context: "写入自启动项",
            code: rc.0 as i32,
        });
    }
    Ok(())
}

/// 当前是否已设为自启动。
///
/// **以注册表为准**而不是读配置：用户可能在"任务管理器 → 启动"里禁用了它，
/// 那种情况下配置里还写着 `true`，界面就会显示成开着的 —— 骗人。
pub fn is_enabled() -> bool {
    let path = wide(RUN_KEY);
    let name = wide(VALUE_NAME);
    let mut key = HKEY::default();

    // SAFETY: 全过程只操作本地变量；每个成功打开的 key 都会关闭。
    unsafe {
        let rc = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            None,
            KEY_QUERY_VALUE,
            &mut key,
        );
        if rc != ERROR_SUCCESS {
            return false;
        }
        let rc = RegQueryValueExW(key, PCWSTR(name.as_ptr()), None, None, None, None);
        let _ = RegCloseKey(key);
        rc == ERROR_SUCCESS
    }
}
