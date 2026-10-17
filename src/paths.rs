//! 全部文件位置集中在这里。
//!
//! 之前把 `join("QuickSpin")` 散在各个模块里，改一次目录名就要全局搜，
//! 而且很容易出现某处大小写不一致、凭空多出一个目录。集中一处就没有这个问题。
//!
//! **布局：数据与 exe 同目录（便携 / 绿色软件布局）。**
//! `config.toml`、`state.toml`、`shortcuts\`、`icon-cache\`、`lang\` 都放在
//! exe 旁边，整个文件夹拷走即用，备份程序目录时会连配置一起备份到。
//!
//! 代价有两个，都是有意的取舍：
//! 1. exe 所在目录必须可写。放进 `C:\Program Files\` 这类需要管理员权限的
//!    位置会写不进去 —— 本程序定位是便携工具，不打算支持那种安装方式。
//! 2. 开发时 exe 在 `target\release\`，`cargo clean` 会把配置和快捷方式一起删掉。
//!    要在开发时保住数据，把 exe 复制到一个单独目录再运行。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::Result;

/// 旧版数据目录名，仅用于一次性迁移。
///
/// 顺序有意义：先搬 `QuickSpin`（改名后的 %APPDATA% 目录），再搬 `QuickLaunch`
/// （最早的名字）。两个都按"新目录里还没有 `shortcuts\` 才搬"的规则处理，
/// 所以重复跑也不会互相覆盖。
const LEGACY_DIRS: [&str; 2] = ["QuickSpin", "QuickLaunch"];

/// 数据目录 = **exe 所在目录**。
pub fn data_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| {
            // 正常可执行文件不可能没有父目录；走到这里说明路径本身就异常。
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("无法确定可执行文件所在目录: {}", exe.display()),
            )
        })?
        .to_path_buf();
    migrate_legacy_dir_once(&dir);
    Ok(dir)
}

/// 用户配置：`config.toml`
pub fn config_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("config.toml"))
}

/// 程序运行时状态：`state.toml`
pub fn state_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("state.toml"))
}

/// 外部语言文件目录：`lang`
pub fn lang_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("lang"))
}

/// 图标缓存目录：`icon-cache`
pub fn icon_cache_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("icon-cache"))
}

/// 本程序自己管理的快捷方式目录：`shortcuts`
///
/// 这是**唯一的条目来源**。按需求"暂不自动扫描、全部手动添加"，
/// 用户拖进来的 `.lnk` 会被复制到这里一份，程序只列这个目录里的东西。
///
/// 复制而不是引用原路径的原因：原快捷方式被移动、改名、删除后条目不会失效，
/// 程序的数据是自包含的（代价是磁盘上多一份几 KB 的 `.lnk`）。
pub fn shortcuts_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("shortcuts"))
}

/// 把旧版 `%APPDATA%\QuickSpin` / `%APPDATA%\QuickLaunch` 里的数据搬到
/// exe 同目录。只跑一次（`OnceLock`）。
///
/// 失败**不致命**：搬不过来就照常用新目录，最坏是让用户重配一遍，
/// 绝不能让程序因此起不来。
fn migrate_legacy_dir_once(new_dir: &Path) {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let Some(base) = std::env::var_os("APPDATA") else {
            return;
        };
        let base = PathBuf::from(base);
        for name in LEGACY_DIRS {
            let old = base.join(name);
            if !old.is_dir() {
                continue;
            }
            if let Err(e) = migrate_one(&old, new_dir) {
                eprintln!(
                    "迁移旧数据目录失败（不影响程序运行）: {} -> {}: {e}",
                    old.display(),
                    new_dir.display()
                );
            }
        }
    });
}

fn migrate_one(old_dir: &Path, new_dir: &Path) -> std::io::Result<()> {
    // 新目录里已经有 `shortcuts\`，说明用户已经在用新布局了 ——
    // 这时候再搬旧的过来就是在毁他的数据，直接放弃。
    if new_dir.join("shortcuts").exists() {
        return Ok(());
    }

    // 只删我们自己生成的那两个默认文件。
    //
    // **绝不能 `remove_dir_all(new_dir)`**：数据目录就是 exe 所在目录，
    // 里面有 exe 本身和一堆构建产物，整目录删掉会把程序删了。
    for name in ["config.toml", "state.toml"] {
        let p = new_dir.join(name);
        if p.is_file() {
            let _ = std::fs::remove_file(&p);
        }
    }

    // 必须"复制后删源"而不是 `rename`：旧目录在 C: 盘、新目录在 E: 盘，
    // 跨卷 rename 会直接失败（os error 17）。
    for entry in std::fs::read_dir(old_dir)? {
        let entry = entry?;
        let to = new_dir.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            if to.exists() {
                let _ = std::fs::remove_dir_all(&to);
            }
            copy_dir_all(&entry.path(), &to)?;
            let _ = std::fs::remove_dir_all(entry.path());
        } else {
            std::fs::copy(entry.path(), &to)?;
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let _ = std::fs::remove_dir(old_dir);

    // 搬到新位置后，配置里记录的路径全部失效，必须改写。
    // 这一步漏了的话，覆盖表的键会立刻变成"找不到文件"，
    // 紧接着 cleanup 会把用户的隐藏、分组设置当成垃圾清掉。
    rewrite_dir_refs(new_dir, old_dir);
    Ok(())
}

/// 把配置/状态里残留的旧数据目录路径改写成新目录。
///
/// **这一步不能省。** 覆盖表（`state.toml`）的键是**源 `.lnk` 的绝对路径**，
/// 手动添加的条目路径里嵌着数据目录。只搬目录而不同步改键，这些键会立刻变成
/// 失效路径，紧接着 `cleanup` 会把它们当成垃圾清掉 —— 表现是用户的隐藏、
/// 分组设置**无声消失**。
///
/// 用字面量替换而不是解析 TOML 再序列化：TOML 单引号字面量字符串保持反斜杠
/// 原样，与 `to_string_lossy()` 出来的路径形式一致，直接替换即可，
/// 也避免了"解析失败就整份配置丢掉"的风险。
fn rewrite_dir_refs(new_dir: &Path, old_dir: &Path) {
    let old = old_dir.to_string_lossy().to_string();
    let new = new_dir.to_string_lossy().to_string();
    if old == new {
        return;
    }
    for name in ["config.toml", "state.toml"] {
        let p = new_dir.join(name);
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        if !text.contains(&old) {
            continue;
        }
        if let Err(e) = std::fs::write(&p, text.replace(&old, &new)) {
            eprintln!("改写 {name} 里的旧目录路径失败: {e}");
        }
    }
}

fn copy_dir_all(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dst = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &dst)?;
        } else {
            std::fs::copy(entry.path(), &dst)?;
        }
    }
    Ok(())
}
