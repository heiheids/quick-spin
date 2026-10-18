//! COM 初始化的 RAII 守卫。
//!
//! 解析 `.lnk` 需要 `IShellLinkW`，而用 COM 之前必须先初始化套间。
//! 扫描整体跑在后台的一个线程里，所以在这里初始化一次即可 ——
//! 不要每个文件都 `CoInitializeEx` / `CoUninitialize` 一遍，那是有实打实开销的。
//!
//! 用 RAII 而不是手动配对，是为了让任何提前 `return` 或 `?` 分支都不会漏掉配平。

// ComGuard::mta 供后台工作线程使用，当前扫描线程用的是 sta。
// 后台线程池接入后请删掉本行。
#![allow(dead_code)]

use windows::Win32::System::Com::{
    COINIT, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize,
};

use crate::error::{AppError, Result};

/// 该线程已经用别的套间模式初始化过 COM。
/// 这不是错误 —— 直接用现有的就行，但不该由我们去反初始化。
const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;

/// COM 生命周期守卫：构造成功即代表本线程可安全使用 COM，析构时自动配平。
pub struct ComGuard {
    /// 是否需要在自己的 `Drop` 里调用 `CoUninitialize`。
    balance: bool,
}

impl ComGuard {
    /// 单线程套间（STA）。`IShellLinkW` 是套间对象，STA 下最稳。
    pub fn sta() -> Result<Self> {
        Self::init(COINIT_APARTMENTTHREADED)
    }

    /// 多线程套间（MTA）。后台工作线程用它，不需要消息泵。
    pub fn mta() -> Result<Self> {
        Self::init(COINIT_MULTITHREADED)
    }

    fn init(mode: COINIT) -> Result<Self> {
        // SAFETY: 第一个参数是保留参数，按文档必须为 NULL。
        let hr = unsafe { CoInitializeEx(None, mode) };

        if hr.0 >= 0 {
            Ok(Self { balance: true })
        } else if hr.0 == RPC_E_CHANGED_MODE {
            Ok(Self { balance: false })
        } else {
            Err(AppError::Win32 {
                context: "CoInitializeEx",
                code: hr.0,
            })
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.balance {
            // SAFETY: 与上面成功返回的那次 CoInitializeEx 严格配对，只调用一次。
            unsafe { CoUninitialize() };
        }
    }
}
