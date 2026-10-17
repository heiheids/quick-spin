//! 扫描快捷方式来源（开始菜单、桌面、自定义目录）。
//!
//! **性能设计**：
//! 整段是"目录遍历 + COM 解析"的顺序操作，瓶颈在磁盘而非 CPU，
//! 几百个 `.lnk` 通常几十毫秒跑完，因此这里**不做并发** ——
//! 并发还得给每个线程各自初始化 COM，收益不抵复杂度。
//! 整个扫描复用同一个 [`Resolver`]，省掉每个文件一次 `CoCreateInstance`。
//!
//! 调用方的责任：把 [`scan`] 放到后台线程执行，别堵住 UI 线程。
//! 本函数自身不创建线程，也不持有任何长期句柄。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_CommonPrograms, FOLDERID_Desktop, FOLDERID_Programs, FOLDERID_PublicDesktop,
    KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::core::{GUID, PWSTR};

use crate::config::model::Sources;
use crate::error::Result;

use super::com::ComGuard;
use super::lnk::{Origin, Resolver, Shortcut};

/// 递归深度上限。开始菜单没必要更深，同时挡住目录联接形成的环。
const MAX_DEPTH: u32 = 8;

/// 一次扫描的结果。
#[derive(Debug, Default)]
pub struct ScanReport {
    /// 可用的快捷方式（含 shell 命名空间项），已按名称排序
    pub items: Vec<Shortcut>,
    /// 解析失败条数
    pub skipped: usize,
    /// 解析成功但目标已失效的条数
    pub broken: usize,
    /// 逐条跳过原因，仅在诊断模式下打印
    pub failures: Vec<(PathBuf, String)>,
    /// 本次扫描耗时
    pub elapsed: Duration,
}

impl ScanReport {
    /// shell 命名空间项条数（无文件系统目标、由 shell 直接启动）。
    pub fn shell_items(&self) -> usize {
        self.items.iter().filter(|s| s.is_shell_item()).count()
    }
}

/// 扫描配置中指定的全部来源。
pub fn scan(sources: &Sources) -> Result<ScanReport> {
    // 整个扫描过程共用一次 COM 初始化
    let _com = ComGuard::sta()?;
    // 以及一个解析器实例
    let resolver = Resolver::new()?;

    let started = Instant::now();
    let mut report = ScanReport::default();
    // 同一文件可能同时出现在多个来源（例如桌面被重定向），去重避免重复项
    let mut seen: HashSet<String> = HashSet::new();

    for (root, origin) in collect_roots(sources) {
        walk(&root, origin, sources.recursive, 0, &resolver, &mut seen, &mut report);
    }

    report.items.sort_by_key(|s| s.name.to_lowercase());
    report.elapsed = started.elapsed();
    Ok(report)
}

/// 汇总需要扫描的根目录及其来源标记，跳过不存在的。
fn collect_roots(sources: &Sources) -> Vec<(PathBuf, Origin)> {
    let mut roots = Vec::new();

    if sources.user_start_menu {
        push_known(&mut roots, &FOLDERID_Programs, Origin::UserStartMenu, "用户开始菜单");
    }
    if sources.common_start_menu {
        push_known(
            &mut roots,
            &FOLDERID_CommonPrograms,
            Origin::CommonStartMenu,
            "公共开始菜单",
        );
    }
    if sources.user_desktop {
        push_known(&mut roots, &FOLDERID_Desktop, Origin::UserDesktop, "用户桌面");
    }
    if sources.public_desktop {
        push_known(
            &mut roots,
            &FOLDERID_PublicDesktop,
            Origin::PublicDesktop,
            "公共桌面",
        );
    }

    roots.extend(
        sources
            .extra_dirs
            .iter()
            .filter(|p| p.is_dir())
            .cloned()
            .map(|p| (p, Origin::Extra)),
    );
    roots
}

fn push_known(
    out: &mut Vec<(PathBuf, Origin)>,
    folder: &GUID,
    origin: Origin,
    label: &'static str,
) {
    match known_folder(folder) {
        Some(p) => out.push((p, origin)),
        None => eprintln!("[warn] 无法定位已知文件夹: {label}"),
    }
}

/// 查询已知文件夹路径。
///
/// 系统返回的字符串是 `CoTaskMemAlloc` 分配的，**必须**用 `CoTaskMemFree` 释放，
/// 否则每次扫描都会泄漏一点。
fn known_folder(folder: &GUID) -> Option<PathBuf> {
    // SAFETY: folder 是 'static 的 GUID 常量；htoken 传 None 表示当前用户。
    unsafe {
        let raw: PWSTR = SHGetKnownFolderPath(folder, KF_FLAG_DEFAULT, None).ok()?;
        let text = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const core::ffi::c_void));
        text.map(PathBuf::from)
    }
}

/// 递归遍历目录，收集 `.lnk`。
fn walk(
    dir: &Path,
    origin: Origin,
    recursive: bool,
    depth: u32,
    resolver: &Resolver,
    seen: &mut HashSet<String>,
    report: &mut ScanReport,
) {
    if depth > MAX_DEPTH {
        return;
    }

    // 权限不足等错误直接跳过：一个读不了的目录不该中断整次扫描
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            if recursive {
                walk(&path, origin, recursive, depth + 1, resolver, seen, report);
            }
            continue;
        }

        if !is_lnk(&path) {
            continue;
        }
        if !seen.insert(path.to_string_lossy().to_lowercase()) {
            continue;
        }

        match resolver.resolve(&path, origin) {
            // shell 命名空间项也走这条路：`is_usable` 对它们恒为真
            Ok(sc) if sc.is_usable() => report.items.push(sc),
            Ok(sc) => {
                report.broken += 1;
                report
                    .failures
                    .push((path, format!("目标不存在: {}", sc.target.display())));
            }
            Err(e) => {
                report.skipped += 1;
                report.failures.push((path, e.to_string()));
            }
        }
    }
}

/// 是否是 `.lnk` 文件（扩展名大小写不敏感）。
fn is_lnk(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
}
