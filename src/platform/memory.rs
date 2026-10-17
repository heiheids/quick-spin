//! 进程内存读数。
//!
//! ## 口径问题（很容易搞错，所以写在这里）
//!
//! Windows 上没有单一的"内存占用"，至少有三个常用口径：
//!
//! | 口径 | 来源 | 对应任务管理器哪一列 |
//! |---|---|---|
//! | 私有提交量 | `PROCESS_MEMORY_COUNTERS_EX::PrivateUsage` | **提交大小** |
//! | 工作集（全部驻留页） | `PROCESS_MEMORY_COUNTERS::WorkingSetSize` | 比"内存"列偏大（含共享 DLL） |
//! | 私有工作集 | 需遍历工作集逐页判断 | **内存**（默认那一列） |
//!
//! 本模块目前给的是前两个。任务管理器默认"内存"列是**私有工作集**，
//! 它要把每个驻留页按"是否与其它进程共享"筛一遍才能算出来，
//! 本模块还没做（见文件末尾说明）。
//!
//! 之所以要把这张表写清楚：项目目标就是"低内存"，
//! 如果测量口径和用户看到的那一列对不上，就会对着错误的数字做优化。

use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::GetCurrentProcess;

/// 一次内存快照（字节）。
#[derive(Debug, Clone, Copy, Default)]
pub struct MemorySnapshot {
    /// 私有提交量。任务管理器"提交大小"列的口径。
    pub private_commit: usize,
    /// 工作集（全部驻留页，含共享 DLL）。仅作参考。
    pub working_set: usize,
}

/// 读取当前进程的内存占用。失败返回 `None`
/// —— 读不到内存数不该让程序本身出错。
pub fn snapshot() -> Option<MemorySnapshot> {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };

    // SAFETY: counters 是本函数栈上的变量；cb 已按其真实大小填写，
    // 系统只会按这个长度写入，不会越界。
    // GetCurrentProcess 返回伪句柄（-1），不需要也不应该释放。
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut _,
            counters.cb,
        )
    }
    .ok()?;

    Some(MemorySnapshot {
        private_commit: counters.PrivateUsage,
        working_set: counters.WorkingSetSize,
    })
}

/// 字节数格式化成易读文本。
pub fn human(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let value = bytes as f64;

    if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.0} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

// TODO(精确口径): 任务管理器"内存"列 = 私有工作集。
// 算法是 QueryWorkingSetEx 把全部驻留页列出来，按 PSAPI_WORKING_SET_EX_BLOCK
// 的 bit 15（Shared）筛掉与其它进程共享的页，再乘以页大小。
// `windows` crate 把该结构体暴露成裸 `_bitfield: usize`（没有具名字段），
// 需要自己按位取值，因此单独一轮做，避免和界面功能混在一起。
