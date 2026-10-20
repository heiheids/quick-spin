//! 把拖入的东西落成程序自己目录下的 `.lnk`。
//!
//! # 为什么单独一个模块
//!
//! 拖放有**两条**接收路径：老式 `WM_DROPFILES`（只有文件系统路径）和 OLE
//! `IDropTarget`（壳命名空间项 + 文件）。两条路最终都要回答同一个问题 ——
//! "这个东西该落成什么快捷方式"。这个判断曾经在两边各写一份，而且**两份不一致**：
//!
//! * `WM_DROPFILES` 那条（`App::on_files_dropped`）只收 `.lnk`，
//!   拖进来一个 `.exe` 被**静默丢掉** —— 表现就是"拖执行程序进来毫无反应"；
//! * OLE 那条什么都收，`.exe` 会建快捷方式 —— 但**工作目录留空**。
//!
//! 后者比"没反应"更难查：快捷方式看起来完全正常、双击也能开，
//! 只有程序自己按相对路径找配置/依赖时才失败。工作目录留空时，
//! 被启动的进程继承的是**本程序**的目录（实测：把 `cmd.exe` 拖进来，
//! 它里面的 `cd` 打印的是 `E:\工程-快启桌面管理\target\release\`）。
//! 用户的感受就是"这个条目点了起不来"。
//!
//! 所以判定规则收成一处（[`store`]），两条路都只调它。

use std::path::{Path, PathBuf};

use crate::error::{AppError, Result};

/// 一个拖入项的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source<'a> {
    /// 文件系统里的东西：文件、文件夹、快捷方式。
    File(&'a Path),
    /// 壳命名空间项。`parsing` 是解析名（`::{CLSID}` / `ms-settings:` /
    /// UWP 的 AUMID），它没有文件系统路径。
    Shell(&'a str),
}

/// 把 OLE 拖放给出的**解析名**归类成 [`Source`]。
///
/// 这一步只做一次、且只有一处 —— 曾经 `drop_target` 自己写了一遍
/// （"以 `::` 开头就是壳项"），漏掉了 `ms-settings:` 与 UWP 的 AUMID，
/// 于是把设置页当文件路径去建快捷方式，得到一条指向
/// `C:\...\ms-settings:` 这种不存在路径的坏条目。
///
/// 判据按**从强到弱**排：
///
/// 1. `::` 开头 —— CLSID 解析名。**必须先判它**：控制面板的解析名是
///    `::{26EE0668-…}\0`，带一个反斜杠，按"含 `\` 就是路径"会误判。
/// 2. 带 URI 协议头（`ms-settings:`、`shell:AppsFolder\…`、`https://…`）。
/// 3. UWP 的 AUMID（`包名!应用`）—— 没有 `\` 也没有协议头。
/// 4. 其余按文件路径。
///
/// 判错一位不会报错，只会**静默生成一条坏条目**（指向不存在的路径），
/// 而界面看起来一切正常 —— 所以每一条判据都有对应的测试。
pub fn classify(parsing: &str) -> Source<'_> {
    if parsing.is_empty() {
        // 建一条"指向空路径"的快捷方式没有意义，直接交给壳项路径去失败
        return Source::Shell(parsing);
    }
    // 1. CLSID
    if parsing.starts_with("::") {
        return Source::Shell(parsing);
    }
    // 2. 协议式壳项 / URI
    if has_uri_scheme(parsing) {
        return Source::Shell(parsing);
    }
    // 3. UWP AUMID
    if is_aumid(parsing) {
        return Source::Shell(parsing);
    }
    // 4. 文件路径
    Source::File(Path::new(parsing))
}

/// 这个字符串带 URI 协议头吗（`ms-settings:`、`shell:…`、`https://…`）。
///
/// 判据是"冒号出现在第 2 个字符之后，且前面全是协议名允许的字符"。
/// 关键是**排除盘符**：`C:\…` 的冒号在第 1 位，不能算协议。
fn has_uri_scheme(text: &str) -> bool {
    let Some(colon) = text.find(':') else {
        return false;
    };
    // `C:` 的冒号在下标 1；下标 0 的 `:xxx` 不是合法协议。
    if colon <= 1 {
        return false;
    }
    text[..colon]
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// UWP 应用的 AUMID（`包名_发布者!应用`）吗。
///
/// 特征是有 `!`、没有路径分隔符、也没有协议头：
/// `Microsoft.Paint_8wekyb3d8bbwe!App`。
fn is_aumid(parsing: &str) -> bool {
    parsing.contains('!')
        && !parsing.contains('\\')
        && !parsing.contains('/')
        && !has_uri_scheme(parsing)
}

/// 把一个拖入项落成快捷方式，返回落盘的完整路径。
///
/// `dest` 由调用方用 [`unique_dest`] 在同名时加序号（绝不覆盖）。
pub fn store(dest: &Path, display: &str, source: Source<'_>) -> Result<()> {
    match source {
        // 本身是快捷方式就原样复制：它内部可能带着参数、工作目录、自定义图标，
        // 重建一个指向它目标的快捷方式会把这些全丢掉。
        Source::File(src)
            if src
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("lnk")) =>
        {
            std::fs::copy(src, dest).map_err(|e| AppError::io(src, e))?;
        }
        // 其余文件与文件夹（**含 `.exe`**）一律建快捷方式。
        //
        // 刻意不按扩展名白名单过滤：对用户来说，一个 `.exe`、一个目录、
        // 一张图片都是"我想把它放进启动器"。原先只在 `.lnk` 上放行，
        // 正是"拖执行程序进来没反应"的直接原因。
        Source::File(src) => {
            crate::shell::lnk::create_link(dest, src, "", display)?;
        }
        Source::Shell(parsing) => {
            crate::shell::lnk::create_shell_shortcut(dest, parsing, display)?;
        }
    }
    Ok(())
}

/// 把显示名里不能做文件名的字符换掉。
///
/// 壳项的显示名是本地化的自由文本，可能含 `\ / : * ? " < > |` 等，
/// 直接当文件名会创建失败。
pub fn sanitize(name: &str) -> String {
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

/// 在同名文件已存在时另找一个名字，绝不覆盖。
///
/// 覆盖会让"拖了两次同一个东西"丢掉先前的条目。
pub fn unique_dest(dir: &Path, display: &str) -> PathBuf {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_illegal_chars() {
        assert_eq!(sanitize(r#"a/b:c*d?e"f<g>h|i"#), "a_b_c_d_e_f_g_h_i");
        assert_eq!(sanitize("   "), "新条目");
        // 中文与空格要原样保留
        assert_eq!(sanitize("微信 App"), "微信 App");
    }

    #[test]
    fn unique_dest_never_overwrites() {
        let dir = std::env::temp_dir().join("quick-spin-drop-plan-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let first = unique_dest(&dir, "微信");
        assert_eq!(first.file_name().unwrap(), "微信.lnk");
        std::fs::write(&first, b"").unwrap();

        let second = unique_dest(&dir, "微信");
        assert_eq!(second.file_name().unwrap(), "微信 (2).lnk");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 解析名归类：每个真实出现过的形态都要归对。
    ///
    /// 逐条钉死是因为**判错一位不会报错**，只会静默生成一条坏条目
    ///（指向 `C:\…\ms-settings:` 这类不存在的路径），界面看起来一切正常。
    #[test]
    fn classify_covers_every_real_shape() {
        // 1. CLSID 解析名（含带反斜杠的控制面板）
        for name in [
            "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}",
            "::{645FF040-5081-101B-9F08-00AA002F954E}",
            // 这个是 key：带一个 `\0`，"含 \ 就是路径"会把它误判成文件
            "::{26EE0668-A00A-44D7-9371-BEB064C98683}\\0",
            "::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}",
        ] {
            assert!(
                matches!(classify(name), Source::Shell(s) if s == name),
                "应当归为壳项: {name}"
            );
        }

        // 2. 协议式壳项 / URI
        for name in [
            "ms-settings:",
            "shell:AppsFolder\\Microsoft.Paint_8wekyb3d8bbwe!App",
            "https://www.bilibili.com/",
        ] {
            assert!(
                matches!(classify(name), Source::Shell(_)),
                "应当归为壳项: {name}"
            );
        }

        // 3. UWP 的 AUMID
        for name in [
            "Microsoft.Paint_8wekyb3d8bbwe!App",
            "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic",
        ] {
            assert!(
                matches!(classify(name), Source::Shell(_)),
                "应当归为壳项: {name}"
            );
        }

        // 4. 文件路径：盘符、UNC、中文目录，都必须是文件
        for name in [
            r"C:\Windows\System32\notepad.exe",
            r"D:\Program Files\Tencent\Weixin\Weixin.exe",
            r"\\server\share\x.exe",
            r"E:\游戏",
        ] {
            assert!(
                matches!(classify(name), Source::File(_)),
                "应当归为文件: {name}"
            );
        }
    }

    /// 拖一个 exe 进来：必须落成快捷方式，且**带正确的起始位置**。
    ///
    /// 这条是本次 bug 的回归锁：之前要么根本不建条目（被扩展名过滤丢掉），
    /// 要么建出来的快捷方式起始位置为空 —— 后者启动起来会用启动器的目录。
    #[test]
    fn exe_drop_creates_working_shortcut() {
        // COM 必须先初始化（`create_link` 走 IShellLinkW）
        let _com = crate::shell::com::ComGuard::sta().expect("CoInitializeEx");

        let dir = std::env::temp_dir().join("quick-spin-drop-exe-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 用真实存在的一个 exe（notepad 在每台机器上都有）
        let src = Path::new(r"C:\Windows\System32\notepad.exe");
        assert!(src.is_file(), "测试前提：notepad.exe 应当存在");

        let dest = unique_dest(&dir, "记事本测试");
        store(&dest, "记事本测试", Source::File(src)).expect("建快捷方式失败");
        assert!(dest.is_file(), "快捷方式没有落盘");
        // 解析回来核对目标与起始位置
        let shortcut = crate::shell::lnk::Resolver::new()
            .expect("Resolver")
            .resolve(&dest, crate::shell::lnk::Origin::Extra)
            .expect("解析刚建好的快捷方式失败");

        assert_eq!(
            shortcut.target.to_string_lossy().to_lowercase(),
            src.to_string_lossy().to_lowercase(),
            "目标不对"
        );
        assert_eq!(
            shortcut.work_dir.to_string_lossy().to_lowercase(),
            r"c:\windows\system32",
            "起始位置必须是 exe 自己所在目录，否则启动起来的程序会用启动器的目录"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 指向**文件夹**的快捷方式，起始位置取那个文件夹自己。
    #[test]
    fn folder_drop_uses_folder_as_working_dir() {
        let _com = crate::shell::com::ComGuard::sta().expect("CoInitializeEx");

        let dir = std::env::temp_dir().join("quick-spin-drop-dir-test");
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join("游戏");
        std::fs::create_dir_all(&target).unwrap();

        let dest = unique_dest(&dir, "游戏");
        store(&dest, "游戏", Source::File(&target)).expect("建快捷方式失败");

        let shortcut = crate::shell::lnk::Resolver::new()
            .unwrap()
            .resolve(&dest, crate::shell::lnk::Origin::Extra)
            .unwrap();
        assert_eq!(shortcut.work_dir, target, "文件夹的起始位置应当是它自己");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 网址不该被推导出一个"父目录"。
    ///
    /// 这条守的是给 `create_link` 加默认起始位置时容易踩的坑：
    /// `https://example.com` 的 `parent()` 会拼出 `https:` 这种垃圾。
    #[test]
    fn url_shortcut_has_no_working_dir() {
        let _com = crate::shell::com::ComGuard::sta().expect("CoInitializeEx");

        let dir = std::env::temp_dir().join("quick-spin-drop-url-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let url = "https://example.com/page";
        let dest = unique_dest(&dir, "example.com");
        store(&dest, "example.com", Source::File(Path::new(url))).expect("建快捷方式失败");

        let shortcut = crate::shell::lnk::Resolver::new()
            .unwrap()
            .resolve(&dest, crate::shell::lnk::Origin::Extra)
            .unwrap();
        assert!(
            shortcut.work_dir.as_os_str().is_empty(),
            "网址快捷方式不该有起始位置，实际是 {:?}",
            shortcut.work_dir
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
