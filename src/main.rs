//! Quick spin 程序入口。
//!
//! 界面、渲染、快捷方式三条链路均已实现，用 `--selftest` / `--diagnose` 自检。
//! 框架设计见 `docs/ARCHITECTURE.md`。
//!
//! ## 为什么编成 GUI 子系统
//!
//! `windows_subsystem = "windows"` 让双击 exe **不再弹出控制台黑框**。
//! 本程序是常驻托盘、可"启动后自动收起"的启动器，跟着一个黑框是不能接受的。
//!
//! 代价是进程默认**没有可用的标准句柄**，所以两种模式要分别接上
//!（见 [`prepare_console`]）：命令行模式接控制台，图形界面模式接 `NUL`。

// 无参数启动界面时不要控制台窗口；命令行模式的输出由 `prepare_console` 负责。
#![windows_subsystem = "windows"]

mod app;
mod config;
mod error;
mod i18n;
mod model;
mod paths;
mod platform;
mod render;
mod shell;
mod state;
mod ui;

use std::process::ExitCode;

use config::SchemeBook;
use i18n::I18n;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let first = args.next().unwrap_or_default();

    // 有子命令 = 命令行模式（要能看到输出）；无参数或 `--gui` = 图形界面模式
    //（要**没有**控制台窗口）。判据与下面的分派保持一致。
    prepare_console(!first.is_empty() && first != "--gui");

    match first.as_str() {
        // 无参数即启动界面 —— 双击 exe 的预期行为
        "" | "--gui" => run_gui(),
        "--selftest" => selftest(),
        "--icontest" => icontest(args.next(), args.next(), args.next()),
        "--diagnose" | "scan" => diagnose(),
        "--open" => open_by_name(args.next()),
        _ => {
            usage();
            ExitCode::SUCCESS
        }
    }
}

/// 按运行模式把标准句柄接到「控制台」或 `NUL`。
///
/// 本程序编成 GUI 子系统（理由见文件头），**双击时不会自动获得控制台**，
/// 标准句柄是空的。于是两种模式要分别处理：
///
/// * **命令行模式**：先挂到父进程的控制台 —— 从 cmd / PowerShell 启动时，
///   输出就落回那个窗口，不会另开一个新的；父进程没有控制台（双击、或被别的
///   GUI 程序拉起）才自己 `AllocConsole` 开一个，否则用户会以为程序没反应。
/// * **图形界面模式**：接到 `NUL`。**这一条不能省** —— Rust 的 `print_to`
///   在写入失败时会 `panic!`，而本工程是 `panic = "abort"`，于是一句
///   "写不出去的日志"会把整个界面**直接终止**，且不留任何痕迹。
///   接到 `NUL` 之后写入永远成功、内容丢弃，这类崩溃就不可能发生。
///
/// 已经拿到有效标准输出时**什么都不做**：那说明输出被重定向到了管道或文件
///（`quick-spin --diagnose | findstr ...`），覆盖成 `CONOUT$` 会把管道打断。
fn prepare_console(cli: bool) {
    use windows::Win32::System::Console::{
        AllocConsole, AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
    };

    // SAFETY: 都是无参/常量入参的控制台 API；只影响本进程的标准句柄。
    unsafe {
        if GetStdHandle(STD_OUTPUT_HANDLE).is_ok() {
            return;
        }
        if cli {
            if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
                let _ = AllocConsole();
            }
            bind_std_handles("CONOUT$", "CONIN$");
        } else {
            bind_std_handles("NUL", "NUL");
        }
    }
}

/// 打开设备名，把标准输出/错误/输入指过去。
///
/// 用 `std::fs` 而不是 `CreateFileW`：设备名（`CONOUT$` / `NUL` / `CONIN$`）
/// 走的是同一条打开路径，省掉一次裸句柄拼装。
///
/// 句柄**故意泄漏**（`mem::forget`）：`File` 一 drop 就把句柄关掉，
/// 标准输出随即变成野句柄，之后每一次 `println!` 都会失败。
/// 泄漏的是三个进程级句柄，进程退出时由系统回收。
fn bind_std_handles(out_dev: &str, in_dev: &str) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{
        SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    let open = |name: &str| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
            .ok()
    };

    if let Some(f) = open(out_dev) {
        let handle = HANDLE(f.as_raw_handle());
        // SAFETY: handle 来自刚打开、且被 forget 掉（不会关闭）的文件。
        unsafe {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, handle);
            let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
        }
        std::mem::forget(f);
    }
    if let Some(f) = open(in_dev) {
        let handle = HANDLE(f.as_raw_handle());
        // SAFETY: 同上。
        unsafe {
            let _ = SetStdHandle(STD_INPUT_HANDLE, handle);
        }
        std::mem::forget(f);
    }
}

fn usage() {
    println!("Quick spin v{}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("用法:");
    println!("  quick-spin                            启动界面");
    println!("  quick-spin --selftest                 跑一遍图形链路并实测内存（不显示窗口）");
    println!("  quick-spin --diagnose                 扫描快捷方式并打印统计（不启界面）");
    println!("  quick-spin --open <名称关键词>        按名称启动一个快捷方式");
    println!("  quick-spin --diagnose                 列出扫描到的条目与来源统计");
    println!("  quick-spin --help                     显示本帮助");
    println!();
    println!("框架设计见 docs/ARCHITECTURE.md。");
}

/// 启动图形界面。
fn run_gui() -> ExitCode {
    // 单实例检查放在**最前面**：要在建窗口、挂托盘、注册全局热键之前完成。
    //
    // 顺序很要紧：如果先建窗口再检查，第二个实例会先往托盘里塞一个图标
    // （于是短暂出现两个图标），还会去抢同一个全局热键（必然失败并打日志），
    // 然后才退出 —— 用户看到的是"闪了一下、日志里有报错"。
    match config::open() {
        Ok(store) => {
            let wanted = store.get().general.single_instance;
            if wanted {
                match platform::single_instance::acquire_or_activate() {
                    // 已有实例，已经把它叫到前面，本次启动到此为止
                    Ok(false) => return ExitCode::SUCCESS,
                    Ok(true) => {}
                    // 单实例机制本身失败时**照常启动**：宁可多开一个窗口，
                    // 也不要因为一个保护性功能出错而让程序打不开。
                    Err(e) => eprintln!("单实例检查失败，按多实例继续启动: {e}"),
                }
            }
        }
        // 配置读不出来时也照常启动：`App::new` 里还会再读一次并报真正的错。
        Err(e) => eprintln!("读取配置失败（跳过单实例检查）: {e}"),
    }

    match app::App::new().and_then(|a| a.run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("启动失败: {e}");
            ExitCode::FAILURE
        }
    }
}

/// 图形链路自检：**不显示窗口**，但把整条渲染路径真正跑一遍。
///
/// 这样能在不打扰用户、也不需要看画面的前提下回答三个问题：
/// 1. D3D11 / D2D1 / DComposition 这条链路在本机能不能建起来；
/// 2. 请求的背景材质有没有被系统降级；
/// 3. 每个阶段真实吃掉多少内存（而不是靠估算）。
/// 自检图标提取：`--icontest [路径] [尺寸] [导出PNG路径]`。
///
/// 这是"HBITMAP → BGRA → 预乘"整条转换链路的验证入口。
/// 之所以要专门验证：这条链路里任何一个环节出错（行序反了、alpha 没预乘、
/// 位深不对）都不会报错，只会让图标**看起来不对**（上下颠倒、亮边、全透明），
/// 而视觉问题人工逐个核对成本很高。
fn icontest(path: Option<String>, size: Option<String>, dump: Option<String>) -> ExitCode {
    // 图标提取走 shell COM，必须先初始化。跟主程序一致用 STA。
    let _com = match shell::com::ComGuard::sta() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("COM 初始化失败: {e}");
            return ExitCode::FAILURE;
        }
    };

    let size: u32 = size.and_then(|s| s.parse().ok()).unwrap_or(128);

    println!("图标提取自检（期望边长 {size}px）");
    println!("{}", "─".repeat(64));

    // 覆盖不同类型的路径：它们的图标来源完全不同
    // （快捷方式要解引用、dll 要从资源段取、文件夹用系统图标）。
    let mut cases: Vec<(&str, std::path::PathBuf)> = Vec::new();
    if let Some(p) = path {
        cases.push(("命令行指定", std::path::PathBuf::from(p)));
    }
    match find_a_lnk() {
        Some(p) => cases.push(("快捷方式", p)),
        None => println!("（没找到可用的 .lnk，跳过快捷方式用例）"),
    }
    cases.push((
        "可执行文件",
        std::path::PathBuf::from(r"C:\Windows\System32\notepad.exe"),
    ));
    cases.push(("文件夹", std::path::PathBuf::from(r"C:\Windows")));
    cases.push((
        "图标资源 dll",
        std::path::PathBuf::from(r"C:\Windows\System32\shell32.dll"),
    ));
    cases.push((
        "不存在的路径",
        std::path::PathBuf::from(r"C:\__不存在_icon_selftest__\nope.lnk"),
    ));

    let mut ok_count = 0usize;
    let mut bad_count = 0usize;

    for (label, p) in &cases {
        let started = std::time::Instant::now();
        match platform::icon::extract(p, size) {
            Ok(icon) => {
                let elapsed = started.elapsed();
                let opaque = icon.opaque_ratio();
                let violations = icon.premultiply_violations();
                let expected = icon.width as usize * icon.height as usize * 4;

                let size_ok = icon.bgra.len() == expected;
                // 图标应该是"有内容"的：全透明说明取到了空图
                let alpha_ok = opaque > 0.01;
                let premul_ok = violations == 0;

                println!(
                    "{label:<12}: {w}×{h}, {bytes} 字节, 不透明像素 {pct:.0}%, {ms:.0}ms",
                    w = icon.width,
                    h = icon.height,
                    bytes = icon.bytes(),
                    // opaque_ratio 返回的是 0~1 的比例，这里要乘 100 ——
                    // 之前漏乘，把 60% 印成了 "0.6%"，看起来像图标几乎是空的
                    pct = opaque * 100.0,
                    ms = elapsed.as_secs_f64() * 1000.0,
                );

                if size_ok && alpha_ok && premul_ok {
                    ok_count += 1;
                } else {
                    bad_count += 1;
                    if !size_ok {
                        println!(
                            "              ⚠ 缓冲区长度 {} 与 {}×{}×4={} 不符",
                            icon.bgra.len(),
                            icon.width,
                            icon.height,
                            expected
                        );
                    }
                    if !alpha_ok {
                        println!("              ⚠ 整张图几乎全透明，图标可能是空的");
                    }
                    if !premul_ok {
                        println!(
                            "              ⚠ {violations} 个像素违反预乘上界（channel > alpha），会出现亮边"
                        );
                    }
                }
            }
            Err(e) => {
                // 只有"不存在的路径"这一条**应该**失败；其余失败要标出来
                if label == &"不存在的路径" {
                    println!("{label:<12}: 按预期返回错误 —— {e}");
                } else {
                    println!("{label:<12}: ⚠ 提取失败 —— {e}");
                    bad_count += 1;
                }
            }
        }
    }

    println!("{}", "─".repeat(64));
    println!("通过 {ok_count} 项，异常 {bad_count} 项");

    // 顺带看一眼尺寸档位：每个档位都应拿到对应的边长
    println!("\n尺寸档位验证:");
    if let Some((_, p)) = cases.iter().find(|(l, _)| *l == "可执行文件") {
        for s in [16u32, 32, 64, 128, 256] {
            match platform::icon::extract(p, s) {
                Ok(icon) => println!("  请求 {s:>3}px → 实得 {}×{}", icon.width, icon.height),
                Err(e) => println!("  请求 {s:>3}px → 失败: {e}"),
            }
        }
    }

    // 导出预览图：数值对不代表图像对（可能上下颠倒/斜切），必须看一眼
    if let Some(out) = dump {
        // 优先用命令行指定的路径 —— 显式传参就该被尊重，
        // 否则想验证某个特定图标时会拿到快捷方式那张，白白排查一轮
        let source = cases
            .iter()
            .find(|(l, _)| *l == "命令行指定")
            .or_else(|| cases.iter().find(|(l, _)| *l == "快捷方式"))
            .or_else(|| cases.first())
            .map(|(_, p)| p.clone());
        match source {
            Some(src) => match platform::icon::extract(&src, size) {
                Ok(icon) => match platform::icon::write_preview_png(&icon, std::path::Path::new(&out))
                {
                    Ok(()) => println!(
                        "\n预览图已导出: {out}  (来源 {}，{}×{})",
                        src.display(),
                        icon.width,
                        icon.height
                    ),
                    Err(e) => println!("\n预览图导出失败: {e}"),
                },
                Err(e) => println!("\n预览图来源提取失败: {e}"),
            },
            None => println!("\n没有可用的预览来源"),
        }
    }

    if bad_count == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// 在开始菜单与桌面里找第一个 `.lnk`。
///
/// 只需要一个样本，所以广度优先找到就返回，不做完整扫描。
fn find_a_lnk() -> Option<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        roots.push(
            std::path::PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs"),
        );
    }
    if let Ok(pd) = std::env::var("ProgramData") {
        roots.push(
            std::path::PathBuf::from(pd).join(r"Microsoft\Windows\Start Menu\Programs"),
        );
    }
    if let Ok(up) = std::env::var("USERPROFILE") {
        roots.push(std::path::PathBuf::from(up).join("Desktop"));
    }

    let mut queue = roots;
    while let Some(dir) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                queue.push(p);
            } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
                return Some(p);
            }
        }
    }
    None
}

fn selftest() -> ExitCode {
    let _ = unsafe { windows::Win32::System::Console::SetConsoleOutputCP(65001) };

    println!("图形链路自检（窗口不会显示）");
    println!("{}", "─".repeat(64));

    let baseline = platform::memory::snapshot();
    if let Some(m) = baseline {
        println!(
            "进程基线   : 私有提交 {} / 工作集 {}",
            platform::memory::human(m.private_commit),
            platform::memory::human(m.working_set)
        );
    }

    let mut app = match app::App::new() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("初始化失败: {e}");
            return ExitCode::FAILURE;
        }
    };

    let after_init = platform::memory::snapshot();
    if let (Some(b), Some(a)) = (baseline, after_init) {
        println!(
            "建窗口+设备: 私有提交 {}  （本阶段 +{}，主要是 D3D11/D2D 设备与驱动分配）",
            platform::memory::human(a.private_commit),
            platform::memory::human(a.private_commit.saturating_sub(b.private_commit))
        );
        println!(
            "            工作集 {}（含共享 DLL，仅供参考）",
            platform::memory::human(a.working_set)
        );
    }

    // 配置里请求的材质若不被系统支持，App::new 会另外打印一行降级说明。
    let effective = app.backdrop();
    if app.outer_glass_enabled() {
        println!("外圈玻璃   : 开；实际材质 {effective:?}");
        if effective == config::theme::Backdrop::None {
            println!("             ⚠ 模糊未生效，面板会退化为纯半透明（系统不支持所选项）");
        }
    } else {
        // 这是用户主动关闭的，不是失败
        println!("外圈玻璃   : 关（设置面板里关掉的）；面板按实心不透明绘制");
    }

    match app.draw_once() {
        Ok(()) => println!("绘制一帧   : 成功（BeginDraw → 面板/描边/发光/侧栏/网格 → EndDraw → Present）"),
        Err(e) => {
            eprintln!("绘制失败: {e}");
            return ExitCode::FAILURE;
        }
    }

    println!(
        "后台扫描   : 已就绪，共 {} 条；选项卡「{}」显示 {} 条",
        app.scanned_total(),
        app.active_tab_key(),
        app.visible_count()
    );

    // 验证尺寸变化：交换链与布局必须跟着窗口一起变。
    //
    // 这是对"拖边框后内容被裁切/空白"的直接复现：
    // 若 ResizeBuffers 失败，渲染器尺寸会停在旧值，
    // 右对齐的元素（标题栏按钮）就会被画到窗口外面。
    for (w, h, label) in [(520, 700, "缩小"), (780, 820, "放大"), (640, 880, "复原")] {
        match app.selftest_resize(w, h) {
            Ok((dw, dh, pw, ph)) => {
                let (cw, ch) = (w as f32, h as f32);
                let ok = (dw - cw).abs() <= 2.0 && (dh - ch).abs() <= 2.0;
                println!(
                    "改尺寸({label}) : 窗口 {cw:.0}×{ch:.0} → 渲染器 {dw:.0}×{dh:.0}，面板 {pw:.0}×{ph:.0}{}",
                    if ok { "" } else { "   ⚠ 渲染器未跟上窗口" }
                );
                if !ok {
                    println!("             ⚠ 交换链没有按新尺寸重建，内容会被裁切或留白");
                }
            }
            Err(e) => {
                eprintln!("尺寸变更验证失败({label}): {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    // 验证滚动映射：把视口拉到底，确认能滚到列表末尾
    match app.selftest_scroll_to_end() {
        Ok((first, end, max_scroll, total, tab_key)) => {
            println!(
                "滚动到末尾 : 选项卡「{}」可见第 {}~{} 条（共 {} 条），总可滚 {:.0} DIP",
                tab_key,
                first + 1,
                end,
                total,
                max_scroll
            );
            if total > 0 && end < total {
                println!("             ⚠ 末尾未完全可见，滚动计算可能有误");
            }
        }
        Err(e) => {
            eprintln!("滚动验证失败: {e}");
            return ExitCode::FAILURE;
        }
    }

    // 验证设置面板绘制 + "外圈玻璃"开关是否真的接通了系统层
    match app.selftest_settings() {
        Ok(()) => {
            let ((blur_a, inset_a, rad_a), (blur_b, inset_b, rad_b)) = app.selftest_toggle_glass();
            let works = blur_a != blur_b;
            println!(
                "设置面板   : 绘制成功；玻璃开关{}",
                if works { "已接通系统层" } else { "⚠ 未生效" }
            );
            println!(
                "             模糊 {} / 内缩 {:.0}px / 圆角 {:.0}px   →   模糊 {} / 内缩 {:.0}px / 圆角 {:.0}px",
                if blur_a { "开" } else { "关" },
                inset_a,
                rad_a,
                if blur_b { "开" } else { "关" },
                inset_b,
                rad_b,
            );
            // 玻璃关闭时：内缩与圆角都必须为 0，否则窗口边角会残留透明
            for (blur, inset, radius) in [(blur_a, inset_a, rad_a), (blur_b, inset_b, rad_b)] {
                if !blur && (inset > 0.5 || radius > 0.5) {
                    println!("             ⚠ 平面模式下仍有内缩或圆角，窗口外会残留透明");
                }
            }
        }
        Err(e) => {
            eprintln!("设置面板绘制失败: {e}");
            return ExitCode::FAILURE;
        }
    }

    // 验证缩放行为：缩小图标必须让一屏放下更多条目
    {
        let rows = app.selftest_zoom();
        println!("缩放验证   : 图标px → 方格px，列×行，一屏条目数");
        let mut last_items = usize::MAX;
        let mut monotonic = true;
        for (icon, side, cols, r, items) in &rows {
            println!("              {icon:>3} → {side:>3}，{cols}×{r}，{items} 条");
            // 档位从小到大遍历，所以"一屏条目数"必须**单调不增** ——
            // 图标变大，一屏能放的只会变少。
            // 之前写成"不能下降"，把正确行为当成了失效。
            if *items > last_items {
                monotonic = false;
            }
            last_items = *items;
        }
        if !monotonic {
            println!("             ⚠ 一屏条目数没有随图标变大而减少，布局算错了");
        }
    }

    // 验证图标链路：请求 → 后台提取 → 转 D2D 位图 → 进缓存。
    // 放在内存采样之前，这样图标的内存占用会被算进"绘制后"那一段。
    {
        let (count, used, budget, evictions, inflight, dims) = app.selftest_icons();
        println!(
            "图标缓存   : {} 张位图，占用 {:.1} MB / 预算 {:.0} MB（淘汰 {} 次，仍在途 {}）",
            count,
            used as f64 / (1024.0 * 1024.0),
            budget as f64 / (1024.0 * 1024.0),
            evictions,
            inflight,
        );
        println!(
            "             单张 {:.0} KB，尺寸示例 {}",
            if count > 0 {
                used as f64 / count as f64 / 1024.0
            } else {
                0.0
            },
            dims
        );
        if count == 0 {
            println!("             ⚠ 一张图标都没加载出来，检查加载线程与提取链路");
        } else if used > budget {
            println!("             ⚠ 超出预算，LRU 淘汰没起作用");
        }
    }

    if let (Some(a), Some(d)) = (after_init, platform::memory::snapshot()) {
        println!(
            "绘制后     : 私有提交 {}  （本阶段 +{}）",
            platform::memory::human(d.private_commit),
            platform::memory::human(d.private_commit.saturating_sub(a.private_commit))
        );
    }

    if let Some(m) = platform::memory::snapshot() {
        println!("{}", "─".repeat(64));
        println!(
            "空载总计   : 私有提交 {} / 工作集 {}",
            platform::memory::human(m.private_commit),
            platform::memory::human(m.working_set)
        );
        println!("注意口径   : 任务管理器\"内存\"列是**私有工作集**，本工具尚未计算它");
        println!("             想核对可在程序运行时执行：");
        println!("             Get-Counter \"\\Process(quick-spin)\\Working Set - Private\"");
    }

    ExitCode::SUCCESS
}

/// 按名称关键词启动一个快捷方式。
///
/// 这是没有界面时的启动链路验证入口，尤其用来验证"控制面板""运行"这类
/// shell 命名空间项 —— 它们没有文件系统目标，必须把 `.lnk` 本身交给 shell 才能启动。
fn open_by_name(query: Option<String>) -> ExitCode {
    let Some(query) = query else {
        eprintln!("用法: quick-spin --open <名称关键词>");
        return ExitCode::FAILURE;
    };

    let cfg_store = match config::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("加载配置失败: {e}");
            return ExitCode::FAILURE;
        }
    };

    let report = match shell::scan::scan(&cfg_store.get().sources) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("扫描失败: {e}");
            return ExitCode::FAILURE;
        }
    };

    let needle = query.to_lowercase();
    let Some(hit) = report
        .items
        .iter()
        .find(|s| s.name.to_lowercase().contains(&needle))
    else {
        eprintln!("没有找到名称包含「{query}」的快捷方式（可用 --diagnose 查看清单）");
        return ExitCode::FAILURE;
    };

    let kind = if hit.is_shell_item() {
        "shell 命名空间项"
    } else {
        "普通快捷方式"
    };
    println!("启动: {}  [{}]", hit.name, kind);
    println!("来源: {}", hit.lnk_path.display());
    if !hit.is_shell_item() {
        println!("目标: {}", hit.target.display());
    }

    // 关键：执行 `.lnk` 本身，而不是解析出的目标路径。
    // 控制面板 / UWP / .msc / 需要提权的程序都依赖 shell 才能正确处理。
    match shell::launch::open(&hit.lnk_path) {
        Ok(()) => {
            println!("已交给系统 shell 处理。");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// 自检模式：跑通"读配置 → 解析配色与语言 → 扫描 → 报告"整条链路。
///
/// 之所以先做这个入口，是因为它能在没有界面的情况下验证最容易出错的一环
/// （`.lnk` 解析、来源定位、配色与语言装载），并顺便打印真实耗时数据。
fn diagnose() -> ExitCode {
    // 让中文在传统 conhost 里也能正常显示
    let _ = unsafe { windows::Win32::System::Console::SetConsoleOutputCP(65001) };

    let cfg_store = match config::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("加载配置失败: {e}");
            return ExitCode::FAILURE;
        }
    };
    let state_store = match state::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("加载状态失败: {e}");
            return ExitCode::FAILURE;
        }
    };

    let cfg = cfg_store.get();
    let st = state_store.get();

    // 配色方案 = 内置方案 + 用户自定义（同名覆盖）
    let schemes = SchemeBook::new(&cfg.schemes);
    // **与 `App::new` 保持一致的取值规则**：自定义基础色优先。
    //
    // 不这么做的话，配了 `base_color` 的人会看到 `--diagnose` 报出**另一个**
    // 方案名（命名的那个），排查"颜色为什么不对"时会被它带偏 ——
    // 实测就踩过一次：以为主题是深蓝，实际渲染的是基础色派生出来的橄榄绿。
    let scheme = match config::Color::from_hex(&cfg.appearance.base_color) {
        Some(base) => config::Scheme::from_base(base),
        None => schemes.resolve(&cfg.appearance.scheme).clone(),
    };

    // 语言：配置留空表示跟随系统
    let lang_dir = paths::lang_dir().ok();
    let i18n = I18n::load(&cfg.language.locale, lang_dir.as_deref());

    println!("{}", i18n.t("app.title"));
    println!("{}", "─".repeat(64));
    println!("配置文件   : {}", cfg_store.path().display());
    println!("状态文件   : {}", state_store.path().display());
    println!(
        "记住的窗口 : {}×{}   位置 {:?}   最大化 {}",
        st.window.width, st.window.height, st.window.x.map(|x| (x, st.window.y)), st.window.maximized
    );
    println!(
        "界面语言   : {}（可选 {} 种）",
        i18n.locale(),
        I18n::available().len()
    );
    println!(
        "外观       : 透明度 {:.2} / 圆角 {:.0} / 背景 {:?}",
        cfg.appearance.opacity, cfg.appearance.corner_radius, cfg.appearance.backdrop
    );

    println!();
    println!("配色方案   : {} [{}]", scheme.name, cfg.appearance.scheme);
    println!(
        "  面板渐变 : 0x{:08X} → 0x{:08X}  角度 {:.0}°",
        scheme.panel.from.0, scheme.panel.to.0, scheme.panel.angle
    );
    println!(
        "  外描边   : 0x{:08X} → 0x{:08X}  发光 {:.2}",
        scheme.border.from.0, scheme.border.to.0, scheme.glow_strength
    );
    println!(
        "  亚克力色 : 0x{:08X}（供系统模糊层着色）",
        scheme.acrylic_tint(cfg.appearance.opacity).0
    );
    // 派生出来的交互态颜色也一并打出来。
    //
    // 它们是"看着没问题、实际全透明"这类故障的唯一线索：悬停/选中是绘制时
    // 现取这几个字段的，只要派生没跑过，界面上就是**什么都不显示**，
    // 而且不报任何错。打出来一眼就能确认。
    println!(
        "  交互态   : 悬停 0x{:08X} / 选项卡悬停 0x{:08X} / 选中 0x{:08X} / 强调整色 0x{:08X}",
        scheme.tile_hover.0,
        scheme.tab_hover.0,
        scheme.tile_active.0,
        scheme.accent_soft.0
    );
    println!("  可选方案 :");
    for (key, name) in schemes.listing() {
        let mark = if key == cfg.appearance.scheme { "●" } else { "○" };
        println!("    {mark} {key:<14} {name}");
    }

    println!();
    println!("{}", i18n.t("msg.scanning"));
    let report = match shell::scan::scan(&cfg.sources) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("扫描失败: {e}");
            return ExitCode::FAILURE;
        }
    };

    let total = report.items.len() + report.skipped + report.broken;
    let per_item = if total > 0 {
        report.elapsed.as_secs_f64() * 1000.0 / total as f64
    } else {
        0.0
    };

    println!(
        "扫描完成   : 可用 {} 条，目标失效 {} 条，解析失败 {} 条，耗时 {:.1} ms（平均 {:.2} ms/条）",
        report.items.len(),
        report.broken,
        report.skipped,
        report.elapsed.as_secs_f64() * 1000.0,
        per_item
    );
    println!(
        "其中 shell 命名空间项 {} 条（如控制面板、运行 —— 无文件系统目标，靠 shell 启动）",
        report.shell_items()
    );

    if !report.failures.is_empty() {
        println!();
        println!("跳过的条目（最多显示 6 条）:");
        for (p, why) in report.failures.iter().take(6) {
            println!("  - {} : {}", p.display(), why);
        }
        if report.failures.len() > 6 {
            println!("  ... 其余 {} 条省略", report.failures.len() - 6);
        }
    }

    if !report.items.is_empty() {
        println!();
        println!("快捷方式（最多显示 6 条）:");
        for (i, sc) in report.items.iter().take(6).enumerate() {
            // shell 命名空间项没有目标，直接显示它的来源文件更有意义
            let dest = if sc.is_shell_item() {
                "(shell 命名空间项)".to_string()
            } else {
                sc.target.display().to_string()
            };
            println!("  {:>3}. {:<24} -> {}", i + 1, truncate(&sc.name, 22), dest);
        }
        if report.items.len() > 6 {
            println!("  ... 其余 {} 条省略", report.items.len() - 6);
        }
    }

    ExitCode::SUCCESS
}

/// 按字符数截断，避免中英文混排把输出撑歪。
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}
