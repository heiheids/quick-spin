//! 把 `assets/app.ico` 作为**图标资源**编进 exe。
//!
//! # 为什么需要它，以及为什么不用现成的 crate
//!
//! 托盘图标是在运行时从嵌入的字节里加载的（见 `platform/tray.rs`），
//! 那已经够了。但 exe **文件本身**在资源管理器 / 任务栏里显示的是 PE 资源段里的
//! 图标 —— 没有这一步，exe 文件永远是默认的空白图标。
//!
//! 常见的做法是引入 `embed-resource` / `winres` 之类的构建依赖。这里不用，
//! 因为它们最终也是去找 `rc.exe` 并调用链接器，而**本机已经有 rc.exe**
//!（随 MSVC 工具链的 Windows SDK 一起装的）。少一个依赖就少一处
//! 版本兼容与联网拉取的问题。
//!
//! # 找不到 rc.exe 时怎么办
//!
//! **不报错，只打印一条警告。** 图标资源属于"锦上添花"：没有它程序照常编译、
//! 照常运行（托盘图标仍然正常）。为此让整个构建失败是不划算的 ——
//! 尤其在没有装 Windows SDK 的机器上。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // 图标变了要重新编译资源
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = std::env::var_os("OUT_DIR").map(PathBuf::from);
    let Some(out_dir) = out_dir else {
        return;
    };

    let Some(rc) = find_rc() else {
        println!("cargo:warning=未找到 rc.exe，跳过图标资源（exe 文件将没有自定义图标）");
        return;
    };

    // 生成 .rc 脚本。
    //
    // 两个都要小心：
    //
    // 1. **不能用 `fs::canonicalize`**：Windows 上它返回 `\\?\` 逐字路径
    //    （形如 `\\?\E:\...`），而 `rc.exe` 不认这个前缀 —— 报错信息还会被
    //    截断成一个毫无线索的盘符。这里改用 `current_dir()` 拼绝对路径，
    //    它是普通形式。（构建脚本的工作目录就是工程根。）
    // 2. 路径里的反斜杠在资源脚本里是**转义字符**，必须换成正斜杠，
    //    否则 `C:\Windows` 里的 `\W` 会被当成转义序列。
    let ico_rel = Path::new("assets/app.ico");
    if !ico_rel.is_file() {
        println!(
            "cargo:warning=找不到 {}，跳过图标资源",
            ico_rel.display()
        );
        return;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let ico_rc = cwd.join(ico_rel).display().to_string().replace('\\', "/");
    // 1 是图标组的资源 ID。名字不重要，ID 有效即可。
    let rc_text = format!("1 ICON \"{ico_rc}\"\n");
    let rc_path = out_dir.join("app.rc");
    if let Err(e) = std::fs::write(&rc_path, rc_text) {
        println!("cargo:warning=写 app.rc 失败: {e}");
        return;
    }

    let res_path = out_dir.join("app.res");
    let output = Command::new(&rc)
        .arg("/nologo")
        // **必须显式指定代码页为 UTF-8**。
        //
        // `rc.exe` 默认按系统 ANSI 代码页（简体中文环境是 GBK）读 `.rc` 文件，
        // 而我们写的是 UTF-8 —— 目录名里的中文会被按 GBK 误解释，
        // 于是它找不到图标文件。报错信息还极其误导：退出码 1，输出只剩一个
        // 被截断的盘符（`E:\`），看起来像路径拼接错了。
        .arg("/c")
        .arg("65001")
        .arg(format!("/fo{}", res_path.display()))
        .arg(&rc_path)
        .output();

    match output {
        Ok(o) if o.status.success() => {
            // MSVC 的 link.exe 可以直接把 .res 当输入文件。
            println!("cargo:rustc-link-arg={}", res_path.display());
        }
        Ok(o) => {
            // rc.exe 的报错在 stdout 上（不是 stderr），而且可能被截断 ——
            // 所以把完整输出和退出码都打出来，否则只剩一个盘符字母可查。
            let msg = String::from_utf8_lossy(&o.stdout);
            println!(
                "cargo:warning=rc.exe 编译图标失败（退出码 {:?}）: {}",
                o.status.code(),
                msg.trim()
            );
        }
        Err(e) => println!("cargo:warning=调用 rc.exe 失败: {e}"),
    }
}

/// 找 `rc.exe`。挑**版本号最大**的那个 SDK。
///
/// 机器上常同时装着多个版本的 Windows SDK，用目录名排序取最后一个
/// 比写死版本号稳 —— 写死会在 SDK 升级后失效。
fn find_rc() -> Option<PathBuf> {
    let mut roots = Vec::new();
    for key in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(base) = std::env::var_os(key) {
            roots.push(PathBuf::from(base).join("Windows Kits/10/bin"));
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            // 目标架构要与宿主一致：x64 的 Rust 工具链要 x64 的 rc.exe。
            let exe = entry.path().join("x64/rc.exe");
            if exe.is_file() {
                candidates.push(exe);
            }
        }
    }

    // `read_dir` 的顺序不保证，这里按路径排序后取最后一个（版本号最大的）。
    candidates.sort();
    candidates.pop()
}
