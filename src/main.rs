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
mod diag;
mod error;
mod i18n;
mod model;
mod paths;
mod platform;
mod popout;
mod render;
mod shell;
mod state;
mod ui;

use std::process::ExitCode;

use config::SchemeBook;
use i18n::I18n;

fn main() -> ExitCode {
    // **最早装 panic 钩子。**
    //
    // 放在 `prepare_console` 之前：从这里往后每一行都可能出问题（参数、控制台、
    // 单实例、配置），而崩溃一旦发生，没有钩子就什么都不留 —— 这正是
    // 2026-10-18 那次崩溃查不到任何记录的原因（见 `diag::install_panic_hook`）。
    // 命令行模式（`--selftest` 等）走的也是这个 `main`，一并覆盖。
    diag::install_panic_hook();

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
        ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE,
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
        STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
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
                    Ok(false) => {
                        // 这也是一种"正常结束"，而且**不是崩溃**：记一行，
                        // 免得下次看到日志在这里断掉、误判成"启动时就崩了"。
                        diag::log("已有实例在运行，已把它叫到前面，本次启动结束");
                        return ExitCode::SUCCESS;
                    }
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
        Ok(()) => {
            // **这一行是"正常退出"的凭据。**
            //
            // 排查"程序突然没了"时先看日志最后一行：
            // 有它 = 消息循环正常结束；没有 = 崩了（或被强杀）。
            // 配合 `WM_DESTROY` 那行（主窗口销毁 / 弹窗销毁）就能定位到哪一步。
            diag::log("程序正常退出（消息循环结束）");
            ExitCode::SUCCESS
        }
        Err(e) => {
            diag::log(&format!("启动失败，程序退出: {e}"));
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
                Ok(icon) => {
                    match platform::icon::write_preview_png(&icon, std::path::Path::new(&out)) {
                        Ok(()) => println!(
                            "\n预览图已导出: {out}  (来源 {}，{}×{})",
                            src.display(),
                            icon.width,
                            icon.height
                        ),
                        Err(e) => println!("\n预览图导出失败: {e}"),
                    }
                }
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
        roots
            .push(std::path::PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(pd) = std::env::var("ProgramData") {
        roots.push(std::path::PathBuf::from(pd).join(r"Microsoft\Windows\Start Menu\Programs"));
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

/// 自检输出必须走这个宏：它顺手登记"这一行是不是失败"（含 `⚠` 即失败）。
///
/// 这个口子是补出来的。原先自检的退出码**恒为 `SUCCESS`**，一次反向验证证明
/// "故意改坏 → 报出两条失败 → 退出码仍然是 0" —— 也就是说，凡是以退出码为判据
/// 的人（脚本、CI、以及后来的我）都会把红的看成绿的。判据有两个来源必然会脱节，
/// 所以让它只有一个来源：**输出由谁产生，成败就由谁登记**。
static SELFTEST_WARNINGS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

macro_rules! selftest_line {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        if line.contains('⚠') {
            SELFTEST_WARNINGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        println!("{line}");
    }};
}
fn selftest() -> ExitCode {
    let _ = unsafe { windows::Win32::System::Console::SetConsoleOutputCP(65001) };

    selftest_line!("图形链路自检（窗口不会显示）");
    selftest_line!("{}", "─".repeat(64));

    let baseline = platform::memory::snapshot();
    if let Some(m) = baseline {
        selftest_line!(
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
        selftest_line!(
            "建窗口+设备: 私有提交 {}  （本阶段 +{}，主要是 D3D11/D2D 设备与驱动分配）",
            platform::memory::human(a.private_commit),
            platform::memory::human(a.private_commit.saturating_sub(b.private_commit))
        );
        selftest_line!(
            "            工作集 {}（含共享 DLL，仅供参考）",
            platform::memory::human(a.working_set)
        );
    }

    // 配置里请求的材质若不被系统支持，App::new 会另外打印一行降级说明。
    let effective = app.backdrop();
    if app.outer_glass_enabled() {
        selftest_line!("外圈玻璃   : 开；实际材质 {effective:?}");
        if effective == config::theme::Backdrop::None {
            selftest_line!("             ⚠ 模糊未生效，面板会退化为纯半透明（系统不支持所选项）");
        }
    } else {
        // 这是用户主动关闭的，不是失败
        selftest_line!("外圈玻璃   : 关（设置面板里关掉的）；面板按实心不透明绘制");
    }

    match app.draw_once() {
        Ok(()) => selftest_line!(
            "绘制一帧   : 成功（BeginDraw → 面板/描边/发光/侧栏/网格 → EndDraw → Present）"
        ),
        Err(e) => {
            eprintln!("绘制失败: {e}");
            return ExitCode::FAILURE;
        }
    }

    // 帧耗时：直接量"连续重绘能不能跟得上鼠标"。
    //
    // 拖动（框选、拖窗口尺寸）期间是**每一帧都在重绘**的，所以帧耗时
    // 就是手感的上限：超过一个刷新周期（60Hz ≈ 16.7ms）就会开始积压，
    // 表现为选框滞后于光标。
    {
        let (avg_raw, worst_raw) = app.selftest_frame_bench(40, true);
        let (avg_vs, worst_vs) = app.selftest_frame_bench(40, false);
        selftest_line!(
            "帧耗时     : 不等同步 平均 {:>5.1}ms / 最慢 {:>5.1}ms  → 上限 {:>3.0} fps",
            avg_raw,
            worst_raw,
            1000.0 / avg_raw.max(0.01)
        );
        selftest_line!(
            "             等同步   平均 {:>5.1}ms / 最慢 {:>5.1}ms  → 上限 {:>3.0} fps",
            avg_vs,
            worst_vs,
            1000.0 / avg_vs.max(0.01)
        );

        // 图标是否被"每帧重复请求"。正常情况下应当是 0：
        // 每个图标只请求一次，落地后进缓存就不再请求。
        // 非 0 且随帧数增长 ⇒ 有图标永远提取不成功，而每帧都在重试，
        // 加载线程会一直做白工（拖动时的卡顿就是这么来的）。
        let re = app.selftest_icon_rerequests(10, 30);
        selftest_line!("图标请求   : 30 帧内发起 {re} 次（含首次请求，不代表异常）");

        // 真正的判据：有多少图标**试满次数后放弃**了。
        // 那才是"某个方格永远没有图标"，需要定位。
        let (failing, blocked) = app.selftest_icon_failures();
        selftest_line!(
            "图标失败   : 待重试 {failing} 个 / 已放弃 {blocked} 个{}",
            if blocked == 0 {
                "（正常：没有永远提取不成功的图标）"
            } else {
                "  ⚠ 有方格会一直是空的！看 诊断.log 里的『图标提取失败』行"
            }
        );

        // 框选拖动时"每次移动是否都请求了重绘"。这是选框流畅度的直接判据。
        //
        // 期望 `asked` 与 `steps` 基本相等：前几步位移还在 `MARQUEE_MIN`（3px）
        // 阈值内，那时选框还没开始画，不请求重绘是对的，所以允许差一两次。
        let (steps, asked) = app.selftest_marquee_redraws(60);
        selftest_line!(
            "框选重绘率 : {asked}/{steps} 次移动请求了重绘{}",
            if steps == 0 {
                "（跳过：内容区左下角被方格占住了）"
            } else if asked * 10 >= steps * 9 {
                "（正常：选框会跟着鼠标连续移动）"
            } else {
                "  ⚠ 多数移动没请求重绘，选框会一格一跳"
            }
        );
    }

    // 弹出子窗口的状态模型。纯数据，没有窗口也能完整验证。
    //
    // 这一步在"窗口层还没改造成多窗口"之前就能做，正是把它单独成模块的好处：
    // 取值规则（默认值、夹紧、互斥、排序）先测死，后面接窗口时就不必怀疑数据。
    {
        use popout::state::{MAX_H, MIN_W, PopoutState, PopoutWindow};

        let mut failures: Vec<&str> = Vec::new();

        // 默认值：默认吸附在主窗口右侧、不钉桌面、尺寸 320×420
        let d = PopoutWindow::default();
        if !(d.docked && !d.pinned && !d.open && d.width == 320 && d.height == 420) {
            failures.push("默认值不对");
        }

        // 夹紧：荒谬尺寸要收进合法区间
        let mut rough = PopoutWindow::default();
        rough.width = 1;
        rough.height = 99_999;
        rough.normalize();
        if rough.width != MIN_W || rough.height != MAX_H {
            failures.push("尺寸未夹紧");
        }

        // `pinned` 与 `docked` 是**正交**的两件事（可见性 vs 位置），
        // 四种组合都必须原样保留。
        //
        // 这里原本测的是"pinned 压过 docked"——那条互斥的依据是
        // "钉 = 常驻桌面层"，而使用者已把"钉"收窄成"主窗口收起时不跟着隐藏"，
        // 于是互斥不再成立。留着它的后果不是显示问题，而是**静默改写用户的
        // 记录**：手写 `pinned = true, docked = true` 会被 `normalize` 抹成
        // 不吸附，表现是"我明明没拖它，怎么就不吸附了"。
        let mut lost = 0;
        for pin in [false, true] {
            for dock in [false, true] {
                let mut w = PopoutWindow::default();
                w.pinned = pin;
                w.docked = dock;
                w.normalize();
                if w.pinned != pin || w.docked != dock {
                    lost += 1;
                }
            }
        }
        if lost > 0 {
            failures.push("钉住×吸附 的四种组合未被原样保留");
        }

        // get 不能写入（否则 state.toml 会随访问长大）
        let mut ps = PopoutState::default();
        let _ = ps.get("不存在的");
        if !ps.is_empty() {
            failures.push("get 写入了记录");
        }

        // to_open 只取开着的，且按 key 排序（多个窗口的创建顺序必须稳定）
        for (k, open) in [("tab3", false), ("tab1", true), ("tab2", true)] {
            let mut w = PopoutWindow::default();
            w.open = open;
            ps.set(k, w);
        }
        let opened: Vec<&str> = ps.to_open().iter().map(|(k, _)| *k).collect();
        if opened != vec!["tab1", "tab2"] {
            failures.push("to_open 顺序或筛选不对");
        }

        // retain_tabs：选项卡被删掉后，它的窗口状态必须能被清掉
        let live: std::collections::HashSet<String> = ["tab1".to_string()].into_iter().collect();
        let removed = ps.retain_tabs(&live);
        if removed != 2 || ps.windows.len() != 1 {
            failures.push("retain_tabs 清理数不对");
        }

        // 标题回退：没自命名时用选项卡名，自命名后用自己的
        let unnamed = PopoutWindow::default();
        let mut named = PopoutWindow::default();
        named.name = Some("我的面板".to_string());
        if unnamed.title("主窗口") != "主窗口" || named.title("主窗口") != "我的面板" {
            failures.push("标题回退不对");
        }

        selftest_line!(
            "弹出窗状态 : {}",
            if failures.is_empty() {
                "默认值 / 尺寸夹紧 / 钉住与吸附正交 / get 不写入 / to_open 排序 / retain_tabs / 标题回退 —— 全部通过".to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("、"))
            }
        );
    }

    // 吸附跟随的几何。纯算术，能离线把"该贴哪边、放不下怎么翻、主窗口移动后
    // 位置跟不跟得上、没有显示器信息时怎么办"逐项测死。
    //
    // 而"弹窗**真的**跟着动了没有"只能人工验 —— 自检建不出第二个窗口
    //（前几轮的悬垂指针崩溃、漏 show()、"一个字都不显示"都是这么漏过去的）。
    {
        use platform::window::Rect;
        use popout::dock::{DockRequest, dock_position};
        use popout::state::DockSide;

        let work = Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let main = Rect {
            x: 100,
            y: 100,
            w: 400,
            h: 800,
        };
        let size = (320, 420);
        let mut failures: Vec<String> = Vec::new();
        let pos = |m: Rect, side: DockSide, w: Option<Rect>| {
            dock_position(&DockRequest {
                main: m,
                size,
                side,
                work: w,
                // 这一块测的是"单窗口"的规则（空链、没有当前位置），
                // 于是它的期望值与加堆叠之前完全一致 —— 正好当回归基线用。
                current: None,
                occupied: &[],
            })
        };

        // 右吸附：弹窗左边缘贴主窗口右边缘（100+400=500），顶部对齐（100）
        match pos(main, DockSide::Right, Some(work)) {
            Some(d) if (d.x, d.y, d.side) == (500, 100, DockSide::Right) => {}
            other => failures.push(format!("右吸附应为 (500,100,Right)，实为 {other:?}")),
        }

        // 右侧放不下 → 翻到左侧：主窗口右边缘 1700+400=2100，2100+320 越界
        let at_right = Rect {
            x: 1700,
            y: 100,
            w: 400,
            h: 800,
        };
        match pos(at_right, DockSide::Right, Some(work)) {
            Some(d) if (d.x, d.y, d.side) == (1380, 100, DockSide::Left) => {}
            other => failures.push(format!(
                "右侧放不下应翻到左侧 (1380,100,Left)，实为 {other:?}"
            )),
        }

        // 左侧放不下 → 翻回右侧（期望左侧，但左边出界）
        let at_left = Rect {
            x: 10,
            y: 100,
            w: 400,
            h: 800,
        };
        match pos(at_left, DockSide::Left, Some(work)) {
            Some(d) if (d.x, d.side) == (410, DockSide::Right) => {}
            other => failures.push(format!("左侧放不下应翻回右侧 (410,Right)，实为 {other:?}")),
        }

        // 两侧都放不下：保持**期望的侧**，但必须夹进工作区 ——
        // 宁可和主窗口重叠，也不能把窗口推出屏幕（工作区 1000 宽、主窗口占 900）
        let narrow = Rect {
            x: 0,
            y: 0,
            w: 1000,
            h: 1080,
        };
        let wide_main = Rect {
            x: 0,
            y: 0,
            w: 900,
            h: 600,
        };
        match pos(wide_main, DockSide::Right, Some(narrow)) {
            Some(d) if (d.x, d.side) == (680, DockSide::Right) => {}
            other => failures.push(format!("两侧都放不下应夹到 680/Right，实为 {other:?}")),
        }

        // 垂直夹紧：主窗口贴到工作区下边时，整窗仍要可见（1080-420=660）
        let low = Rect {
            x: 100,
            y: 900,
            w: 400,
            h: 800,
        };
        match pos(low, DockSide::Right, Some(work)) {
            Some(d) if d.y == 660 => {}
            other => failures.push(format!("垂直应夹到 y=660，实为 {other:?}")),
        }

        // 主窗口移动 → 位置跟着平移 200（"跟随"这件事的最小证据）
        let moved = Rect {
            x: 300,
            y: 100,
            w: 400,
            h: 800,
        };
        match (
            pos(main, DockSide::Right, Some(work)),
            pos(moved, DockSide::Right, Some(work)),
        ) {
            (Some(a), Some(b)) if b.x == a.x + 200 => {}
            other => failures.push(format!("主窗口右移 200 后弹窗未跟着平移: {other:?}")),
        }

        // 弹窗比工作区还大：**绝不能 panic**。
        // 标准库的 `clamp` 在 min>max 时会崩，而这里是能真实发生的
        //（用户把窗口拉得比屏幕还大 / 工作区被竖排任务栏切窄）。
        let tiny = Rect {
            x: 10,
            y: 20,
            w: 200,
            h: 200,
        };
        let small_main = Rect {
            x: 0,
            y: 0,
            w: 50,
            h: 50,
        };
        match pos(small_main, DockSide::Right, Some(tiny)) {
            Some(d) if (d.x, d.y) == (10, 20) => {}
            other => failures.push(format!("超宽窗口应退化成贴左上角 (10,20)，实为 {other:?}")),
        }

        // 拿不到工作区 → 不给位置（调用方保持原位），而不是猜一个坐标
        if pos(main, DockSide::Right, None).is_some() {
            failures.push("没有显示器工作区时仍给出了位置".to_string());
        }

        selftest_line!(
            "吸附跟随   : {}",
            if failures.is_empty() {
                "右吸附 / 右放不下翻左 / 左放不下翻右 / 两侧都放不下夹紧 / 垂直夹紧 / 移动跟随 / 超宽不 panic / 无工作区不摆 —— 全部通过（逐项比对期望值）".to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // 吸附**堆叠**：多个弹窗一起吸附时不能重叠。
    //
    // 使用者实测报障的正是这一条："多个窗口一起吸附会重叠在一起" —— 那时
    // `dock_position` 对每个窗口都算同一个位置（贴主窗口右边缘、上边对齐），
    // 计算里没有"那一侧已经站了别人"这个概念。所以这一块的第一条断言就是
    // **两个窗口的坐标必须不同**，那是那个 bug 的直接反例。
    //
    // 仍然是纯几何：喂进"已占用的矩形"，比较"算出来的落位"。
    {
        use platform::window::Rect;
        use popout::dock::{DockRequest, dock_position};
        use popout::state::DockSide;

        let work = Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let size = (320, 420);
        let mut failures: Vec<String> = Vec::new();

        // 右吸附链上第 n 个的期望坐标：x 恒为 500（= 100+400），y 依次相接。
        let hi = |x: i32, y: i32| Rect {
            x,
            y,
            w: size.0,
            h: size.1,
        };

        let place = |main: Rect, side: DockSide, occupied: &[Rect]| {
            dock_position(&DockRequest {
                main,
                size,
                side,
                work: Some(work),
                // 用期望侧（不靠"当前中心"推断），这样每条断言只考一件事。
                current: None,
                occupied,
            })
        };

        // ---- 1. 空链时与加堆叠之前完全一致（回归基线）----
        let main = Rect {
            x: 100,
            y: 100,
            w: 400,
            h: 800,
        };
        match place(main, DockSide::Right, &[]) {
            Some(d) if (d.x, d.y) == (500, 100) => {}
            other => failures.push(format!("空链时应为 (500,100)，实为 {other:?}")),
        }

        // ---- 2. 两个窗口：坐标必须不同，且首尾相接 ----
        let first = hi(500, 100);
        match place(main, DockSide::Right, &[first]) {
            Some(d) if (d.x, d.y) == (500, 520) => {}
            // 520 = 100 + 420（上一个的下边缘）
            other => failures.push(format!("第二个应接在 (500,520)，实为 {other:?}")),
        }

        // ---- 3. 三个窗口：依次向下相接 ----
        let second = hi(500, 520);
        match place(main, DockSide::Right, &[first, second]) {
            // 100 + 420*2 = 940，而 940+420=1360 > 1080 → 这一列排满
            Some(d) if (d.x, d.y) == (820, 100) => {}
            // 820 = 500 + 320：另起一列，贴在第一列外侧，再从主窗口上边缘开始
            other => failures.push(format!("第一列排满应另起一列 (820,100)，实为 {other:?}")),
        }

        // ---- 4. 回填空位：把链上中间那个移走，后来的应补进去 ----
        match place(main, DockSide::Right, &[first, hi(820, 100)]) {
            Some(d) if (d.x, d.y) == (500, 520) => {}
            other => failures.push(format!("应回填空位 (500,520)，实为 {other:?}")),
        }

        // ---- 5. 左吸附是镜像的 ----
        let main_left = Rect {
            x: 900,
            y: 100,
            w: 400,
            h: 800,
        };
        let l_first = hi(580, 100); // 900 - 320 = 580
        match place(main_left, DockSide::Left, &[l_first]) {
            Some(d) if (d.x, d.y) == (580, 520) => {}
            other => failures.push(format!("左吸附第二个应为 (580,520)，实为 {other:?}")),
        }

        // ---- 6. 宽窄不一的窗口 ----
        // 链上有一个 500 宽（比自己宽）、且占满整列的窗口。自己的第一个槽位
        // 被它挡住，往下也放不下 → 只能落到它**外侧**：列间距取了两者较宽的
        // 那个，所以 x 不会压进它。
        let wide = Rect {
            x: 500,
            y: 100,
            w: 500,
            h: 800,
        };
        match place(main, DockSide::Right, &[wide]) {
            Some(d) if d.x >= wide.right() => {}
            other => failures.push(format!("整列被宽窗口占满时应落到它外侧，实为 {other:?}")),
        }
        // 同一个宽窗口只占上半段时，应当接在它**下方**（而不是另起一列）
        let wide_top = Rect {
            x: 500,
            y: 100,
            w: 500,
            h: 400,
        };
        match place(main, DockSide::Right, &[wide_top]) {
            Some(d) if (d.x, d.y) == (500, 500) => {}
            other => failures.push(format!("应接在宽窗口下方 (500,500)，实为 {other:?}")),
        }

        // ---- 7. 排不下时的退化：不越界、不 panic ----
        // 主窗口比工作区还宽 → 两侧都放不下 → 保持侧别并夹进工作区。
        let huge_main = Rect {
            x: -500,
            y: 100,
            w: 3000,
            h: 800,
        };
        match place(huge_main, DockSide::Right, &[]) {
            Some(d)
                if d.x >= work.x
                    && d.x + size.0 <= work.right()
                    && d.y >= work.y
                    && d.y + size.1 <= work.bottom() => {}
            other => failures.push(format!("排不下时应夹进工作区，实为 {other:?}")),
        }
        // 工作区被切得比窗口还矮（竖排任务栏 / 极窄屏）：同样不许 panic
        let tiny_work = Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 100,
        };
        if dock_position(&DockRequest {
            main,
            size,
            side: DockSide::Right,
            work: Some(tiny_work),
            current: None,
            occupied: &[],
        })
        .is_none()
        {
            failures.push("工作区比窗口还矮时不该没有结果".to_string());
        }

        selftest_line!(
            "吸附堆叠   : {}",
            if failures.is_empty() {
                "两个不相撞且相接 / 三个依次相接 / 一列排满另起一列 / 回填空位 / 左吸附镜像 / 宽窄不一不压邻列 / 排不下仍夹进工作区且不 panic —— 全部通过（逐项比对期望值）".to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // 色板与"用哪套配色"。**纯数据**，没有窗口也能全测 —— 而这几条判据
    // 出错的表现都在另一个窗口里（"点了色没反应"、"选了色重启就没了"），
    // 靠人眼撞是碰运气。
    {
        use config::theme::{BASE_PALETTE, palette_colors, palette_index};
        use platform::hittest::RectI;
        use popout::palette::{PaletteLayout, follows_main, resolve_scheme};

        let mut failures: Vec<String> = Vec::new();

        // ---- 色板：唯一出处，且每一项都解析得出来 ----
        //
        // 少解析出一项就会让"格子数与颜色列表错位"，表现是点 A 选中 B ——
        // 而 `filter_map` 会**静默**跳过坏项，所以必须在这里钉住数量。
        let colors = palette_colors();
        if colors.len() != BASE_PALETTE.len() {
            failures.push(format!(
                "色板有 {} 项却没解析出同样多的颜色（{} 项）—— 有项解析失败了",
                BASE_PALETTE.len(),
                colors.len()
            ));
        }
        // 每一项都能被 `palette_index` 找回自己（往返一致）
        for (i, hex) in BASE_PALETTE.iter().enumerate() {
            if palette_index(hex) != Some(i) {
                failures.push(format!("色板第 {i} 项 {hex} 找不到自己"));
            }
        }
        // 空串与不在色板里的颜色都没有对应格子：不该"硬找最近似的"高亮一格
        if palette_index("").is_some() {
            failures.push("空的基础色不该高亮任何色块".to_string());
        }
        if palette_index("#010203").is_some() {
            failures.push("不在色板里的自定义色不该高亮任何色块".to_string());
        }
        // 带空白的写法要能容忍（状态文件是手改得到的）
        if palette_index("  #0A1A2F  ") != Some(0) {
            failures.push("带空白的基础色应当照常识别".to_string());
        }
        // 下标 → 文本 → 下标 必须往返一致：选色时走"格子下标 → 写进状态的文本"，
        // 高亮时走"状态里的文本 → 格子下标"。两步用不同的表（一个过滤、一个不
        // 过滤）就会错位 —— 表现是"点了 A 格，高亮的是 B 格"。
        let mut hexes: Vec<&str> = Vec::new();
        for i in 0..colors.len() {
            match config::theme::palette_hex(i) {
                Some(h) if palette_index(h) == Some(i) => hexes.push(h),
                Some(h) => failures.push(format!("第 {i} 格取到的 {h} 反而指回别处")),
                None => failures.push(format!("第 {i} 格取不到十六进制文本")),
            }
        }
        if hexes.len() == BASE_PALETTE.len() && hexes != BASE_PALETTE.to_vec() {
            failures.push("下标取到的文本顺序与色板常量不一致".to_string());
        }
        if config::theme::palette_hex(colors.len()).is_some() {
            failures.push("越界的下标不该取到文本".to_string());
        }

        // ---- "用哪套配色"的规则 ----
        let main = config::Scheme::from_base(config::Color::from_hex("#159AA4").unwrap());
        if resolve_scheme("", &main) != main {
            failures.push("空串应当**跟随主窗口**，实为另一套".to_string());
        }
        if !follows_main("") || !follows_main("   ") || follows_main("#0A1A2F") {
            failures.push("follows_main 判据不对".to_string());
        }
        let own = resolve_scheme("#0A1A2F", &main);
        if own == main {
            failures.push("自己设了颜色却仍旧用主窗口那套".to_string());
        }
        // 派生是确定性的：同一个基础色两次必须完全一致（否则每次重算都"变了"，
        // 会变成每帧请求重绘）
        if config::Scheme::from_base(config::Color::from_hex("#0A1A2F").unwrap())
            != config::Scheme::from_base(config::Color::from_hex("#0A1A2F").unwrap())
        {
            failures.push("同一个基础色派生出的两套配色不相等".to_string());
        }
        // 坏值回落到主窗口那套，而不是没有配色
        if resolve_scheme("不是颜色", &main) != main {
            failures.push("非法基础色应当回落到主窗口配色".to_string());
        }

        // ---- 选色浮层的几何与命中 ----
        let area = RectI::new(10, 48, 300, 372);
        let pl = PaletteLayout::compute(area);
        if pl.swatches.len() != colors.len() {
            failures.push(format!(
                "色块数 {} 与颜色数 {} 不一致（绘制与命中的下标会错位）",
                pl.swatches.len(),
                colors.len()
            ));
        }
        // 默认尺寸下浮层要完整落在内容区里，且不压标题栏
        if pl.panel.x < area.x
            || pl.panel.y < area.y
            || pl.panel.x + pl.panel.w > area.x + area.w
            || pl.panel.y + pl.panel.h > area.y + area.h
        {
            failures.push(format!(
                "默认尺寸下浮层越界: 面板 {:?} 内容区 {:?}",
                pl.panel, area
            ));
        }
        // 每个色块都要落在面板里，且点到它的中心能命中它自己
        for (i, r) in pl.swatches.iter().enumerate() {
            if !pl.panel.contains(r.x, r.y) || !pl.panel.contains(r.x + r.w - 1, r.y + r.h - 1) {
                failures.push(format!("第 {i} 个色块画到面板外面了"));
                break;
            }
            if pl.swatch_at(r.x + r.w / 2, r.y + r.h / 2) != Some(i) {
                failures.push(format!("点第 {i} 个色块的中心没命中它自己"));
                break;
            }
        }
        // "跟随主窗口"那一行：能命中，且不与任何色块重叠
        if !pl.is_follow(pl.follow.x + pl.follow.w / 2, pl.follow.y + pl.follow.h / 2) {
            failures.push("点\"跟随主窗口\"那一行没命中".to_string());
        }
        for r in pl.swatches.iter() {
            if r.intersects(&pl.follow) {
                failures.push("有颜色格子与\"跟随主窗口\"那一行重叠".to_string());
                break;
            }
        }
        // 点浮层外面 = 关闭：内容区左上角在浮层之外（浮层是居中的）
        if pl.contains(area.x, area.y) {
            failures.push("浮层不该盖住内容区左上角（那里应当是\"点外面关闭\"）".to_string());
        }
        // 极窄/极矮的窗口：不许 panic，色块数也不能少（宁可由绘制层裁掉）
        for (w, h) in [(60, 60), (120, 90), (1, 1), (4000, 4000)] {
            let tiny = PaletteLayout::compute(RectI::new(0, 0, w, h));
            if tiny.swatches.len() != colors.len() {
                failures.push(format!(
                    "窗口 {w}×{h} 时色块数变了（{} 个）",
                    tiny.swatches.len()
                ));
            }
            if tiny.swatches.iter().any(|r| r.w <= 0 || r.h <= 0) {
                failures.push(format!("窗口 {w}×{h} 时出现了零尺寸色块"));
            }
        }

        selftest_line!(
            "弹窗配色   : {}",
            if failures.is_empty() {
                "色板单一出处 / 往返可回查 / 自定义色不高亮 / 跟随主窗口 / 派生确定 / 非法值回落 / 浮层不越界 / 命中一一对应 / 极窄窗口不 panic —— 全部通过（逐项比对期望值）".to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // **配色能不能穿过 `persist_popout` 的合并** —— 使用者点名要钉住的那条。
    //
    // 出错的表现是"选了色、当时生效、重启就没了"：合并时用了记录里的旧值，
    // 把窗口刚选的颜色盖掉。这条只有真的重启一次才会暴露，所以必须离线钉死。
    {
        use popout::state::PopoutWindow;

        let mut failures: Vec<String> = Vec::new();

        // 记录里有旧配色 + 用户起的名字
        let old = PopoutWindow {
            name: Some("用户起的名字".to_string()),
            base_color: "#111111".to_string(),
            width: 300,
            ..Default::default()
        };
        // 窗口此刻：选了新配色（名字它是拿不到的，只能是 None）
        let fresh = PopoutWindow {
            name: None,
            base_color: "#3A8FE0".to_string(),
            width: 420,
            ..Default::default()
        };
        let merged = PopoutWindow::merge_persisted(fresh, &old);

        if merged.base_color != "#3A8FE0" {
            failures.push(format!(
                "配色没穿过合并：期望 #3A8FE0，实为 {}（选了色会被静默丢弃）",
                merged.base_color
            ));
        }
        if merged.name != Some("用户起的名字".to_string()) {
            failures.push("名字被窗口那份默认值覆盖了".to_string());
        }
        if merged.width != 420 {
            failures.push("尺寸没取窗口那边的值".to_string());
        }

        // 反方向同样重要：用户选了"跟随主窗口"（空串），不能被旧配色盖回去
        let fresh_follow = PopoutWindow {
            base_color: String::new(),
            ..Default::default()
        };
        let merged_follow = PopoutWindow::merge_persisted(fresh_follow, &old);
        if !merged_follow.base_color.is_empty() {
            failures.push(format!(
                "选\"跟随主窗口\"被旧配色覆盖了：实为 {}",
                merged_follow.base_color
            ));
        }

        // 没变化时必须**相等**：否则每次落盘都会写一次（写盘风暴）
        let same = PopoutWindow::merge_persisted(old.clone(), &old);
        if same != old {
            failures.push("窗口状态没变时合并结果却与记录不同（会每次落盘）".to_string());
        }

        selftest_line!(
            "弹窗配色落盘: {}",
            if failures.is_empty() {
                "配色穿过合并 / 跟随主窗口也能穿过 / 名字用记录值 / 无变化不写盘 —— 全部通过（逐项比对期望值）".to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // 启动恢复：只有 `open` 的那些要被开出来，且顺序稳定。
    {
        use popout::state::{PopoutState, PopoutWindow};

        let mut st = PopoutState::default();
        st.set(
            "tab3",
            PopoutWindow {
                open: true,
                ..Default::default()
            },
        );
        st.set(
            "main",
            PopoutWindow {
                open: false,
                ..Default::default()
            },
        );
        st.set(
            "tab2",
            PopoutWindow {
                open: true,
                ..Default::default()
            },
        );

        let open: Vec<&str> = st.to_open().into_iter().map(|(k, _)| k).collect();
        let want = vec!["tab2", "tab3"];
        selftest_line!(
            "启动恢复   : {}",
            if open == want {
                "只恢复 open 的窗口 / 按选项卡 key 排序稳定（tab2, tab3）—— 通过".to_string()
            } else {
                format!("⚠ 期望 {want:?}，实为 {open:?}")
            }
        );
    }

    // 主窗口标志：决定关闭主窗口时程序退不退出。    // 错了不会报错，只是关了窗口进程还在 —— 必须单独验。
    selftest_line!(
        "多窗口基础 : 主窗口标志 {}；活着的窗口 {} 个",
        if app.selftest_main_window_flag() {
            "已置位"
        } else {
            "⚠ 未置位（关窗口将不退出）"
        },
        app.selftest_live_window_count()
    );

    // 弹出子窗口的布局几何。纯算术，能离线把"越界/重叠"这类问题测死。
    {
        use popout::layout::{BUTTON_GAP, PopoutLayout, TITLE_BAR, TitleButton};

        let mut failures: Vec<String> = Vec::new();

        // 最小尺寸、典型尺寸、大尺寸都试：布局在不同宽高下的退化行为不同。
        for (w, h) in [(200, 160), (320, 420), (900, 1400), (420, 300)] {
            let l = PopoutLayout::compute(w, h, 48);
            let tag = format!("{w}×{h}");

            // 面板必须覆盖整个客户区（画背景与描边靠它）
            if l.width != w || l.height != h || l.panel.w != w || l.panel.h != h {
                failures.push(format!("{tag} 面板未覆盖客户区"));
            }

            // 描边矩形：**四边内缩必须相等**，且外沿不越出客户区。
            //
            // 元组含义按消费方 `render::painters::rounded` 来：`(left, top, right, bottom)`。
            // 它把第 3/4 个参数直接塞进 `D2D_RECT_F.right/bottom`（`rounded_i` 才做
            // `x+w` 的换算），所以往这个字段里写"宽/高"会**静默**变成右下角坐标。
            //
            // 这正是 2026-10-18 那次改动（`67208b4`）的错：文档写的是
            // `(left, top, width, height)`，于是算出 `right = 宽 − 1.2`，
            // 而正确值是 `宽 − 0.6` —— 左/上内缩 0.6、右/下内缩 1.2，
            // **描边不对称，缺口全落在底部与右侧**（使用者报的"底部两角"）。
            //
            // 所以断言不能只查"没越界"（越界方向是安全的，错的那版恰好更保守、
            // 所以能骗过旧断言）。必须查**四个内缩量彼此相等**。
            {
                let half = popout::layout::PANEL_STROKE / 2.0;
                let (sl, st, sr, sb) = l.panel_stroke;
                let insets = [sl, st, w as f32 - sr, h as f32 - sb];
                for (i, v) in insets.iter().enumerate() {
                    if (v - half).abs() > 0.001 {
                        failures.push(format!(
                            "{tag} 描边第 {i} 边内缩 {v:.2}，应为 {half:.2}（四边必须相等，\
                             元组是 left/top/right/bottom）"
                        ));
                    }
                }
                // 外沿（中线外扩半个线宽）不得越出客户区
                if sl - half < -0.001
                    || st - half < -0.001
                    || sr + half > w as f32 + 0.001
                    || sb + half > h as f32 + 0.001
                {
                    failures.push(format!(
                        "{tag} 描边外沿越出客户区（描边 {sl},{st},{sr},{sb}，半线宽 {half}）"
                    ));
                }
                // 半径也不能因内缩变成负数（那会让 D2D 行为不可预期）
                if l.panel_stroke_radius(16.0) < 0.0 {
                    failures.push(format!("{tag} 描边半径算成负数"));
                }
            }

            // 四个按钮都要落在窗口内
            for (i, b) in l.buttons.iter().enumerate() {
                if b.x < 0 || b.y < 0 || b.x + b.w > w || b.y + b.h > h {
                    failures.push(format!("{tag} 第 {i} 个按钮越界"));
                }
            }
            // 按钮两两不重叠
            for i in 0..4 {
                for j in (i + 1)..4 {
                    if l.buttons[i].intersects(&l.buttons[j]) {
                        failures.push(format!("{tag} 按钮 {i}/{j} 重叠"));
                    }
                }
            }
            // 关闭按钮必须在最右（它点得最多、最不能点错）
            if l.buttons[3].x <= l.buttons[2].x || l.buttons[3].x <= l.buttons[0].x {
                failures.push(format!("{tag} 关闭不在最右"));
            }
            // 标题文字不能压到按钮组上
            if l.title_text.x + l.title_text.w > l.buttons[0].x {
                failures.push(format!("{tag} 标题压到按钮"));
            }
            // 内容区要在窗口内且非负
            if l.content.w < 0 || l.content.h < 0 {
                failures.push(format!("{tag} 内容区为负"));
            }
            if l.content.y + l.content.h > h {
                failures.push(format!("{tag} 内容区越界"));
            }
            // 网格至少一列，方格边长不小于下限
            if l.columns < 1 || l.tile < 16 {
                failures.push(format!("{tag} 网格参数非法"));
            }
            // 命中测试：每个按钮的中心都要命中它自己
            for (i, b) in l.buttons.iter().enumerate() {
                let hit = l.button_at(b.x + b.w / 2, b.y + b.h / 2);
                let want = popout::layout::TITLE_BUTTONS[i];
                if hit != Some(want) {
                    failures.push(format!("{tag} 第 {i} 个按钮中心命中错误"));
                }
            }
            // 内容区（避开方格）不应命中任何标题栏按钮
            if l.button_at(l.content.x + 4, l.content.y + 4).is_some() {
                failures.push(format!("{tag} 内容区误命中标题栏按钮"));
            }
            // 按钮间距必须真的是 BUTTON_GAP（相邻按钮左边缘之差）
            let pitch = l.buttons[1].x - l.buttons[0].x;
            if pitch != l.buttons[0].w + BUTTON_GAP {
                failures.push(format!("{tag} 按钮间距不对"));
            }
            // 标题栏高度不超过窗口高度
            if TITLE_BAR > h && l.title_bar.h != h {
                failures.push(format!("{tag} 标题栏超高"));
            }
            let _ = TitleButton::Color; // 枚举被引用过，避免"未使用"噪声
        }

        // 方格排布：第 0 个在网格原点，第 columns 个换行
        let l = PopoutLayout::compute(320, 420, 48);
        let rh = l.row_height();
        if l.tile_at(0, rh) != l.grid_origin {
            failures.push("首格不在网格原点".to_string());
        }
        let (x1, y1) = l.tile_at(l.columns, rh);
        if x1 != l.grid_origin.0 || y1 <= l.grid_origin.1 {
            failures.push("换行位置不对".to_string());
        }

        // ---- 面板填充必须盖住**整个物理客户区**（2026-10-18 的"底部浅色边"）----
        //
        // 布局尺寸是逻辑整数（`round(物理/缩放)`），绘制时再乘回缩放系数 ——
        // 先除后乘会掉一点，面板底边停在第 N−0.5 个物理像素上，最后半行没盖住，
        // 透出浅灰桌面（经抗锯齿就是一道极细的浅色边）。修复是让填充矩形
        // 右/下各外扩 `PANEL_OVERDRAW`。这条断言钉住"外扩之后必定覆盖"。
        //
        // 用例取**本机真实的缩放系数 1.25**（AppliedDPI = 120）与真实窗口尺寸，
        // 并钉上每一组的期望值：只断言"覆盖"会漏掉"逻辑尺寸算错"这类错。
        {
            let scale = 1.25_f32;
            // 603 与 453 是 round **向下**的用例：
            // 外扩之前 482×1.25 = 602.5 < 603、362×1.25 = 452.5 < 453。
            for (phys, want_lw) in [(603, 482), (453, 362), (449, 359), (450, 360), (455, 364)] {
                let (lw, _) = popout::window::logical_size(phys, phys, scale);
                if lw != want_lw {
                    failures.push(format!(
                        "物理 {phys} ÷ {scale} 的逻辑尺寸应为 {want_lw}，实为 {lw}"
                    ));
                }
                let fill_right = lw as f32 + popout::layout::PANEL_OVERDRAW;
                if fill_right * scale < phys as f32 {
                    failures.push(format!(
                        "{phys} 物理像素：填充只到 {:.2} 物理像素，**没盖住客户区**",
                        fill_right * scale
                    ));
                }
            }
            // 外扩量不能小于 `round` 可能掉掉的上界（严格小于 0.5）。
            if popout::layout::PANEL_OVERDRAW < 0.5 {
                failures.push("PANEL_OVERDRAW 小于 round 可能掉掉的上界".to_string());
            }
            // 真布局里的填充矩形必须真的用上了外扩（防止只改了常量、没接线）。
            let probe = PopoutLayout::compute(453, 453, 48);
            if probe.panel_fill.2 < 453.0 + 0.5 {
                failures.push(format!(
                    "布局的 panel_fill 右边界没外扩：{:.2}",
                    probe.panel_fill.2
                ));
            }
        }

        selftest_line!(
            "弹出窗布局 : {}",
            if failures.is_empty() {
                format!(
                    "4 种尺寸下 按钮不越界/不重叠/关闭最右/标题不压按钮/内容区合法/命中正确/间距正确/**描边不越出客户区**/**描边四边内缩相等**/**填充盖住物理客户区（含 1.25 缩放逐项期望值）** —— 全部通过（典型 320×420：{} 列，方格 {}）",
                    l.columns, l.tile
                )
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // 弹出子窗口的**点击命中**：点哪一格才算点到了它。
    //
    // 纯几何，所以无头自检能钉住 —— 而"点上去真的启动了程序"不行（要真窗口），
    // 那一半只能靠真实界面确认。这里钉的是边界：格内、名称行、格间空隙、
    // 标题栏、四个按钮、行尾空白、被裁掉的那半行。
    {
        use popout::layout::{PopoutLayout, TITLE_BUTTONS};

        let mut failures: Vec<String> = Vec::new();
        let l = PopoutLayout::compute(320, 420, 48);
        let rh = l.row_height();
        let total = 40usize; // 多于放得下的格数，用来检查"被裁掉的不该命中"
        let visible = l.visible_tiles(total);
        if visible == 0 {
            failures.push("320×420 下一个方格都放不下".to_string());
        }
        let mid = |r: (i32, i32)| (r.0 + l.tile / 2, r.1 + rh / 2);

        // 1. 每一格的中心都要命中它自己（方格正中）
        for i in 0..visible {
            let r = l.tile_at(i, rh);
            if l.tile_index_at(r.0 + l.tile / 2, r.1 + l.tile / 2, total) != Some(i) {
                failures.push(format!("第 {i} 格中心没有命中它自己"));
            }
        }

        // 2. 名称行也算命中那一格（与主窗口一致：方格矩形含名称）
        let (x0, y0) = l.tile_at(0, rh);
        if l.tile_index_at(x0 + 2, y0 + l.tile + 4, total) != Some(0) {
            failures.push("名称行没有命中格子".to_string());
        }

        // 3. 格间空隙不该命中任何一格
        //    取第 0 格的右边缘再往右 1px（仍在 TILE_GAP 里）
        if l.tile_index_at(x0 + l.tile + 1, y0 + l.tile / 2, total)
            .is_some()
        {
            failures.push("格间空隙误命中".to_string());
        }
        //    行下沿再往下 1px（仍在行距里）
        if l.tile_index_at(x0 + l.tile / 2, y0 + rh + 1, total)
            .is_some()
        {
            failures.push("行间空隙误命中".to_string());
        }

        // 4. 标题栏与四个按钮都不该命中方格
        //    （标题栏在内容区之上，按钮也在标题栏里）
        for (i, b) in l.buttons.iter().enumerate() {
            let hit = l.tile_index_at(b.x + b.w / 2, b.y + b.h / 2, total);
            if hit.is_some() {
                failures.push(format!("第 {i} 个标题栏按钮误命中方格"));
            }
            let _ = TITLE_BUTTONS[i];
        }
        if l.tile_index_at(l.title_text.x + 2, l.title_text.y + 2, total)
            .is_some()
        {
            failures.push("标题栏误命中方格".to_string());
        }

        // 5. 被裁掉的那半行**不该**命中：放不下的格子既没画、也取不到图标，
        //    它若还能被点到，用户会点到一个看不见的东西并启动程序。
        if visible < total {
            let r = l.tile_at(visible, rh);
            if l.tile_index_at(r.0 + l.tile / 2, r.1 + l.tile / 2, total)
                .is_some()
            {
                failures.push("放不下的格子仍然能命中".to_string());
            }
        }

        // 6. 条目数少于可见格数时，多出来的格子不该命中
        //    （`visible_tiles(total)` 用的是**条目数**，这里用小 total 验证）
        let small = 1usize;
        if l.tile_index_at(x0 + l.tile / 2, y0 + l.tile / 2, small) != Some(0) {
            failures.push("只有一条时第 0 格命不中".to_string());
        }
        let r1 = l.tile_at(1, rh);
        if l.tile_index_at(r1.0 + l.tile / 2, r1.1 + l.tile / 2, small)
            .is_some()
        {
            failures.push("只有一条时第 1 格仍然能命中".to_string());
        }

        // 7. 窗口小到只剩标题栏时，命中必须是 None 而不是 panic
        let tiny = PopoutLayout::compute(200, 40, 48);
        if tiny
            .tile_index_at(mid((0, 0)).0, mid((0, 0)).1, total)
            .is_some()
        {
            failures.push("极窄窗口仍然命中了方格".to_string());
        }

        selftest_line!(
            "弹窗点击命中: {}",
            if failures.is_empty() {
                format!(
                    "格内 / 名称行 / 格间空隙 / 标题栏 / 四个按钮 / 放不下的半行 / 条目数不足 / 极窄窗口 —— 全部通过（320×420 可见 {visible} 格）"
                )
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // 弹出子窗口标题栏四个图标的外形。纯几何，验证"画的和点的在同一块地方"。
    {
        use popout::layout::{PopoutLayout, TITLE_BUTTONS};
        use popout::painter::{Glyph, clover_colors, glyph};

        let mut failures: Vec<String> = Vec::new();

        for (w, h) in [(200, 160), (320, 420), (900, 1400)] {
            let l = PopoutLayout::compute(w, h, 48);
            let tag = format!("{w}×{h}");

            for (i, btn) in TITLE_BUTTONS.iter().enumerate() {
                let rect = l.buttons[i];
                let shapes = glyph(*btn, rect, false);
                if shapes.is_empty() {
                    failures.push(format!("{tag} {btn:?} 没有外形"));
                    continue;
                }
                // 每个原语都必须落在自己的按钮矩形内（0.01 容差给浮点）
                for (k, s) in shapes.iter().enumerate() {
                    let (x0, y0, x1, y1) = s.bounds();
                    if x0 < rect.x as f32 - 0.01
                        || y0 < rect.y as f32 - 0.01
                        || x1 > (rect.x + rect.w) as f32 + 0.01
                        || y1 > (rect.y + rect.h) as f32 + 0.01
                    {
                        failures.push(format!("{tag} {btn:?} 第 {k} 个原语画出按钮外"));
                    }
                }
            }

            // 四个图标外形必须互不相同，否则用户分不清哪个是哪个
            let sigs: Vec<String> = TITLE_BUTTONS
                .iter()
                .map(|b| {
                    glyph(*b, l.buttons[0], false)
                        .iter()
                        .map(|g| match g {
                            Glyph::Disc { .. } => 'D',
                            Glyph::Line { .. } => 'L',
                        })
                        .collect::<String>()
                })
                .collect();
            if sigs.iter().collect::<std::collections::HashSet<_>>().len() != 4 {
                failures.push(format!("{tag} 有图标外形重复: {sigs:?}"));
            }

            // 叉必须左右对称
            let close = l.buttons[3];
            let x = glyph(popout::layout::TitleButton::Close, close, false);
            if let (
                [
                    Glyph::Line { a: a0, b: b0, .. },
                    Glyph::Line { a: a1, b: b1, .. },
                ],
                true,
            ) = (x.as_slice(), x.len() == 2)
            {
                let cx = close.x as f32 + close.w as f32 / 2.0;
                if (a0.0 - cx).abs() != (a1.0 - cx).abs() || (b0.0 - cx).abs() != (b1.0 - cx).abs()
                {
                    failures.push(format!("{tag} 叉左右不对称"));
                }
            } else {
                failures.push(format!("{tag} 叉不是两条线"));
            }
        }

        // "彩色"四叶草：四个圆必须是四个不同颜色
        if clover_colors()
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != 4
        {
            failures.push("四叶草颜色有重复".to_string());
        }

        selftest_line!(
            "弹出窗图标 : {}",
            if failures.is_empty() {
                "3 种尺寸下 4 个图标 全部落在各自按钮内 / 外形互不相同 / 叉左右对称 / 四叶草四色不同 —— 全部通过".to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // 弹出子窗口的图标管线。**只测与窗口无关的那部分**：键的构造与内容比较。
    //
    // 真正"创建第二个窗口 → 后台提取 → 转位图 → 画上去"那条路径自检走不到
    //（前两轮的悬垂指针崩溃与"漏了 show()"两个 bug 就是这样漏过去的），
    // 所以这里不声称验证了图标能画出来。能测死的是那条链路的**规则**：
    // 尺寸换算错了、或者"按图"与"按图标"混用，图标会看起来不对而不报任何错。
    {
        use popout::icons::{Content, IconSource, Item, pixel_size};

        let mut failures: Vec<String> = Vec::new();

        // 像素边长换算：与主窗口 `App::collect_icons` 同一条规则
        //（DIP × 缩放系数，再夹到 16~256 物理像素）。
        for (dip, dpi, want) in [
            (128, 96.0, 128u32),
            (128, 144.0, 192),
            (128, 192.0, 256), // 上限
            (8, 96.0, 16),     // 下限
            (300, 96.0, 256),  // 上限
            (48, 120.0, 60),
        ] {
            let got = pixel_size(dip, dpi);
            if got != want {
                failures.push(format!("{dip}DIP@{dpi} 缩放 → {got}px（应为 {want}）"));
            }
        }

        // 键的三条性质。少任何一条都会有具体后果：
        // * 同来源同尺寸必须相等 —— 否则每帧都被当成"还没加载"，无限重复请求；
        // * 尺寸不同必须不等 —— 否则改图标大小后会拿旧分辨率的位图去画，发虚；
        // * 按图 / 按图标必须分开 —— 否则用户"更换图标"选了图片也不生效。
        let icon = IconSource::icon(r"C:\Windows\System32\notepad.exe");
        let image = IconSource::image(r"C:\Windows\System32\notepad.exe");
        if icon.key(128) != icon.key(128) {
            failures.push("同来源同尺寸的键不相等".to_string());
        }
        if icon.key(128) == icon.key(256) {
            failures.push("尺寸不同却得到同一个键".to_string());
        }
        if icon.key(128) == image.key(128) {
            failures.push("按图与按图标得到同一个键".to_string());
        }

        // 内容比较：`set_content` 靠它决定"要不要重建键、要不要重绘"。
        // 判错的方向有两个，两个都测：**该等的等不了**（白重建、白重绘），
        // **该变的没变**（弹出窗口停在旧内容上）。
        let items = vec![
            Item {
                name: "记事本".to_string(),
                source: icon.clone(),
                lnk: std::path::PathBuf::from(r"C:\数据\shortcuts\记事本.lnk"),
                target: std::path::PathBuf::from(r"C:\Windows\System32\notepad.exe"),
            },
            Item {
                name: "画图".to_string(),
                source: image.clone(),
                lnk: std::path::PathBuf::from(r"C:\数据\shortcuts\画图.lnk"),
                target: std::path::PathBuf::from(r"C:\Windows\System32\mspaint.exe"),
            },
        ];
        let base = Content {
            icon_size: 48,
            items: items.clone(),
        };
        let same = Content {
            icon_size: 48,
            items,
        };
        if base != same {
            failures.push("同样的内容被判成不同（会反复重建键）".to_string());
        }
        let mut renamed = base.clone();
        renamed.items[0].name = "另一个名字".to_string();
        if renamed == base {
            failures.push("改名没被察觉".to_string());
        }
        let mut resized = base.clone();
        resized.icon_size = 96;
        if resized == base {
            failures.push("图标尺寸变化没被察觉".to_string());
        }
        let mut reordered = base.clone();
        reordered.items.reverse();
        if reordered == base {
            failures.push("顺序变化没被察觉".to_string());
        }
        // 启动目标变了也要被察觉：`.lnk` 还在、名字也没改，但用户重新指向了
        // 别的程序（或换个图标后我们记的目标路径变了）。`Item` 里那两个字段
        // 必须参与相等比较，否则弹窗会拿着旧目标启动。
        let mut retargeted = base.clone();
        retargeted.items[0].target = std::path::PathBuf::from(r"D:\别的\程序.exe");
        if retargeted == base {
            failures.push("启动目标变化没被察觉".to_string());
        }

        selftest_line!(
            "弹出窗图标管线: {}",
            if failures.is_empty() {
                "像素换算 6 档 / 键同源相等、随尺寸变化、按图与按图标分开 / 内容比较（改名·重排·改尺寸·换启动目标）—— 全部通过".to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // 弹窗与主窗口的显示名必须走**同一条规则**（别名优先）。
    //
    // 这条规则一度在两处各写一份：主窗口绘制查了 `alias`，弹窗推送直接用
    // `shortcut.name` —— 表现是"在主窗口改了名，弹窗里连**关掉重开**都不变"
    //（推的内容本身就是旧的，跟同步机制无关）。现在规则只有一处
    //（`model::library::display_name`），这里把它的三条语义钉住。
    {
        let mut failures: Vec<String> = Vec::new();
        let shortcut = crate::shell::lnk::Shortcut {
            lnk_path: std::path::PathBuf::from(r"E:\tmp\shortcuts\记事本.lnk"),
            origin: crate::shell::lnk::Origin::Extra,
            name: "记事本".to_string(),
            target: std::path::PathBuf::from(r"C:\Windows\notepad.exe"),
            args: String::new(),
            work_dir: std::path::PathBuf::new(),
            description: String::new(),
            shell_name: None,
        };
        let key = crate::model::library::path_key(&shortcut);

        // 1. 没有覆盖记录 → 回退原名
        let none = std::collections::BTreeMap::new();
        if crate::model::library::display_name(&shortcut, &none) != "记事本" {
            failures.push("没有别名时没有回退到原名".to_string());
        }

        // 2. 有别名 → 用别名（这就是"重命名"）
        let mut overrides = std::collections::BTreeMap::new();
        let mut ov = crate::state::EntryOverride::default();
        ov.alias = Some("我的记事本".to_string());
        overrides.insert(key.clone(), ov.clone());
        if crate::model::library::display_name(&shortcut, &overrides) != "我的记事本" {
            failures.push("有别名时没有用别名".to_string());
        }

        // 3. 别名被清空（"空文本等于清除别名"）→ 回退原名
        ov.alias = None;
        overrides.insert(key, ov);
        if crate::model::library::display_name(&shortcut, &overrides) != "记事本" {
            failures.push("别名清空后没有回退到原名".to_string());
        }

        selftest_line!(
            "显示名规则 : {}",
            if failures.is_empty() {
                "别名优先 / 无别名回退原名 / 别名清空回退原名 —— 全部通过".to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // 弹窗的关闭回报通道。
    //
    // 关窗有两条路（主窗口菜单 / 弹窗自己的 ✕），只有后者窗口层才知道；
    // 装配层靠这个通道收尾。这里钉住两条语义：**能认出是哪一个**（存 key）
    // 与**取走即清空**（否则同一次关闭会被重复收尾，第二次会把已经关掉的
    // 窗口再"关"一遍，或者把刚重开的那个误删）。
    {
        let mut failures: Vec<String> = Vec::new();
        crate::popout::report::report_closed("tab2");
        crate::popout::report::report_closed("tab3");
        let mut got = crate::popout::report::take_closed();
        got.sort();
        let want = vec!["tab2".to_string(), "tab3".to_string()];
        if got != want {
            failures.push(format!("回报内容不对: {got:?}"));
        }
        if !crate::popout::report::take_closed().is_empty() {
            failures.push("取走之后又取到同一批回报（会重复收尾）".to_string());
        }

        selftest_line!(
            "弹窗关闭回报: {}",
            if failures.is_empty() {
                "按选项卡 key 回报 / 取走即清空 —— 全部通过".to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // 悬停提示的几何（使用者 2026-10-20："鼠标放在快捷方式上 0.3 秒弹出全名"）。
    //
    // 纯几何、不建窗口就能验证。重点钉住三件事：
    // 1. 提示框必须**完整落在窗口内** —— 画到窗口外的部分看不见，等于提示丢了；
    // 2. 默认在方格**下方**，下方放不下翻到上方（不能盖住图标本身）；
    // 3. 宽度随名字增长但**有上限**，否则超长名字会撑出一个跑出窗口的框。
    {
        use crate::platform::hittest::RectI;
        use crate::render::painters::TooltipPainter;

        let mut failures: Vec<String> = Vec::new();
        let bounds = RectI::new(0, 0, 640, 880);

        // 一个居中的方格
        let anchor = RectI::new(200, 300, 64, 64);

        // 1. 常规情况：在方格下方、水平居中
        let f = TooltipPainter::frame(anchor, "很长的程序名字用来触发提示", bounds);
        if f.y <= anchor.y + anchor.h {
            failures.push(format!("常规情况下没有放在方格下方: y={}", f.y));
        }
        if f.x < bounds.x || f.x + f.w > bounds.x + bounds.w {
            failures.push("提示框水平越界".to_string());
        }

        // 2. 窗口底部的方格：下方放不下，必须翻到上方
        let low = RectI::new(200, 830, 64, 64);
        let f = TooltipPainter::frame(low, "底部这条", bounds);
        if f.y + f.h > bounds.y + bounds.h {
            failures.push(format!(
                "底部方格下方放不下，提示框越界: y+h={} > {}",
                f.y + f.h,
                bounds.y + bounds.h
            ));
        }
        if f.y >= low.y {
            failures.push("底部方格没有把提示翻到上方".to_string());
        }

        // 3. 超长名字：宽度有上限，且不越界
        let long_name: String = "超长".repeat(200);
        let f = TooltipPainter::frame(anchor, &long_name, bounds);
        if f.w > 320 {
            failures.push(format!("提示框宽度没有上限: {}", f.w));
        }
        if f.x + f.w > bounds.x + bounds.w || f.x < bounds.x {
            failures.push("超长名字导致提示框越界".to_string());
        }

        // 4. 窗口很窄时（方格贴着右边缘）仍不越界
        let narrow = RectI::new(0, 0, 200, 400);
        let edge = RectI::new(180, 100, 20, 20);
        let f = TooltipPainter::frame(edge, "贴边的一条", narrow);
        if f.x + f.w > narrow.x + narrow.w || f.x < narrow.x {
            failures.push(format!(
                "窄窗口里提示框越界: x={} w={} 窗口宽={}",
                f.x, f.w, narrow.w
            ));
        }

        // 5. 截断标记：超长的要带省略号，短的保持原样
        let fitted = crate::render::painters::tooltip::fit_label(&long_name, 320);
        if !fitted.ends_with('…') {
            failures.push("超长名字没有被截断并加省略号".to_string());
        }
        if crate::render::painters::tooltip::fit_label("记事本", 320) != "记事本" {
            failures.push("短名字被改动了".to_string());
        }

        selftest_line!(
            "悬停提示   : {}",
            if failures.is_empty() {
                "方格下方优先 / 下方放不下翻上方 / 宽度有上限 / 窄窗口不越界 / 截断加省略号 —— 全部通过"
                    .to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // 每个窗口的图标大小互不干扰（使用者 2026-10-20："每个窗口各自设定"）。
    //
    // 规则本体在 `popout::state::PopoutWindow::effective_icon_size`，
    // 窗口层 `Popout` 实现的是它的等价形式（只有 `Option<u32>`，没有整个状态）。
    // 这里把**两者必须一致**这件事钉住：将来谁改了一边，自检立刻报出来。
    {
        use popout::state::{MAX_ICON, MIN_ICON, PopoutWindow, step_icon};

        let mut failures: Vec<String> = Vec::new();

        // 0 = 跟随主窗口
        let follow = PopoutWindow::default();
        if follow.effective_icon_size(64) != 64 {
            failures.push("没设过的窗口没有跟随主窗口".to_string());
        }

        // 两个窗口各设各的，互不影响
        let mut a = PopoutWindow::default();
        let b = PopoutWindow::default();
        a.icon_size = 128;
        if a.effective_icon_size(64) != 128 {
            failures.push("设过的窗口没有用自己的值".to_string());
        }
        if b.effective_icon_size(64) != 64 {
            failures.push("别的窗口被波及了".to_string());
        }
        if a.effective_icon_size(32) != 128 {
            failures.push("设过的窗口被主窗口的改动带走了".to_string());
        }

        // `0` 必须能穿过 normalize（否则一启动就永远不再跟随主窗口）
        let mut w = PopoutWindow::default();
        w.normalize();
        if w.icon_size != 0 {
            failures.push("normalize 把'跟随主窗口'的 0 改掉了".to_string());
        }
        w.icon_size = 9999;
        w.normalize();
        if w.icon_size != MAX_ICON {
            failures.push("越界的图标边长没有被收进上限".to_string());
        }
        w.icon_size = 1;
        w.normalize();
        if w.icon_size != MIN_ICON {
            failures.push("越界的图标边长没有被收进下限".to_string());
        }

        // 档位步进：一格一格走，到顶到底停住
        if step_icon(64, true, 256) != Some(96) || step_icon(64, false, 256) != Some(48) {
            failures.push("档位步进不对".to_string());
        }
        if step_icon(256, true, 256).is_some() || step_icon(16, false, 256).is_some() {
            failures.push("到达上下限后还能继续走".to_string());
        }

        selftest_line!(
            "每窗图标尺寸: {}",
            if failures.is_empty() {
                "0=跟随主窗口 / 同窗口各设各的 / 主窗口改动不波及已设过的窗口 / normalize 保住 0 / 档位到顶到底停住 —— 全部通过"
                    .to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // **图标尺寸按选项卡分开**（使用者 2026-10-20 原话）：
    //
    // > 主窗口内选项卡和对应弹出一样尺寸可以，但是不同的选项卡之间必须分开。
    //
    // 这是上一条的**语义更正**：早先按"窗口"存，于是主窗口一个值、每个弹窗
    // 各一个值，切选项卡时主窗口不跟着换 —— 表现就是"还是全部同步大小"。
    // 现在按选项卡 key 存，主窗口与它的弹窗读同一个键。
    {
        use crate::state::AppState;

        let mut failures: Vec<String> = Vec::new();
        let mut s = AppState {
            icon_size: 64,
            ..Default::default()
        };

        // 没设过的选项卡用兜底值
        if s.icon_size_of("tab2") != 64 || s.icon_size_of("tab3") != 64 {
            failures.push("没设过的选项卡没有用兜底值".to_string());
        }

        // 给 tab2 设一个：tab3 与主界面**不许**受影响
        s.set_icon_size("tab2", 128);
        if s.icon_size_of("tab2") != 128 {
            failures.push("tab2 自己的值没生效".to_string());
        }
        if s.icon_size_of("tab3") != 64 {
            failures.push("改 tab2 波及了 tab3（这就是那条 bug）".to_string());
        }
        if s.icon_size_of("main") != 64 {
            failures.push("改 tab2 波及了主界面".to_string());
        }

        // 再给 tab3 另一个值，两者独立
        s.set_icon_size("tab3", 32);
        if s.icon_size_of("tab2") != 128 || s.icon_size_of("tab3") != 32 {
            failures.push("两个选项卡的值没有各自独立".to_string());
        }

        // 改回兜底值 -> 记录应当消失（不让状态文件无限增长）
        s.set_icon_size("tab2", 64);
        if s.icon_sizes.contains_key("tab2") {
            failures.push("改回默认值后记录还在".to_string());
        }

        selftest_line!(
            "图标尺寸归属: {}",
            if failures.is_empty() {
                "按选项卡分开 / 主界面与各选项卡互不影响 / 没设过用兜底值 / 改回默认值清掉记录 —— 全部通过"
                    .to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // 滚轮必须先判断"鼠标在不在自己身上"（使用者 2026-10-20："图标还是统一变化"）。
    //
    // `WM_MOUSEWHEEL` 发给**焦点窗口**，不是鼠标下面的窗口。主窗口是唯一持有焦点的
    // 那个，所以在弹窗上滚轮时主窗口也会收到 —— 不做落点判断就会两边一起改。
    // 这里验证那个判据本身（纯函数，不需要建窗口）。
    {
        let mut failures: Vec<String> = Vec::new();

        // 判据：坐标必须落在自己的客户区里（含左上、不含右下，与 RectI::contains 同口径）
        let inside = |x: i32, y: i32, w: i32, h: i32| x >= 0 && y >= 0 && x < w && y < h;

        // 实测得到的真实数字：鼠标停在弹窗(x=500,w=300)上时，
        // 主窗口(x=100,w=300) 收到的换算坐标是 (443,93) —— 落在自己 300 宽之外。
        if inside(443, 93, 300, 300) {
            failures.push("误投到主窗口的滚轮坐标被判成'在自己身上'".to_string());
        }
        // 同一个消息在弹窗里换算是 (43,93)，落在自己身上
        if !inside(43, 93, 300, 300) {
            failures.push("真的落在弹窗上的滚轮坐标没有被认出来".to_string());
        }
        // 边界：左上角算在内、右下角在外
        if !inside(0, 0, 300, 300) {
            failures.push("左上角应当算在窗口内".to_string());
        }
        if inside(300, 300, 300, 300) {
            failures.push("右下角应当算在窗口外".to_string());
        }
        // 负数（鼠标在窗口左上方）也必须算在外面
        if inside(-1, 50, 300, 300) || inside(50, -1, 300, 300) {
            failures.push("负坐标应当算在窗口外".to_string());
        }

        selftest_line!(
            "滚轮落点   : {}",
            if failures.is_empty() {
                "只认落在自己客户区里的滚轮 / 误投的坐标被拒 / 负坐标与右下角算在外 —— 全部通过"
                    .to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    // 窗口外框的圆角：系统偏好与面板半径必须**同源**。
    //
    // 这条规则被三处消费（主窗口的 `acrylic::apply`、主窗口面板绘制、
    // 每个弹出子窗口）。弹窗此前既不设系统圆角、面板半径又写死 10.0，
    // 于是拿到 Win11 的默认圆角 —— 使用者报的"弹出小窗底部是圆角"。
    // 这里把推导钉死，任何人把它改回去都会立刻看见失败。
    {
        use config::model::Appearance;

        let mut failures: Vec<String> = Vec::new();

        // 开了外圈玻璃：系统圆角开，面板用配置里的半径
        let on = Appearance {
            outer_glass: true,
            corner_radius: 16.0,
            ..Default::default()
        };
        let c = on.corner_style(true);
        if !c.rounded || (c.radius - 16.0).abs() > f32::EPSILON {
            failures.push(format!("外圈玻璃开时应为 圆角/16.0，实为 {c:?}"));
        }

        // 关掉外圈玻璃 = 平面模式：**系统圆角也必须一起关**
        //（只关面板半径的话，DWM 仍会把窗口角部裁圆、露出桌面）
        let c = on.corner_style(false);
        if c.rounded || c.radius != 0.0 {
            failures.push(format!("平面模式应为 直角/0.0，实为 {c:?}"));
        }

        // 边界：半径为 0 但玻璃开着 —— 面板是直角，系统偏好仍为"圆角"。
        // 两者**不同**是合法的：`rounded` 说的是"要不要用系统圆角能力"，
        // 而 DWM 对 0 半径没有意见；把它当成"必须一起为假"就会引入一条假规则。
        let zero = Appearance {
            outer_glass: true,
            corner_radius: 0.0,
            ..Default::default()
        };
        let c = zero.corner_style(true);
        if !c.rounded || c.radius != 0.0 {
            failures.push(format!("半径 0 且玻璃开时应为 圆角/0.0，实为 {c:?}"));
        }

        // 生效值来自"实际外圈玻璃状态"，而不是配置里的 `outer_glass` 字段：
        // 设置面板改的是 state.toml 的覆盖项，config 里那份可能仍是旧值。
        // 这里用 `App` 的生效值再算一次，确认它与配置同规则。
        let live = app.outer_glass_enabled();
        let cfg_style = app.corner_style_for_test();
        if cfg_style.rounded != live {
            failures.push(format!(
                "系统圆角偏好({})与生效的外圈玻璃({live})不一致",
                cfg_style.rounded
            ));
        }

        // ---- 弹窗**绘制**半径：DWM 负责圆角时必须是 0 ----
        //
        // 这是"底部两角浅色边"的修法所依赖的规则，所以必须用**具体期望值**钉住，
        // 而不是"看着对"。三种取值都要查，尤其第三条 —— 它防的是"无条件返回 0"
        // 这种把规则写死的假实现。
        let glass_on = crate::config::model::CornerStyle {
            rounded: true,
            radius: 16.0,
        };
        if glass_on.drawn_radius() != 0.0 {
            failures.push(format!(
                "DWM 圆角开时弹窗应画直角 0.0，实为 {}（角上会露出月牙）",
                glass_on.drawn_radius()
            ));
        }
        let flat = crate::config::model::CornerStyle {
            rounded: false,
            radius: 0.0,
        };
        if flat.drawn_radius() != 0.0 {
            failures.push(format!(
                "平面模式应画直角 0.0，实为 {}",
                flat.drawn_radius()
            ));
        }
        let own = crate::config::model::CornerStyle {
            rounded: false,
            radius: 16.0,
        };
        if own.drawn_radius() != 16.0 {
            failures.push(format!(
                "不让 DWM 圆角时应画自己的 16.0，实为 {}（规则写死成 0 了）",
                own.drawn_radius()
            ));
        }
        // 生效值上再确认一次：使用者当前若在玻璃模式，弹窗绘制半径必须是 0。
        if cfg_style.rounded && cfg_style.drawn_radius() != 0.0 {
            failures.push(format!(
                "生效值 DWM 圆角为真，但弹窗绘制半径是 {}（角上会露出月牙）",
                cfg_style.drawn_radius()
            ));
        }

        selftest_line!(
            "外框圆角   : {}",
            if failures.is_empty() {
                "玻璃开=圆角+配置半径 / 玻璃关=直角+0.0 / 半径0是合法边界 / 取自生效值而非配置字段 / **弹窗绘制半径：DWM 圆角时为 0**（含无条件返回 0 的反例） —— 全部通过"
                    .to_string()
            } else {
                format!("⚠ {} 项不通过: {}", failures.len(), failures.join("；"))
            }
        );
    }

    selftest_line!(
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
                selftest_line!(
                    "改尺寸({label}) : 窗口 {cw:.0}×{ch:.0} → 渲染器 {dw:.0}×{dh:.0}，面板 {pw:.0}×{ph:.0}{}",
                    if ok {
                        ""
                    } else {
                        "   ⚠ 渲染器未跟上窗口"
                    }
                );
                if !ok {
                    selftest_line!("             ⚠ 交换链没有按新尺寸重建，内容会被裁切或留白");
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
            selftest_line!(
                "滚动到末尾 : 选项卡「{}」可见第 {}~{} 条（共 {} 条），总可滚 {:.0} DIP",
                tab_key,
                first + 1,
                end,
                total,
                max_scroll
            );
            if total > 0 && end < total {
                selftest_line!("             ⚠ 末尾未完全可见，滚动计算可能有误");
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
            selftest_line!(
                "设置面板   : 绘制成功；玻璃开关{}",
                if works {
                    "已接通系统层"
                } else {
                    "⚠ 未生效"
                }
            );
            selftest_line!(
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
                    selftest_line!("             ⚠ 平面模式下仍有内缩或圆角，窗口外会残留透明");
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
        selftest_line!("缩放验证   : 图标px → 方格px，列×行，一屏条目数");
        let mut last_items = usize::MAX;
        let mut monotonic = true;
        for (icon, side, cols, r, items) in &rows {
            selftest_line!("              {icon:>3} → {side:>3}，{cols}×{r}，{items} 条");
            // 档位从小到大遍历，所以"一屏条目数"必须**单调不增** ——
            // 图标变大，一屏能放的只会变少。
            // 之前写成"不能下降"，把正确行为当成了失效。
            if *items > last_items {
                monotonic = false;
            }
            last_items = *items;
        }
        if !monotonic {
            selftest_line!("             ⚠ 一屏条目数没有随图标变大而减少，布局算错了");
        }
    }

    // 验证图标链路：请求 → 后台提取 → 转 D2D 位图 → 进缓存。
    // 放在内存采样之前，这样图标的内存占用会被算进"绘制后"那一段。
    {
        let (count, used, budget, evictions, inflight, dims) = app.selftest_icons();
        selftest_line!(
            "图标缓存   : {} 张位图，占用 {:.1} MB / 预算 {:.0} MB（淘汰 {} 次，仍在途 {}）",
            count,
            used as f64 / (1024.0 * 1024.0),
            budget as f64 / (1024.0 * 1024.0),
            evictions,
            inflight,
        );
        selftest_line!(
            "             单张 {:.0} KB，尺寸示例 {}",
            if count > 0 {
                used as f64 / count as f64 / 1024.0
            } else {
                0.0
            },
            dims
        );
        if count == 0 {
            selftest_line!("             ⚠ 一张图标都没加载出来，检查加载线程与提取链路");
        } else if used > budget {
            selftest_line!("             ⚠ 超出预算，LRU 淘汰没起作用");
        }
    }

    if let (Some(a), Some(d)) = (after_init, platform::memory::snapshot()) {
        selftest_line!(
            "绘制后     : 私有提交 {}  （本阶段 +{}）",
            platform::memory::human(d.private_commit),
            platform::memory::human(d.private_commit.saturating_sub(a.private_commit))
        );
    }

    if let Some(m) = platform::memory::snapshot() {
        selftest_line!("{}", "─".repeat(64));
        selftest_line!(
            "空载总计   : 私有提交 {} / 工作集 {}",
            platform::memory::human(m.private_commit),
            platform::memory::human(m.working_set)
        );
        selftest_line!("注意口径   : 任务管理器\"内存\"列是**私有工作集**，本工具尚未计算它");
        selftest_line!("             想核对可在程序运行时执行：");
        selftest_line!("             Get-Counter \"\\Process(quick-spin)\\Working Set - Private\"");
    }

    // 退出码必须反映实际结果：任何一行带 `⚠` 都算失败。
    // 不能恒返回 SUCCESS —— 那样"退出码 0"会被当成通过。
    let warnings = SELFTEST_WARNINGS.load(std::sync::atomic::Ordering::Relaxed);
    if warnings > 0 {
        eprintln!("自检有 {warnings} 行告警/失败");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
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
        st.window.width,
        st.window.height,
        st.window.x.map(|x| (x, st.window.y)),
        st.window.maximized
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
        scheme.tile_hover.0, scheme.tab_hover.0, scheme.tile_active.0, scheme.accent_soft.0
    );
    println!("  可选方案 :");
    for (key, name) in schemes.listing() {
        let mark = if key == cfg.appearance.scheme {
            "●"
        } else {
            "○"
        };
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
