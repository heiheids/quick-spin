//! 窗口层：创建无边框窗口、跑消息循环、把 Win32 消息翻译成事件。
//!
//! ## 反闪烁 / 反拖影措施（对应 `docs/ARCHITECTURE.md` §3.2）
//!
//! | 措施 | 位置 |
//! |---|---|
//! | `WS_EX_NOREDIRECTIONBITMAP`：无重定向位图，GDI 完全不参与绘制 | [`Window::create`] |
//! | `WM_ERASEBKGND` 直接返回 1，不擦背景 | [`wnd_proc`] |
//! | `WM_PAINT` 先 `ValidateRect` 再立即绘制 | [`wnd_proc`] |
//! | 标题栏返回 `HTCAPTION`，拖动交给系统（不手动 `SetWindowPos`） | [`wnd_proc`] |
//! | 消息积压时先排空再统一重绘一次，避免缩放时重绘风暴 | [`message_loop`] |
//!
//! ## 多窗口支持
//!
//! 每个窗口的状态挂在它自己的 `GWLP_USERDATA` 上（见 [`WindowCtx`]），
//! 而不是用线程局部变量。这样"弹出新窗口"功能可以直接复用，
//! 不用等到那时再回来重构。

use windows::Win32::Foundation::{
    GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM, WIN32_ERROR,
};
use windows::Win32::Graphics::Gdi::{HBRUSH, InvalidateRect, ScreenToClient, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
// 鼠标离开跟踪在 Input 模块下，不在 WindowsAndMessaging（容易记错）。
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::error::{AppError, Result, Win32Ext};

use super::hittest::{self, HitTest};

/// 类已存在。重复注册不是错误 —— "弹出新窗口"会多次创建窗口。
const ERROR_CLASS_ALREADY_EXISTS: WIN32_ERROR = WIN32_ERROR(1410);

/// 自定义消息：请求重绘一次。
///
/// 异步任务（例如图标提取线程）完成后用它唤醒消息循环。
/// 比挂一个轮询定时器省电得多：没有活干时线程真的在 `GetMessageW` 上睡着。
const WM_APP_REDRAW: u32 = WM_APP + 1;

/// `WM_MOUSELEAVE` = 0x02A3。
///
/// 这个名字在 `windows` crate 里被归到 `Win32::UI::Controls` 而不是
/// `WindowsAndMessaging`，glob 导入带不进来。为一个常量多开一个 feature 不值当，
/// 且**必须**在本地定义：若让它以变量绑定的形式出现在 match 分支里，
/// 它会变成"捕获所有值"的模式，导致后面所有分支静默不可达
/// （编译器只会给一条 `unreachable pattern` 警告，很容易漏掉）。
const WM_MOUSELEAVE: u32 = 0x02A3;

/// 窗口层向上层发出的事件。
// 不能 derive Copy：`FilesDropped` 带一个 `Vec<PathBuf>`。
// `Clone` 仍在，绝大多数事件是小的值类型，克隆代价可忽略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Resized { width: u32, height: u32 },
    DpiChanged { dpi: u32 },
    /// 开始 / 结束拖动窗口尺寸。
    /// 缩放期间上层应切到轻量绘制（架构设计铁律 6）。
    SizeMove { entering: bool },
    MouseDown { x: i32, y: i32 },
    MouseUp { x: i32, y: i32, inside: bool },
    /// 右键按下。单独一个事件而不是给 `MouseDown` 加 button 字段：
    /// 右键和左键的处理路径完全不同（右键弹菜单、不参与拖动与悬停状态机），
    /// 混在一个事件里会逼着每个左键分支都先判断一次按键。
    RightMouseDown { x: i32, y: i32 },
    MouseMove { x: i32, y: i32 },
    MouseLeave,
    Wheel { x: i32, y: i32, delta: i32, ctrl: bool },
    /// 输入了一个字符。已经过 IME 组合，可直接插入。
    Char(char),
    /// 按下了非字符键。参数是虚拟键码（`VK_*`）。
    ///
    /// 与 `Char` 分开是必要的：就地编辑既要收字符（插入），
    /// 也要收功能键（退格、方向键、回车、Esc），两者来源的消息不同。
    KeyDown(u32),
    /// 用户把文件拖进了窗口。路径已经是**普通文件系统路径**。
    FilesDropped { paths: Vec<std::path::PathBuf> },
    /// OLE 拖放完成（`platform::drop_target`）。
    ///
    /// 与 `FilesDropped` 分开是因为**落盘已经在那一边做完了**：
    /// 壳命名空间项（此电脑、回收站）没有文件路径可传，只能先就地写成 `.lnk`
    /// 再通知这里重新扫描。所以这个事件不带数据。
    ExternalDrop,
    /// 托盘图标被左键单击：切换窗口显示。
    TrayLeftClick,
    /// 托盘图标被右键单击：弹托盘菜单。
    TrayRightClick,
    /// 全局热键被按下。
    Hotkey,
    /// 窗口失去了激活状态（用户点了别的程序，或者点了桌面）。
    ///
    /// 轻量浮层（右键菜单）必须靠它来关闭：点在自己窗口之外时，
    /// 消息根本不会送到我们这里，靠"点外部关闭"的判断是永远等不到的。
    FocusLost,
    Destroyed,
}

/// 用户点关闭按钮后的去向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    /// 退出程序。
    Exit,
    /// 只隐藏窗口（"关闭到托盘"）。
    ///
    /// 托盘图标接入前不会构造这个值：还没有办法把窗口唤回来，
    /// 现在允许隐藏就等于让用户关不掉程序。托盘落地后启用。
    #[allow(dead_code)]
    Hide,
}

/// 上层需要实现的窗口回调。
///
/// 所有方法都在 UI 线程上调用，实现里不必考虑同步。
///
/// `'static` 约束是刻意加的：窗口上下文把 handler 存成裸指针放在 `GWLP_USERDATA` 里，
/// 而 trait 对象默认携带 `'static` 绑定。要求实现者自持全部数据
/// （而不是借用外部变量）是这里最省事也最不容易出错的做法。
pub trait Handler: 'static {
    /// 处理事件，返回是否需要重绘。
    fn on_event(&mut self, event: Event) -> bool;

    /// 当前的命中测试参数。
    ///
    /// **在 `WM_NCHITTEST` 里被同步调用**，所以实现要足够快：
    /// 直接返回缓存的布局，不要现场重新计算整棵布局树。
    fn hit_test(&self) -> HitTest;

    /// 最小客户区尺寸（逻辑像素）。
    fn min_size(&self) -> (i32, i32);

    /// 执行一次绘制。窗口层不关心画什么。
    fn paint(&mut self);

    /// 用户点了关闭按钮。默认退出程序。
    fn on_close_request(&mut self) -> CloseAction {
        CloseAction::Exit
    }

    /// 是否需要托盘图标。
    ///
    /// 窗口层在**资源管理器重启**时要用它决定要不要重挂图标
    ///（见 `wnd_proc` 里 `TaskbarCreated` 分支）。
    fn wants_tray(&self) -> bool {
        false
    }

    /// 窗口即将销毁，用于保存状态。
    fn on_destroyed(&mut self) {}
}

/// 每个窗口一份的上下文，指针存放在窗口的 `GWLP_USERDATA` 里。
struct WindowCtx {
    /// 由 [`Window::run`] 设置、返回前清空。为空表示尚未进入消息循环。
    ///
    /// 用裸指针是因为 Win32 回调不能带闭包捕获，而 trait 对象又要保持动态分发；
    /// 安全性由 [`Window::run`] 的"设置—使用—清空"配对保证。
    handler: Option<*mut dyn Handler>,
    dirty: bool,
    /// 客户区尺寸（物理像素），`WM_SIZE` 时更新。
    ///
    /// 缓存它是因为命中测试在鼠标移动时高频触发，
    /// 不该每次都去 syscall 问系统要一次 `GetClientRect`。
    client: (i32, i32),
    dpi: u32,
    /// 是否已向系统登记 `WM_MOUSELEAVE` 跟踪。
    tracking_mouse: bool,
}

impl WindowCtx {
    fn new(cx: i32, cy: i32) -> Self {
        Self {
            handler: None,
            dirty: true,
            client: (cx, cy),
            dpi: 96,
            tracking_mouse: false,
        }
    }
}

/// 窗口几何。`x` / `y` 为 `None` 表示没记录过位置。
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub width: i32,
    pub height: i32,
    pub x: Option<i32>,
    pub y: Option<i32>,
}

/// 可以跨线程使用的"请求重绘"凭据。
///
/// ## 为什么需要单独一个类型
///
/// [`Window`] 内含 `HWND`，而 `HWND` 本质是 `*mut c_void` —— 裸指针不是 `Send`，
/// 所以 `Window` 无法直接搬进后台线程（编译器会拒绝）。
///
/// 但 `PostMessageW` 本身**是线程安全的**：系统保证可以把消息投递到任意线程
/// 创建的窗口。所以这里只搬运句柄的数值，并显式声明它可以跨线程。
/// 这个 `unsafe impl` 的确切依据就是文档里那句"PostMessage 可从任意线程调用"。
#[derive(Clone, Copy)]
pub struct RedrawNotifier(isize);

// SAFETY: 见类型文档 —— 该类型唯一的用途是调用线程安全的 PostMessageW，
// 且不含任何所有权或生命周期信息。
unsafe impl Send for RedrawNotifier {}

impl RedrawNotifier {
    /// 请求目标窗口重绘一次。
    pub fn request_redraw(&self) {
        let hwnd = HWND(self.0 as *mut core::ffi::c_void);
        // SAFETY: 句柄来自一个当时有效的窗口。窗口若已销毁，PostMessage 只是失败，
        // 不会造成未定义行为。
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_REDRAW, WPARAM(0), LPARAM(0));
        }
    }
}

/// 一个无边框窗口。
///
/// `Copy` 是刻意的：它只是 `HWND` 的一层包装，复制不产生任何系统资源。
/// 这一点很实用 —— `App` 既持有窗口又实现了 [`Handler`]，
/// 而 `self.window.run(&mut self)` 会因为同时借用 `self` 的字段与整体而报错。
/// 先复制一份句柄到局部变量（`let window = self.window;`）就绕开了，
/// 比把窗口拆出去再传进来干净得多。
#[derive(Clone, Copy)]
pub struct Window {
    hwnd: HWND,
}

// 窗口 API 里有几个方法（hide / is_visible / toggle_visible / set_topmost /
// set_title / request_redraw）要等托盘、全局热键、异步图标加载接入后才有调用方。
// 它们构成一套完整的窗口能力，先删掉再回加反而更容易出错。
// 上述功能落地后请删掉下面这行，让死代码告警恢复。
#[allow(dead_code)]
impl Window {
    /// 创建窗口。窗口尺寸即客户区尺寸（非客户区已被去掉）。
    pub fn create(title: &str, geometry: Geometry, always_on_top: bool) -> Result<Self> {
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None) }.ctx("GetModuleHandleW")?.0);

        // WS_THICKFRAME 不是为了显示边框（WM_NCCALCSIZE 已经把它去掉），
        // 而是为了拿到 DWM 阴影、系统级贴靠与 Snap。
        // WS_EX_NOREDIRECTIONBITMAP 是"无闪烁"的地基：没有重定向位图，
        // GDI 就没有可擦可画的表面，从根上消除了擦背景导致的闪烁。
        let style = WS_POPUP | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_CLIPCHILDREN;
        let mut ex_style = WS_EX_NOREDIRECTIONBITMAP | WS_EX_APPWINDOW;
        if always_on_top {
            ex_style |= WS_EX_TOPMOST;
        }

        register_class(instance)?;

        let width = geometry.width.max(1);
        let height = geometry.height.max(1);
        let (x, y) = match (geometry.x, geometry.y) {
            (Some(x), Some(y)) => (x, y),
            // 没记录过位置就居中，比 CW_USEDEFAULT 更符合"自管窗口"的预期
            _ => {
                let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
                let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
                ((sw - width) / 2, (sh - height) / 2)
            }
        };

        let title_w = to_wide(title);

        // SAFETY: 类已注册；标题缓冲活到调用结束；无父窗口、无菜单。
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                w!("QuickSpinWindow"),
                PCWSTR(title_w.as_ptr()),
                style,
                x,
                y,
                width,
                height,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .ctx("CreateWindowExW")?;

        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);

        // 允许把文件拖进窗口。需求是"所有界面支持拖入快捷方式"。
        // 必须显式开一次；不开的话系统根本不发 WM_DROPFILES。
        unsafe { DragAcceptFiles(hwnd, true) };

        // 再注册 OLE 拖放目标，用来接收**壳命名空间项**（此电脑、回收站…）。
        // 两套并存不冲突：WM_DROPFILES 只能收文件路径，OLE 那条收 PIDL。
        // 失败只记日志 —— 拖入壳项用不了，不该影响程序其余功能。
        if let Err(e) = super::drop_target::install(hwnd) {
            eprintln!("注册拖放目标失败（拖入「此电脑」等将不可用）: {e}");
        }

        let _ = with_ctx(hwnd, |ctx| {
            ctx.dpi = dpi;
            // 窗口尺寸等于客户区尺寸，直接采用请求值，
            // 不必走 AdjustWindowRectEx —— 非客户区宽度在我们这里是 0。
            ctx.client = (width, height);
        });

        Ok(Self { hwnd })
    }

    /// 是否捕获鼠标。
    ///
    /// 右键菜单展开时打开：捕获之后，**即使鼠标在自己的窗口之外**，
    /// 按下鼠标的消息也会送到我们这里，于是"点别处关闭菜单"才成立。
    /// 不捕获的话，点桌面那一下我们根本收不到，菜单会一直挂着。
    pub fn set_capture(&self, on: bool) {
        // SAFETY: 句柄来自一个当时有效的窗口；捕获是窗口级状态，无内存影响。
        unsafe {
            if on {
                SetCapture(self.hwnd);
            } else {
                let _ = ReleaseCapture();
            }
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// 当前 DPI。
    pub fn dpi(&self) -> u32 {
        unsafe { GetDpiForWindow(self.hwnd) }.max(96)
    }

    /// DPI 缩放系数，96 DPI 时为 1.0。
    pub fn scale(&self) -> f32 {
        self.dpi() as f32 / 96.0
    }

    /// 客户区尺寸（物理像素）。
    pub fn client_size(&self) -> (i32, i32) {
        with_ctx(self.hwnd, |ctx| ctx.client).unwrap_or((0, 0))
    }

    pub fn is_maximized(&self) -> bool {
        unsafe { IsZoomed(self.hwnd).as_bool() }
    }

    pub fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    pub fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn minimize(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
        }
    }

    /// 请求关闭。走正常的 `WM_CLOSE` 流程，因此
    /// [`Handler::on_close_request`] 仍有机会选择"隐藏而不是退出"。
    pub fn close(&self) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    /// 显示 / 隐藏切换，供托盘与全局热键使用。
    pub fn toggle_visible(&self) {
        if self.is_visible() {
            self.hide();
        } else {
            self.show();
        }
    }

    pub fn set_topmost(&self, on: bool) {
        let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(after),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    pub fn set_title(&self, title: &str) {
        let wide = to_wide(title);
        unsafe {
            let _ = SetWindowTextW(self.hwnd, PCWSTR(wide.as_ptr()));
        }
    }

    /// 取一个可跨线程使用的重绘凭据。后台任务完成后用它唤醒 UI 线程。
    pub fn redraw_notifier(&self) -> RedrawNotifier {
        RedrawNotifier(self.hwnd.0 as isize)
    }

    /// 请求重绘一次。**可以从任意线程调用**：内部走 `PostMessage`，
    /// 异步任务（图标线程、文件监视线程）完成后用它通知 UI。
    pub fn request_redraw(&self) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_APP_REDRAW, WPARAM(0), LPARAM(0));
        }
    }

    /// 当前窗口几何，用于写入状态文件。
    pub fn geometry(&self) -> Geometry {
        let mut rect = RECT::default();
        let _ = unsafe { GetWindowRect(self.hwnd, &mut rect) };
        let (width, height) = self.client_size();
        Geometry {
            width,
            height,
            x: Some(rect.left),
            y: Some(rect.top),
        }
    }

    /// 跑消息循环，直到窗口关闭。
    ///
    /// 返回后 `handler` 可以安全地继续使用 —— 本方法会先断开窗口对它的引用。
    pub fn run(&self, handler: &mut dyn Handler) -> Result<()> {
        let ptr: *mut dyn Handler = handler;
        let _ = with_ctx(self.hwnd, |ctx| ctx.handler = Some(ptr));

        // 进阻塞循环前先画一帧。否则第一帧要等某个消息到来才发生，
        // 表现为"窗口出现后内容空白一小会儿"。
        if take_dirty(self.hwnd) {
            with_handler(self.hwnd, |h| h.paint());
        }

        let result = message_loop(self.hwnd);

        // 窗口若还活着就断开指针，避免留下悬垂引用。
        // 窗口已销毁时 WindowCtx 已在 WM_NCDESTROY 里释放，不能再碰。
        if unsafe { IsWindow(Some(self.hwnd)).as_bool() } {
            let _ = with_ctx(self.hwnd, |ctx| ctx.handler = None);
        }
        result
    }
}

/// 消息循环。
///
/// 关键设计：**先排空消息队列，再统一重绘一次**。
/// 缩放窗口时系统会连发多条 `WM_SIZE`，逐条重绘会白白多画好几遍，
/// 看起来就是拖边框时画面在抖。
fn message_loop(hwnd: HWND) -> Result<()> {
    let mut msg = MSG::default();

    loop {
        // 阻塞等待：空闲时线程真的在睡，CPU 占用为 0
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 <= 0 {
            return if ret.0 < 0 {
                Err(AppError::Win32 {
                    context: "GetMessageW 返回错误",
                    code: ret.0,
                })
            } else {
                Ok(()) // WM_QUIT
            };
        }

        dispatch(&msg);

        // 把积压的消息一次处理完，最后只重绘一次
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() } {
            if msg.message == WM_QUIT {
                return Ok(());
            }
            dispatch(&msg);
        }

        if take_dirty(hwnd) {
            with_handler(hwnd, |h| h.paint());
        }
    }
}

fn dispatch(msg: &MSG) {
    unsafe {
        let _ = TranslateMessage(msg);
        DispatchMessageW(msg);
    }
}

/// 窗口过程。
///
/// 大部分消息在这里就地翻译成 [`Event`] 交给上层；
/// 只有必须**同步**给出答案的消息（命中测试、最小尺寸、不擦背景）才自己处理，
/// 因为系统等不了主循环走完一圈。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // 建立上下文。放在 NCCREATE 而不是 CREATE，是因为此消息到达时
        // 窗口已存在但还未显示，是挂用户数据的标准时机。
        WM_NCCREATE => {
            let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            let ctx = Box::into_raw(Box::new(WindowCtx::new(cs.cx, cs.cy)));
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx as isize);
            }
            return LRESULT(1); // 返回 0 会中止窗口创建
        }

        // 去掉标准非客户区，让客户区覆盖整个窗口。
        // 保留 WS_THICKFRAME 是为了 DWM 阴影与系统贴靠；
        // 边框本身改由 WM_NCHITTEST 用命中测试补回来。
        WM_NCCALCSIZE if wparam.0 != 0 => return LRESULT(0),

        // 不擦背景（反闪烁铁律 3）。
        // 默认处理会先用背景刷把客户区填一遍再交给我们画，肉眼就是闪一帧。
        WM_ERASEBKGND => return LRESULT(1),

        // 绘制在脏标记驱动下进行，但**必须在这里真正画出来**。
        //
        // 原因：拖动边框时 Windows 会进入模态尺寸循环，该循环是从
        // `DefWindowProcW(WM_NCLBUTTONDOWN)` 内部启动的 —— 也就是说
        // 我们自己的消息循环此刻正阻塞在 `DispatchMessageW` 里。
        // 若只在那个循环里 paint，整段拖动过程都不会重绘，
        // 画面会停在旧尺寸上（现象："窗口看着移动了却没变大"，
        // 松开鼠标才更新；缩小时描边被裁掉）。
        //
        // 所以这里在 ValidateRect 之后立即画一次。`take_dirty` 会消费脏标记，
        // 因此不会与消息循环里的那次绘制重复。
        WM_PAINT => {
            unsafe {
                let _ = ValidateRect(Some(hwnd), None);
            }
            if take_dirty(hwnd) {
                with_handler(hwnd, |h| h.paint());
            }
            return LRESULT(0);
        }

        WM_NCHITTEST => {
            let mut pt = POINT {
                x: x_from_lparam(lparam),
                y: y_from_lparam(lparam),
            };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut pt);
            }

            let hit = with_ctx(hwnd, |ctx| {
                let (width, height) = ctx.client;
                let cfg = ctx
                    .handler
                    .map(|p| unsafe { (*p).hit_test() })
                    .unwrap_or_default();
                hittest::hit_test(&cfg, pt.x, pt.y, width, height, ctx.dpi)
            });

            return LRESULT(hit.unwrap_or(hittest::Hit::Client).as_nchittest());
        }

        // 最小尺寸限制。默认值给一个能放下标题栏与侧栏的尺寸。
        WM_GETMINMAXINFO => {
            let min = with_handler(hwnd, |h| h.min_size()).unwrap_or((360, 260));
            let scale = with_ctx(hwnd, |ctx| ctx.dpi).unwrap_or(96) as f32 / 96.0;
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            // 注意：这里的尺寸是**窗口**尺寸，而我们非客户区为 0，等同客户区
            info.ptMinTrackSize.x = (min.0 as f32 * scale).round() as i32;
            info.ptMinTrackSize.y = (min.1 as f32 * scale).round() as i32;
            return LRESULT(0);
        }

        WM_SIZE => {
            let width = x_from_lparam(lparam).max(0);
            let height = y_from_lparam(lparam).max(0);
            let _ = with_ctx(hwnd, |ctx| ctx.client = (width, height));

            emit(
                hwnd,
                Event::Resized {
                    width: width as u32,
                    height: height as u32,
                },
            );
            // 尺寸变了必须重绘：渲染器要跟着重建交换链缓冲
            mark_dirty(hwnd);
            // 主动请求一次 WM_PAINT。
            //
            // 这不是多余的：模态尺寸循环期间我们的消息循环被阻塞，
            // 只有 WM_PAINT 能触发绘制（见该分支的说明）。
            // 不 Invalidate 的话系统未必会生成 WM_PAINT，拖动时画面就不更新。
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            return LRESULT(0);
        }

        WM_DPICHANGED => {
            let dpi = (wparam.0 & 0xFFFF) as u32;
            let suggested = unsafe { &*(lparam.0 as *const RECT) };

            // 按系统建议的矩形调整窗口，不要自己算缩放 —— 多显示器混合 DPI
            // 场景下自己算几乎一定会偏。
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }

            let _ = with_ctx(hwnd, |ctx| ctx.dpi = dpi);
            emit(hwnd, Event::DpiChanged { dpi });
            mark_dirty(hwnd);
            return LRESULT(0);
        }

        WM_ENTERSIZEMOVE => {
            emit(hwnd, Event::SizeMove { entering: true });
            return LRESULT(0);
        }
        WM_EXITSIZEMOVE => {
            emit(hwnd, Event::SizeMove { entering: false });
            mark_dirty(hwnd);
            return LRESULT(0);
        }

        WM_RBUTTONDOWN => {
            emit(
                hwnd,
                Event::RightMouseDown {
                    x: x_from_lparam(lparam),
                    y: y_from_lparam(lparam),
                },
            );
            mark_dirty(hwnd);
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            emit(
                hwnd,
                Event::MouseDown {
                    x: x_from_lparam(lparam),
                    y: y_from_lparam(lparam),
                },
            );
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            emit(
                hwnd,
                Event::MouseUp {
                    x: x_from_lparam(lparam),
                    y: y_from_lparam(lparam),
                    inside: true,
                },
            );
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            // Win32 只在鼠标离开时通知一次，需要每次移动时续订
            track_mouse_leave(hwnd);
            emit(
                hwnd,
                Event::MouseMove {
                    x: x_from_lparam(lparam),
                    y: y_from_lparam(lparam),
                },
            );
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            let _ = with_ctx(hwnd, |ctx| ctx.tracking_mouse = false);
            emit(hwnd, Event::MouseLeave);
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            // 高 16 位是有符号的滚轮增量（一格 = 120）
            let delta = ((wparam.0 >> 16) as u16 as i16) as i32;
            // 低 16 位是按键状态，MK_CONTROL = 0x0008
            let ctrl = (wparam.0 & 0x0008) != 0;

            // 注意：WM_MOUSEWHEEL 的坐标是**屏幕**坐标，与其它鼠标消息不同
            let mut pt = POINT {
                x: x_from_lparam(lparam),
                y: y_from_lparam(lparam),
            };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut pt);
            }

            emit(
                hwnd,
                Event::Wheel {
                    x: pt.x,
                    y: pt.y,
                    delta,
                    ctrl,
                },
            );
            return LRESULT(0);
        }

        // 异步任务唤醒：只标脏，具体的绘制交给消息循环
        // 键盘输入。
        //
        // 生成 `WM_CHAR` 的活由 `TranslateMessage` 干了（在消息循环里），
        // 这里只负责把结果取出来。
        WM_CHAR => {
            // wParam 是 UTF-16 码元。用 `char::from_u32` 而不是直接 `as char`：
            // 强转会把**孤立代理项**（0xD800..0xDFFF）变成非法 char，
            // 而在 Rust 里构造非法 char 是未定义行为。
            if let Some(c) = char::from_u32(wparam.0 as u32) {
                emit(hwnd, Event::Char(c));
                mark_dirty(hwnd);
            }
            // 不往下传：交给 `DefWindowProcW` 会触发系统的提示音
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            emit(hwnd, Event::KeyDown(wparam.0 as u32));
            mark_dirty(hwnd);
            // 同样不往下传，避免系统对未处理的键发出"哔"声
            return LRESULT(0);
        }
        // 系统键（Alt 组合）也要收，否则那条输入路径会漏
        WM_SYSKEYDOWN => {
            emit(hwnd, Event::KeyDown(wparam.0 as u32));
            mark_dirty(hwnd);
        }

        // 拖放进来的**文件**。
        //
        // 这条路走老式的 `WM_DROPFILES`：对纯文件拖放它足够，且不需要 OLE。
        // 但它**只能收到文件系统路径**，收不到壳命名空间项（此电脑、回收站）——
        // 那些走下面注册的 OLE 拖放目标（见 `platform::drop_target`）。
        WM_DROPFILES => {
            let hdrop = HDROP(wparam.0 as *mut core::ffi::c_void);
            let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
            let mut paths = Vec::with_capacity(count as usize);
            for i in 0..count {
                // 先问长度再取内容，避免定长缓冲把长路径截断
                let len = unsafe { DragQueryFileW(hdrop, i, None) };
                if len == 0 {
                    continue;
                }
                let mut buf = vec![0u16; len as usize + 1];
                let written = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) };
                if written == 0 {
                    continue;
                }
                buf.truncate(written as usize);
                paths.push(std::path::PathBuf::from(String::from_utf16_lossy(&buf)));
            }
            // 必须释放，否则每次拖放泄漏一块 shell 分配的全局内存
            unsafe { DragFinish(hdrop) };

            if !paths.is_empty() {
                emit(hwnd, Event::FilesDropped { paths });
                mark_dirty(hwnd);
            }
            return LRESULT(0);
        }

        // OLE 拖放已完成（可能是壳命名空间项，也可能是文件）。
        //
        // 落盘已经在 `drop_target` 里做完了，这里只负责通知上层"重新扫描"。
        // 拆成"那边落盘 + 这边收消息"是因为消息会回到正常的处理时机，
        // 不会在 OLE 的调用栈里重入界面逻辑。
        crate::platform::drop_target::WM_DROP_COMPLETED => {            emit(hwnd, Event::ExternalDrop);
            mark_dirty(hwnd);
            return LRESULT(0);
        }

        // ---- 托盘 ----
        //
        // 托盘回调消息：lParam 低字是鼠标消息。
        crate::platform::tray::CALLBACK_MESSAGE => {
            let mouse = crate::platform::tray::mouse_event(lparam);
            if crate::platform::tray::is_left_click(mouse) {
                emit(hwnd, Event::TrayLeftClick);
            } else if crate::platform::tray::is_right_click(mouse) {
                emit(hwnd, Event::TrayRightClick);
            }
            return LRESULT(0);
        }

        // 全局热键被按下。
        crate::platform::hotkey::WM_HOTKEY_MSG => {
            emit(hwnd, Event::Hotkey);
            return LRESULT(0);
        }

        // 第二个实例启动时请求我们显示出来。
        crate::platform::single_instance::SHOW_MESSAGE => {
            crate::platform::single_instance::reveal(hwnd);
            mark_dirty(hwnd);
            return LRESULT(0);
        }

        // 资源管理器重启后要重新挂上托盘图标。
        //
        // 这个消息的编号是**运行时注册**的，所以只能在这里现算现比，
        // 不能写成常量分支。
        //
        // 不处理的话，用户重启一次 Explorer 就永远看不到托盘图标了 ——
        // 而且没有任何提示，看起来像"程序坏了"。
        msg if msg == crate::platform::tray::taskbar_created_message() => {
            if with_handler(hwnd, |h| h.wants_tray()).unwrap_or(false) {
                if let Err(e) = crate::platform::tray::install(hwnd) {
                    eprintln!("资源管理器重启后重挂托盘图标失败: {e}");
                }
            }
            return LRESULT(0);
        }

        // 失去激活 = 用户点了别的地方（桌面、别的程序）。
        // 浮层必须在这里收起来 —— 见 `Event::FocusLost` 的说明。
        WM_ACTIVATE => {
            // wParam 低字是 WA_INACTIVE(0) 时表示变成非活动
            if (wparam.0 & 0xFFFF) == 0 {
                emit(hwnd, Event::FocusLost);
                mark_dirty(hwnd);
            }
            return LRESULT(0);
        }
        WM_APP_REDRAW => {
            mark_dirty(hwnd);
            return LRESULT(0);
        }

        WM_CLOSE => {
            let action = with_handler(hwnd, |h| h.on_close_request()).unwrap_or(CloseAction::Exit);
            match action {
                CloseAction::Hide => unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                },
                CloseAction::Exit => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
            }
            return LRESULT(0);
        }

        WM_DESTROY => {
            // 摘掉托盘图标 —— **漏掉这一步会留下幽灵图标**：
            // 托盘里一个点不动的死图标，直到鼠标划过才消失。
            crate::platform::tray::remove(hwnd);
            with_handler(hwnd, |h| h.on_destroyed());
            emit(hwnd, Event::Destroyed);
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }

        // 窗口彻底销毁：释放上下文。
        WM_NCDESTROY => {
            let ptr = ctx_ptr(hwnd);
            if !ptr.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(ptr));
                }
            }
            // 继续走默认处理完成系统侧的清理
        }

        _ => {}
    }

    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 注册窗口类。类只需注册一次，重复注册会返回 `ERROR_CLASS_ALREADY_EXISTS`。
fn register_class(instance: HINSTANCE) -> Result<()> {
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.ctx("LoadCursorW")?;

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        // 不设 CS_OWNDC 等：我们完全不依赖 GDI
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: HICON::default(),
        hCursor: cursor,
        // 不给背景刷：窗口没有任何 GDI 表面（WS_EX_NOREDIRECTIONBITMAP）
        hbrBackground: HBRUSH::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: w!("QuickSpinWindow"),
        hIconSm: HICON::default(),
    };

    if unsafe { RegisterClassExW(&wc) } != 0 {
        return Ok(());
    }

    let err = unsafe { GetLastError() };
    if err == ERROR_CLASS_ALREADY_EXISTS {
        Ok(())
    } else {
        Err(AppError::Win32 {
            context: "RegisterClassExW",
            code: err.0 as i32,
        })
    }
}

/// 续订 `WM_MOUSELEAVE` 通知。
fn track_mouse_leave(hwnd: HWND) {
    let need = with_ctx(hwnd, |ctx| {
        if ctx.tracking_mouse {
            false
        } else {
            ctx.tracking_mouse = true;
            true
        }
    })
    .unwrap_or(false);

    if !need {
        return;
    }

    let mut tme = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    unsafe {
        let _ = TrackMouseEvent(&mut tme);
    }
}

// ---- 上下文与事件分发 ----

fn ctx_ptr(hwnd: HWND) -> *mut WindowCtx {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowCtx }
}

fn with_ctx<R>(hwnd: HWND, f: impl FnOnce(&mut WindowCtx) -> R) -> Option<R> {
    let ptr = ctx_ptr(hwnd);
    if ptr.is_null() {
        None
    } else {
        // SAFETY: 指针由 WM_NCCREATE 建立、WM_NCDESTROY 释放，
        // 期间只有 UI 线程访问，不会出现并发别名。
        Some(unsafe { f(&mut *ptr) })
    }
}

fn with_handler<R>(hwnd: HWND, f: impl FnOnce(&mut dyn Handler) -> R) -> Option<R> {
    let handler = with_ctx(hwnd, |ctx| ctx.handler).flatten();
    // SAFETY: 指针由 Window::run 设置并在返回前清空，期间调用方持有的
    // handler 一直活着；with_ctx 的借用已在此前释放，不存在可变别名。
    handler.map(|ptr| unsafe { f(&mut *ptr) })
}

/// 把事件交给上层，并按需标脏。
fn emit(hwnd: HWND, event: Event) {
    if with_handler(hwnd, |h| h.on_event(event)).unwrap_or(false) {
        mark_dirty(hwnd);
    }
}

/// 切换桌面图标的显示 / 隐藏。
///
/// # 实现方式
///
/// 给桌面的视图窗口（`SHELLDLL_DefView`）发 `WM_COMMAND`，命令号 `0x7402`。
/// 这正是资源管理器"查看 → 显示桌面图标"那一条菜单项背后的命令，
/// 由 shell 自己维护状态并**写回注册表**（`Advanced\HideIcons`），
/// 所以调用一次就够了，不需要我们额外去改注册表或刷新 shell。
///
/// # 为什么不能只改注册表
///
/// 直接写 `HideIcons` 之后 shell 不会立刻重读，得再广播设置变更通知，
/// 何时生效不确定。发命令是同步生效的。
///
/// 命令号是未公开的约定，但它是"显示桌面图标"这个功能二十多年来的实现，
/// 传错最坏是没反应，不会造成破坏。
pub fn toggle_desktop_icons() {
    /// 桌面图标视图的窗口类名
    const DEFVIEW_CLASS: &str = "SHELLDLL_DefView";
    /// 切换"显示桌面图标"
    const TOGGLE_ICONS: usize = 0x7402;

    let Some(defview) = desktop_view_window(DEFVIEW_CLASS) else {
        eprintln!("找不到桌面视图窗口，无法切换桌面图标显示");
        return;
    };

    // SAFETY: 只向别的进程窗口投递消息，不涉及我们自己的状态。
    unsafe {
        // PostMessage 而不是 SendMessage：那是别的进程的窗口，
        // SendMessage 会阻塞等它处理完，而它可能正忙（正在重画桌面）。
        let _ = PostMessageW(Some(defview), WM_COMMAND, WPARAM(TOGGLE_ICONS), LPARAM(0));
    }
}

/// 找到承载桌面图标的 `SHELLDLL_DefView` 窗口。
///
/// **要在两处找**：正常情况下它是 `Progman` 的子窗口；但启用了壁纸轮换、
/// 或者多显示器 / 第三方桌面工具接管时，它会挂在某个 `WorkerW` 下面。
/// 只找 `Progman` 会在那些机器上失败。
fn desktop_view_window(class: &str) -> Option<HWND> {
    let class = class_name_wide(class);
    let class = PCWSTR(class.as_ptr());
    let progman_name = class_name_wide("Progman");
    let workerw_name = class_name_wide("WorkerW");

    // SAFETY: 全部是窗口枚举查询，不改动任何状态。
    unsafe {
        // 先在 Progman 下找
        if let Ok(progman) = FindWindowW(PCWSTR(progman_name.as_ptr()), PCWSTR::null()) {
            if !progman.is_invalid() {
                if let Ok(view) = FindWindowExW(Some(progman), None, class, PCWSTR::null()) {
                    if !view.is_invalid() {
                        return Some(view);
                    }
                }
            }
        }

        // 回退：逐个顶层 WorkerW 找它的子窗口
        let mut worker = HWND::default();
        loop {
            let Ok(next) = FindWindowExW(
                None,
                Some(worker),
                PCWSTR(workerw_name.as_ptr()),
                PCWSTR::null(),
            ) else {
                return None;
            };
            if next.is_invalid() {
                return None;
            }
            if let Ok(view) = FindWindowExW(Some(next), None, class, PCWSTR::null()) {
                if !view.is_invalid() {
                    return Some(view);
                }
            }
            worker = next;
        }
    }
}

/// 把类名转成带结尾 NUL 的宽字符串。
fn class_name_wide(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(std::iter::once(0)).collect()
}

fn mark_dirty(hwnd: HWND) {
    let _ = with_ctx(hwnd, |ctx| ctx.dirty = true);
}

fn take_dirty(hwnd: HWND) -> bool {
    // 取出并清零：一次 dirty 只画一帧
    with_ctx(hwnd, |ctx| std::mem::replace(&mut ctx.dirty, false)).unwrap_or(false)
}

// ---- 小工具 ----

/// 从 `LPARAM` 取有符号 X（低 16 位）。Win32 的坐标按 signed short 传递。
fn x_from_lparam(lparam: LPARAM) -> i32 {
    (lparam.0 as u32 as u16 as i16) as i32
}

/// 从 `LPARAM` 取有符号 Y（高 16 位）。
fn y_from_lparam(lparam: LPARAM) -> i32 {
    ((lparam.0 as u32 >> 16) as u16 as i16) as i32
}

/// 转成 NUL 结尾的 UTF-16。
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
