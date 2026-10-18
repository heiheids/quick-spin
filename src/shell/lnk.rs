//! 单个 `.lnk` 的解析。
//!
//! **为什么走 COM 而不是自己解二进制**：
//! `.lnk` 里目标路径主要藏在 `LinkTargetIDList`（一串 shell item id）中，
//! 自己解等于重新实现一遍 shell 命名空间解析。遇到 MSI 广告式快捷方式、
//! 已被卸载但残留的项、特殊文件夹重定向，很容易悄悄解错。
//! COM 的 `IShellLinkW` 是系统官方实现，正确性最高。
//!
//! 本模块只做解析，不做目录遍历（那是 [`super::scan`] 的事）。

// Shortcut 的多数字段（args / work_dir 等）由界面与右键菜单读取，那些模块尚未接入。
// 界面落地后请删掉本行。
#![allow(dead_code)]

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IShellLinkW, SHGetNameFromIDList, SHParseDisplayName, SIGDN_DESKTOPABSOLUTEPARSING,
    SLGP_RAWPATH, ShellLink,
};
use windows::core::{IUnknown, Interface, PCWSTR};

use crate::error::{AppError, Result, Win32Ext};

/// 路径类字段的缓冲区长度（UTF-16 单元）。
/// 长路径上限虽然是 32767，但快捷方式实际远不会那么长，512 足够且省内存。
const PATH_BUF: usize = 512;
/// 参数/描述类字段的缓冲区长度。
const TEXT_BUF: usize = 1024;

/// 快捷方式的来源。
///
/// 由扫描时所在的**根目录**决定，而不是事后按路径前缀去猜。
/// 开始菜单、桌面、自定义目录各自对应固定的来源，扫描时顺手记下来，
/// 之后按选项卡过滤就只是一次枚举比较，不需要字符串前缀匹配。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// 当前用户开始菜单
    UserStartMenu,
    /// 所有用户开始菜单
    CommonStartMenu,
    /// 当前用户桌面
    UserDesktop,
    /// 公共桌面
    PublicDesktop,
    /// 配置里额外添加的目录
    Extra,
}

/// 一条解析完成的快捷方式。
#[derive(Debug, Clone)]
pub struct Shortcut {
    /// `.lnk` 文件自身的位置
    pub lnk_path: PathBuf,
    /// 来自哪个来源根目录
    pub origin: Origin,
    /// 显示名（`.lnk` 文件名去掉扩展名）
    pub name: String,
    /// 解析出的目标路径。**可能为空** —— 见 [`Shortcut::is_shell_item`]。
    pub target: PathBuf,
    /// 命令行参数
    pub args: String,
    /// 工作目录
    pub work_dir: PathBuf,
    /// `.lnk` 里记录的描述
    pub description: String,
    /// 壳命名空间项的**解析名**（`::{CLSID}`），仅当 [`Self::is_shell_item`] 为真时有值。
    ///
    /// 用它才能认出条目到底是哪一个壳项（此电脑？回收站？），
    /// 从而决定右键菜单里该出现"管理"还是"清空回收站"。
    /// 光看显示名不行 —— 那是本地化文本，还可能被用户重命名。
    pub shell_name: Option<String>,
}

impl Shortcut {
    /// 是否是 shell 命名空间项（"文件资源管理器""控制面板""运行"等）。
    ///
    /// 这类条目没有文件系统目标，`GetPath` 返回空串，但交给 shell 执行完全正常 ——
    /// 所以**不能**因为"目标不存在"就丢弃它们。
    pub fn is_shell_item(&self) -> bool {
        self.target.as_os_str().is_empty()
    }

    /// 目标非空但已不存在。这才是真正失效的条目（程序被卸载后残留的快捷方式）。
    pub fn is_broken(&self) -> bool {
        !self.is_shell_item() && !self.target.exists()
    }

    /// 目标是否还在磁盘上（shell 命名空间项视为"可用"，由 shell 判定）。
    pub fn is_usable(&self) -> bool {
        !self.is_broken()
    }
}

/// 可复用的 `.lnk` 解析器。
///
/// 把字符串转成带结尾 NUL 的 UTF-16。
///
/// 与 `to_wide` 分开是因为那个只收 `&Path` 并会做合法性检查（路径可能含
/// 无法表示的字符）；这里的输入是程序自己的常量与菜单文字，不存在那种情况。
fn text_to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 取一个壳项快捷方式的解析名（形如 `::{20D04FE0-3AEA-1069-A2D8-08002B30309D}`）。
///
/// 路径是 `IShellLink::GetIDList` → `SHGetNameFromIDList`。返回 `None` 表示
/// 取不到（不是真的壳项，或者系统不给名字），调用方按"认不出来"处理。
///
/// # Safety
///
/// 调用前必须已初始化 COM。
unsafe fn shell_parsing_name(link: &IShellLinkW) -> Option<String> {
    // SAFETY: 调用方保证 COM 已初始化；`pidl` 由 CoTaskMemFree 释放。
    unsafe {
        let pidl = link.GetIDList().ok()?;
        if pidl.is_null() {
            return None;
        }
        let name = SHGetNameFromIDList(pidl, SIGDN_DESKTOPABSOLUTEPARSING)
            .ok()
            .and_then(|p| {
                let s = p.to_string().ok();
                CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
                s
            });
        CoTaskMemFree(Some(pidl as *const core::ffi::c_void));
        name.filter(|s| !s.is_empty())
    }
}

/// 创建一个指向 **shell 位置**（此电脑、回收站、控制面板…）的快捷方式。
///
/// `parsing_name` 是壳解析名（`shell:MyComputerFolder` 或 `::{CLSID}`）。
///
/// 用 `IShellLink::SetIDList` 把该项目的 **PIDL** 作为目标，而不是把目标写成
/// `explorer.exe`＋参数。这一点是**图标正确性的前提**：
///
/// 目标写成 `explorer.exe` 时，快捷方式的图标就是 explorer 的图标 ——
/// 无论它打开的是此电脑还是回收站，看起来都一样（用户报的就是这个）。
/// 而 `SetIDList` 建立的是真正的壳项快捷方式，图标、右键动词全都由那个项目决定。
///
/// 调用前必须已初始化 COM。
pub fn create_shell_shortcut(dest: &Path, parsing_name: &str, description: &str) -> Result<()> {
    let name_wide = text_to_wide(parsing_name);
    let desc_wide = text_to_wide(description);
    let file_wide = to_wide(dest)?;

    // SAFETY: 调用方保证 COM 已初始化；`pidl` 由 CoTaskMemFree 释放。
    unsafe {
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        SHParseDisplayName(PCWSTR(name_wide.as_ptr()), None, &mut pidl, 0, None)
            .ctx("SHParseDisplayName")?;

        if pidl.is_null() {
            return Err(AppError::Win32 {
                context: "SHParseDisplayName 返回了空 PIDL",
                code: 0,
            });
        }

        let result = (|| -> Result<()> {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None::<&IUnknown>, CLSCTX_INPROC_SERVER)
                    .ctx("CoCreateInstance(ShellLink)")?;

            link.SetIDList(pidl).ctx("IShellLink::SetIDList")?;
            if !description.is_empty() {
                link.SetDescription(PCWSTR(desc_wide.as_ptr()))
                    .ctx("IShellLink::SetDescription")?;
            }

            let persist: IPersistFile = link.cast().ctx("IShellLinkW -> IPersistFile")?;
            // 第二个参数 TRUE = 记住路径并写进文件；传 FALSE 会让 `.lnk` 不落盘。
            persist
                .Save(PCWSTR(file_wide.as_ptr()), true)
                .ctx("IPersistFile::Save(创建壳项快捷方式)")?;
            Ok(())
        })();

        // 无论成败都要释放 —— 否则每次添加都泄漏一个 shell 分配的 PIDL
        CoTaskMemFree(Some(pidl as *const core::ffi::c_void));
        result
    }
}

/// 创建一个指向**文件或文件夹**的快捷方式。
///
/// 调用前必须已初始化 COM。
pub fn create_link(dest: &Path, target: &Path, args: &str, description: &str) -> Result<()> {
    let target_wide = to_wide(target)?;
    let args_wide = text_to_wide(args);
    let desc_wide = text_to_wide(description);
    let file_wide = to_wide(dest)?;

    // SAFETY: 调用方保证 COM 已初始化；所有宽字符串都在本函数内存活到调用结束。
    unsafe {
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None::<&IUnknown>, CLSCTX_INPROC_SERVER)
                .ctx("CoCreateInstance(ShellLink)")?;

        link.SetPath(PCWSTR(target_wide.as_ptr()))
            .ctx("IShellLink::SetPath")?;
        if !args.is_empty() {
            link.SetArguments(PCWSTR(args_wide.as_ptr()))
                .ctx("IShellLink::SetArguments")?;
        }
        if !description.is_empty() {
            link.SetDescription(PCWSTR(desc_wide.as_ptr()))
                .ctx("IShellLink::SetDescription")?;
        }
        // 工作目录留空：指向文件夹时填它反而会让快捷方式的"起始位置"怪异。

        let persist: IPersistFile = link.cast().ctx("IShellLinkW -> IPersistFile")?;
        // 第二个参数 TRUE = 记住路径并写进文件。传 FALSE 会让 `.lnk` 不落盘。
        persist
            .Save(PCWSTR(file_wide.as_ptr()), true)
            .ctx("IPersistFile::Save(创建快捷方式)")?;
    }

    Ok(())
}

/// 复用同一个 `IShellLinkW` / `IPersistFile` 实例批量解析：
/// `CoCreateInstance` 每次都要走一遍 COM 激活流程，是单文件解析里最贵的一步。
/// `IPersistFile::Load` 会整体覆盖实例状态，所以复用是安全的。
pub struct Resolver {
    link: IShellLinkW,
    persist: IPersistFile,
}

impl Resolver {
    /// 创建解析器。调用前必须已初始化 COM（见 [`super::com::ComGuard`]）。
    pub fn new() -> Result<Self> {
        // SAFETY: 调用方保证 COM 已初始化。
        unsafe {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None::<&IUnknown>, CLSCTX_INPROC_SERVER)
                    .ctx("CoCreateInstance(ShellLink)")?;
            let persist: IPersistFile = link.cast().ctx("IShellLinkW -> IPersistFile")?;
            Ok(Self { link, persist })
        }
    }

    /// 解析一个 `.lnk` 文件。`origin` 由调用方给出（它知道自己在遍历哪个根目录）。
    pub fn resolve(&self, lnk_path: &Path, origin: Origin) -> Result<Shortcut> {
        let wide = to_wide(lnk_path)?;

        // SAFETY: COM 已由调用方初始化；`link`/`persist` 由 self 持有，
        // 所有裸指针都指向本函数栈上的缓冲区，生命周期覆盖全部调用。
        unsafe {
            self.persist
                .Load(PCWSTR(wide.as_ptr()), STGM_READ)
                .ctx("IPersistFile::Load")?;

            // 目标路径：先按"展开环境变量"的方式取（fflags = 0），
            // 拿不到再退回原始路径（SLGP_RAWPATH）。
            // 两条腿走路是因为部分快捷方式存的就是 %ProgramFiles% 这类宏。
            let mut path_buf = vec![0u16; PATH_BUF];
            let target_raw = match self.link.GetPath(&mut path_buf, std::ptr::null_mut(), 0) {
                Ok(()) => take_string(&path_buf),
                Err(_) => {
                    path_buf.fill(0);
                    match self.link.GetPath(
                        &mut path_buf,
                        std::ptr::null_mut(),
                        SLGP_RAWPATH.0 as u32,
                    ) {
                        Ok(()) => take_string(&path_buf),
                        Err(_) => String::new(),
                    }
                }
            };

            let work_dir = {
                let mut b = vec![0u16; PATH_BUF];
                self.link
                    .GetWorkingDirectory(&mut b)
                    .map(|()| take_string(&b))
                    .unwrap_or_default()
            };

            let args = {
                let mut b = vec![0u16; TEXT_BUF];
                self.link
                    .GetArguments(&mut b)
                    .map(|()| take_string(&b))
                    .unwrap_or_default()
            };

            let description = {
                let mut b = vec![0u16; TEXT_BUF];
                self.link
                    .GetDescription(&mut b)
                    .map(|()| take_string(&b))
                    .unwrap_or_default()
            };

            // 这里原本还调 `IShellLink::GetIconLocation` 取"自定义图标位置"并存在
            // `icon_source` / `icon_index` 上，但**那两个字段从未被读过** ——
            // 图标链路走的是对 `.lnk` 路径调 `IShellItemImageFactory`，
            // shell 自己就会把自定义图标解析出来。删掉它同时省下每条
            // 快捷方式一次 COM 调用与环境变量展开。

            // 显示名优先用文件名；文件名异常时退回描述。
            let name = lnk_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| description.clone());

            // 目标是空的说明这是壳命名空间项，再去取它的解析名。
            // 取不到就保持 `None`：条目照常能打开，只是右键少两个动词而已，
            // 不该因为它解析失败就丢弃整条快捷方式。
            let shell_name = if target_raw.is_empty() {
                shell_parsing_name(&self.link)
            } else {
                None
            };

            Ok(Shortcut {
                lnk_path: lnk_path.to_path_buf(),
                origin,
                name,
                target: PathBuf::from(target_raw),
                args,
                work_dir: PathBuf::from(work_dir),
                description,
                shell_name,
            })
        }
    }
}

/// 把路径转成 NUL 结尾的 UTF-16 序列。
fn to_wide(path: &Path) -> Result<Vec<u16>> {
    let mut v: Vec<u16> = path.as_os_str().encode_wide().collect();
    if v.contains(&0) {
        // 路径里带 NUL 说明来源不可信，宁可报错也不截断
        return Err(AppError::BadPath(path.to_path_buf()));
    }
    v.push(0);
    Ok(v)
}

/// 取缓冲区中第一个 NUL 之前的内容。
fn take_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}
