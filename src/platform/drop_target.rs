//! OLE 拖放目标：接收**壳命名空间项**（此电脑、回收站、控制面板…）。
//!
//! # 为什么必须再造一套，而不是复用 `WM_DROPFILES`
//!
//! `WM_DROPFILES`（`DragAcceptFiles`）的数据格式是 `CF_HDROP`，**只能表达
//! 文件系统路径**。壳命名空间项没有文件系统路径 —— 从资源管理器拖「此电脑」
//! 过来时，数据里是一个 PIDL，`WM_DROPFILES` **一个字都收不到**，
//! 表现为"拖上去毫无反应"，不报错。
//!
//! 所以这里实现 `IDropTarget` 并注册 `CFSTR_SHELLIDLIST`（`Shell IDList Array`）。
//! 它同时也能表达普通文件，并且**资源管理器的拖动本来就是 OLE 拖动**，
//! 因此注册之后文件拖入也会先到这里。
//!
//! # 落盘策略
//!
//! 拖进来的东西**直接在这里变成我们目录下的 `.lnk`**，然后只往窗口发一个
//! "有新东西了，重新扫描"的消息。这样做省掉了把结果对象塞进事件流、
//! 再让上层逐个分配目标的整套传递 —— 拖放是低频操作，直接落盘最省事。
//!
//! 但**落盘的路径要攒下来**（见 [`take_dropped`]）：新条目该归到哪个选项卡
//! 是界面状态，本模块看不到，只能由上层在收到消息后决定。
//!
//! 判定规则：
//!
//! * 本身是 `.lnk` → **原样复制**（保留它内部的参数/工作目录/图标设置）
//! * 其他文件或文件夹 → 建一个指向它的快捷方式
//! * 壳命名空间项（解析名以 `::` 开头）→ 建一个 `explorer.exe` +
//!   该解析名为参数的快捷方式

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, POINTL};
// 这些类型分散在四个模块里，是照 crate 实际定义确认过的：
// `STGMEDIUM`/`IDataObject` 在 Com，`IDropTarget`/`DROPEFFECT`/`ReleaseStgMedium`
// 在 Ole，`MODIFIERKEYS_FLAGS` 在 SystemServices，`ITEMIDLIST` 在 Shell::Common。
use windows::Win32::System::Com::{CoTaskMemFree, DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, OleInitialize,
    RegisterDragDrop, ReleaseStgMedium,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    ILCombine, SHGetNameFromIDList, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{Ref, Result, implement};

/// 投放完成后发给窗口的消息。
///
/// 用自定义消息而不是直接调用上层：拖放回调发生在 OLE 的调用栈里，
/// 此刻去改界面状态会重入消息循环。投递成消息能保证它在正常时机被处理。
///
/// 取值是 `WM_APP + 2`：`WM_APP + 1` 已被 `window.rs` 的 `WM_APP_REDRAW` 占用。
/// 撞号会导致两条消息互相吞掉，而"重绘被吞"的症状是界面偶尔不刷新，极难定位。
pub const WM_DROP_COMPLETED: u32 = WM_APP + 2;

/// 本次投放已经落盘的路径，等上层取走（见 [`take_dropped`]）。
///
/// 为什么用静态而不是让事件带数据：`PostMessageW` 只有两个整数参数，
/// 想在事件里传 `Vec<PathBuf>` 就得自己分配一块内存、再约定由谁释放。
/// 拖放是低频操作，一块全局缓冲足够，也避免了在 OLE 调用栈里重入界面逻辑。
///
/// `Drop` 回调和 `WM_DROP_COMPLETED` 的处理都在**同一个 UI 线程**上
///（`OleInitialize` 用的是 STA，就是建窗口的那个线程），
/// 所以这里并不存在真正的并发；用 `Mutex` 只是为了让静态变量合法 ——
/// `Vec` 可以 const 构造，但可变静态需要一层同步原语。
static DROPPED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// 取走并清空"本次投放落盘的路径"。
///
/// 上层据此把新条目归到当前选项卡。返回空表示这次投放什么都没落成
///（例如格式解析失败），调用方照常重新扫描即可。
pub fn take_dropped() -> Vec<PathBuf> {
    lock().drain(..).collect()
}

/// 记下一条已落盘的路径。
fn remember(path: PathBuf) {
    lock().push(path);
}

/// 取那个静态缓冲。
///
/// 中毒（持锁时 panic）时取回内部值继续用：拖放数据丢了最多是"条目没归类"，
/// 不该让整个程序跟着崩。
fn lock() -> std::sync::MutexGuard<'static, Vec<PathBuf>> {
    match DROPPED.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 注册到窗口上的拖放目标。
///
/// 本身不存状态：拖放结果当场落盘，窗口只需要收到一个"重新扫描"的通知。
#[implement(IDropTarget)]
struct ShellDropTarget {
    hwnd: HWND,
}

impl IDropTarget_Impl for ShellDropTarget_Impl {
    /// 拖动进入窗口。必须在这里就给出"接不接受"，否则光标一直显示禁止。
    fn DragEnter(
        &self,
        pdataobj: Ref<IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let accepts = pdataobj
            .as_ref()
            .is_some_and(|obj| has_shell_idlist(obj));
        // SAFETY: 调用方（OLE）保证这个指针有效。
        unsafe {
            *pdweffect = if accepts { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        // SAFETY: 同上。
        unsafe {
            *pdweffect = DROPEFFECT_COPY;
        }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        Ok(())
    }

    /// 真正落下。解析出所有项目并落盘。
    ///
    /// 方法名是 `Drop` 而不是 `DragDrop` —— `windows-rs` 按 vtable 的原名生成，
    /// 而 COM 里这个方法就叫 `Drop`（和 Rust 的 `Drop` trait 不冲突，
    /// 因为这里实现的是 `IDropTarget_Impl` 的方法）。
    fn Drop(
        &self,
        pdataobj: Ref<IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let mut done = 0usize;
        if let Some(obj) = pdataobj.as_ref() {
            match collect_dropped(obj) {
                Ok(items) => {
                    for (display, parsing) in items {
                        match store_one(&display, &parsing) {
                            Ok(path) => {
                                eprintln!("拖入：{} → {}", display, path.display());
                                remember(path);
                                done += 1;
                            }
                            Err(e) => eprintln!("拖入「{display}」失败: {e}"),
                        }
                    }
                }
                Err(e) => eprintln!("解析拖放数据失败: {e}"),
            }
        }

        if done > 0 {
            // SAFETY: hwnd 由 `install` 保证有效；消息不带参数。
            unsafe {
                let _ = PostMessageW(
                    Some(self.hwnd),
                    WM_DROP_COMPLETED,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                );
            }
        }

        // SAFETY: 调用方保证指针有效。
        unsafe {
            *pdweffect = if done > 0 { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
        }
        Ok(())
    }
}

/// 把 `hwnd` 注册为拖放目标。
///
/// 失败不影响程序其余功能（只是拖入壳项没反应），所以调用方记一行日志即可，
/// 不必终止启动。
pub fn install(hwnd: HWND) -> Result<()> {
    // SAFETY: COM 已由启动流程初始化；OLE 拖放要求 OleInitialize，
    // 而 CoInitializeEx(STA) 之后再调 OleInitialize 是允许的。
    unsafe {
        let _ = OleInitialize(None);
        let target: IDropTarget = ShellDropTarget { hwnd }.into();
        RegisterDragDrop(hwnd, &target)?;
    }
    Ok(())
}

/// `CFSTR_SHELLIDLIST`（"Shell IDList Array"）的运行时格式号。
///
/// 每次调用都会拿到同一个值，但**必须**通过这个 API 取 —— 它由系统在运行时
/// 分配，不同机器/会话可能不同，硬编码会得到一个永远匹配不上的格式号。
fn shell_idlist_format() -> u16 {
    // SAFETY: 传入的是以 NUL 结尾的静态宽字符串。
    unsafe { RegisterClipboardFormatW(windows::core::w!("Shell IDList Array")) as u16 }
}

fn has_shell_idlist(obj: &IDataObject) -> bool {
    let fmt = FORMATETC {
        cfFormat: shell_idlist_format(),
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0 as u32,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    // SAFETY: COM 调用；查询不取数据，故不需要释放介质。
    // `QueryGetData` 返回 `HRESULT`，用 `is_ok()` 判成败。
    unsafe { obj.QueryGetData(&fmt).is_ok() }
}

/// 从 `CFSTR_SHELLIDLIST` 里取出所有项目，返回 `(显示名, 解析名)`。
///
/// 数据布局是 `CIDA`：
///
/// ```text
/// UINT   cidl;            项目个数
/// UINT   aoffset[cidl+1]; 各 PIDL 相对数据块起点的字节偏移
///                        aoffset[0] 是**父**目录（绝对 PIDL），
///                        之后每个元素是相对父目录的 **item** PIDL
/// ```
///
/// 注意 `aoffset` 有 `cidl + 1` 项 —— 第一项是父项。少算这一项是这段代码
/// 最容易犯的错，症状是解析出错误的名字。
fn collect_dropped(obj: &IDataObject) -> Result<Vec<(String, String)>> {
    let fmt = FORMATETC {
        cfFormat: shell_idlist_format(),
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0 as u32,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };

    // SAFETY: COM 调用；下面所有指针都只在 `medium` 释放前使用。
    unsafe {
        // windows-rs 把 `GetData(pformatetc, pmedium)` 的出参收成了返回值 ——
        // 所以这里只传格式，介质直接由返回值给出。
        let mut medium = obj.GetData(&fmt)?;

        let base = GlobalLock(medium.u.hGlobal) as *const u8;
        if base.is_null() {
            ReleaseStgMedium(&mut medium);
            return Ok(Vec::new());
        }

        let mut out = Vec::new();
        let cidl = *(base as *const u32) as usize;
        let offsets = base.add(4) as *const u32;
        // 父项
        let parent = base.add(*offsets as usize) as *const ITEMIDLIST;

        for i in 0..cidl {
            let relative = base.add(*offsets.add(i + 1) as usize) as *const ITEMIDLIST;
            // 相对 PIDL 必须与父项合并成绝对 PIDL 才能取名字。
            // `ILCombine` 收 `Option<*const ITEMIDLIST>`，所以要包 Some。
            let full = ILCombine(Some(parent), Some(relative));
            if full.is_null() {
                continue;
            }
            if let Some(pair) = name_of(full as *const ITEMIDLIST) {
                out.push(pair);
            }
            CoTaskMemFree(Some(full as *const core::ffi::c_void));
        }

        let _ = GlobalUnlock(medium.u.hGlobal);
        // 释放介质 —— 共享内存块是系统给的，不释放会一直占着。
        ReleaseStgMedium(&mut medium);
        Ok(out)
    }
}

/// 取一个绝对 PIDL 的 `(显示名, 解析名)`。
///
/// 两个名字都要：
///
/// * **解析名**（`SIGDN_DESKTOPABSOLUTEPARSING`）用来建快捷方式，
///   壳项得到的是 `::{CLSID}`，文件得到的是完整路径
/// * **显示名**（`SIGDN_NORMALDISPLAY`）用来做条目名，是本地化的
///  （"此电脑"而不是那串 GUID）
unsafe fn name_of(pidl: *const ITEMIDLIST) -> Option<(String, String)> {
    // SAFETY: 调用方保证 pidl 有效；返回的字符串都由 CoTaskMemFree 释放。
    unsafe {
        let parsing = SHGetNameFromIDList(pidl, SIGDN_DESKTOPABSOLUTEPARSING).ok()?;
        let parsing_str = parsing.to_string().unwrap_or_default();

        let display = SHGetNameFromIDList(pidl, SIGDN_NORMALDISPLAY).ok();
        let display_str = display
            .as_ref()
            .and_then(|p| p.to_string().ok())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| parsing_str.clone());

        CoTaskMemFree(Some(parsing.0 as *const core::ffi::c_void));
        if let Some(d) = display {
            CoTaskMemFree(Some(d.0 as *const core::ffi::c_void));
        }

        if parsing_str.is_empty() {
            None
        } else {
            Some((display_str, parsing_str))
        }
    }
}

/// 把一个拖入项落成我们目录下的 `.lnk`。
///
/// 返回类型是本工程的 `Result`（`AppError`），不是 `windows::core::Result` ——
/// 它要转发 `paths::shortcuts_dir()` 与文件系统操作的错误。
fn store_one(display: &str, parsing: &str) -> crate::error::Result<PathBuf> {
    let dir = crate::paths::shortcuts_dir()?;
    std::fs::create_dir_all(&dir)?;
    let dest = unique_dest(&dir, display);

    // 壳命名空间项：解析名形如 `::{20D04FE0-3AEA-1069-A2D8-08002B30309D}`。
    // 它没有文件路径，只能让 explorer 用这个解析名去打开。
    if parsing.starts_with("::") {
        crate::shell::lnk::create_shell_shortcut(&dest, parsing, display)?;
        return Ok(dest);
    }

    let src = Path::new(parsing);
    // 本身是快捷方式就原样复制：它内部可能带着参数、工作目录、自定义图标，
    // 重建一个指向它目标的快捷方式会丢掉这些。
    if src
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
    {
        std::fs::copy(src, &dest)?;
        return Ok(dest);
    }

    // 普通文件或文件夹：建一个指向它的快捷方式
    crate::shell::lnk::create_link(&dest, src, "", display)?;
    Ok(dest)
}

/// 在同名文件已存在时另找一个名字，绝不覆盖。
///
/// 覆盖会让"拖了两次同一个东西"丢掉先前的条目。
fn unique_dest(dir: &Path, display: &str) -> PathBuf {
    let safe = sanitize(display);
    let mut dest = dir.join(format!("{safe}.lnk"));
    let mut n = 2;
    while dest.exists() {
        dest = dir.join(format!("{safe} ({n}).lnk"));
        n += 1;
        if n > 999 {
            break;
        }
    }
    dest
}

/// 把显示名里不能做文件名的字符换掉。
///
/// 壳项的显示名是本地化的自由文本，可能含 `\ / : * ? " < > |` 等，
/// 直接当文件名会创建失败。
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.').to_string();
    if trimmed.is_empty() {
        "新条目".to_string()
    } else {
        trimmed
    }
}
