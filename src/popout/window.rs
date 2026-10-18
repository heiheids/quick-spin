//! 弹出子窗口的窗口本体。
//!
//! 每个弹出窗口持有一整套自己的东西：`Window`（HWND）、`Renderer`（交换链）、
//! `PopoutPainter`、`TextEngine`、布局与配色。**不借用主窗口的任何运行时状态**
//!（见 `mod.rs` 的选择 A），所以两层之间没有 `RefCell`、没有共享可变状态，
//! 也没有 `&mut` 别名问题。

use crate::config::theme::Scheme;
use crate::error::Result;
use crate::platform::hittest::HitTest;
use crate::platform::window::{CloseAction, Event, Geometry, Handler, Window};
use crate::popout::layout::{PopoutLayout, TitleButton};
use crate::popout::painter::PopoutPainter;
use crate::render::composer::Composer;
use crate::render::device::Renderer;
use crate::render::TextEngine;

/// 一个活着的弹出子窗口。
pub struct Popout {
    window: Window,
    renderer: Renderer,
    composer: Composer,
    painter: PopoutPainter,
    text: TextEngine,
    layout: PopoutLayout,
    scheme: Scheme,
    /// 标题（用户自命名，或退回选项卡名）
    title: String,
    /// 当前悬停的标题栏按钮
    hover: Option<TitleButton>,
    /// 是否正在拖动窗口尺寸
    resizing: bool,
}

impl Popout {
    /// 创建一个弹出子窗口。
    pub fn create(
        title: &str,
        geometry: Geometry,
        scheme: Scheme,
        icon_size: i32,
    ) -> Result<Self> {
        let window = Window::create(title, geometry, false)?;
        let (cw, ch) = window.client_size();
        let dpi = window.dpi();
        let (renderer, composer) =
            Renderer::new(window.hwnd(), cw.max(1) as u32, ch.max(1) as u32, dpi as f32)?;

        Ok(Self {
            window,
            renderer,
            composer,
            painter: PopoutPainter::new(),
            text: TextEngine::new()?,
            layout: PopoutLayout::compute(cw, ch, icon_size),
            scheme,
            title: title.to_string(),
            hover: None,
            resizing: false,
        })
    }

    /// 这几个是为**吸附跟随 / 保存位置**准备的（下一步）：
    /// 主窗口移动时要读它的几何并反过来摆它，关闭时要把几何写回状态。
    /// 现在还没有调用方，所以先显式允许死代码 —— 接线后请删掉这一行。
    #[allow(dead_code)]
    pub fn hwnd(&self) -> windows::Win32::Foundation::HWND {
        self.window.hwnd()
    }

    /// 窗口位置与尺寸（屏幕物理像素）。
    #[allow(dead_code)]
    pub fn geometry(&self) -> Geometry {
        self.window.geometry()
    }

    /// 登记到消息循环（**不进循环**：一个线程只有一个循环）。
    ///
    /// 先把裸指针拿出来再调用，避开"同时借用 `self.window` 与 `self`"。
    pub fn attach(&mut self) {
        let ptr: *mut dyn Handler = self;
        self.window.attach_ptr(ptr);
    }

    /// 请求关闭。
    pub fn close(&self) {
        self.window.close();
    }

    /// 显示窗口。
    ///
    /// **必须显式调用**：`Window::create` 只创建不显示（`WS_POPUP` 里没有
    /// `WS_VISIBLE`），主窗口那边也是在创建后自己调 `show()`。
    /// 漏掉这一步的表现是"点了菜单什么都没出现"，而不是报错。
    pub fn show(&self) {
        self.window.show();
    }

    /// 移动/改变尺寸（吸附跟随、拖动脱离都用它）。
    #[allow(dead_code)]
    pub fn set_geometry(&self, g: Geometry) {
        self.window.set_geometry(g);
    }

    fn to_logical(&self, x: i32, y: i32) -> (i32, i32) {
        let s = self.window.scale();
        if s <= 0.0 {
            (x, y)
        } else {
            ((x as f32 / s) as i32, (y as f32 / s) as i32)
        }
    }
}

impl Handler for Popout {
    fn on_event(&mut self, event: Event) -> bool {
        match event {
            Event::Resized { width, height } => {
                let dpi = self.window.dpi();
                // 交换链与布局必须跟着一起变，否则画面会被拉伸。
                if self
                    .renderer
                    .resize(width.max(1), height.max(1), dpi as f32)
                    .is_err()
                {
                    return false;
                }
                let scale = self.window.scale();
                let (lw, lh) = (
                    (width as f32 / scale) as i32,
                    (height as f32 / scale) as i32,
                );
                // 图标尺寸沿用当前布局的方格边长，避免尺寸一变方格就跳。
                self.layout = PopoutLayout::compute(lw, lh, self.layout.tile);
                true
            }
            Event::SizeMove { entering } => {
                self.resizing = entering;
                true
            }
            Event::MouseMove { x, y } => {
                let (lx, ly) = self.to_logical(x, y);
                let h = self.layout.button_at(lx, ly);
                if h != self.hover {
                    self.hover = h;
                    return true;
                }
                false
            }
            Event::MouseLeave => {
                if self.hover.is_some() {
                    self.hover = None;
                    return true;
                }
                false
            }
            Event::MouseDown { x, y } => {
                let (lx, ly) = self.to_logical(x, y);
                match self.layout.button_at(lx, ly) {
                    // 关闭：`WM_CLOSE` 走 `on_close_request`。
                    // 本窗口不是主窗口，所以销毁它**不会**结束消息循环
                    //（见 `window.rs` 的 WM_DESTROY）。
                    Some(TitleButton::Close) => {
                        self.window.close();
                        false
                    }
                    // 其余三个按钮（改色 / 吸附 / 钉桌面）是下一步。
                    // 现在点它们只是不响应 —— 不做假的视觉反馈。
                    Some(_) => false,
                    None => false,
                }
            }
            _ => false,
        }
    }

    fn hit_test(&self) -> HitTest {
        let mut ht = HitTest {
            title_bar_height: self.layout.title_bar.h,
            resize_border: crate::popout::layout::RESIZE_BORDER,
            ..Default::default()
        };
        // 标题栏上的四个按钮要排除掉，否则点它们会被当成"按住标题栏拖动窗口"。
        for r in self.layout.buttons.iter() {
            ht.exclude(*r);
        }
        ht
    }

    fn min_size(&self) -> (i32, i32) {
        (crate::popout::state::MIN_W, crate::popout::state::MIN_H)
    }

    fn paint(&mut self) {
        if self.renderer.begin().is_err() {
            return;
        }
        let ctx = self.renderer.context().clone();
        if self.painter.ensure(&ctx).is_ok() {
            let _ = self.painter.draw(
                &ctx,
                &mut self.text,
                &self.layout,
                &self.scheme,
                &self.title,
                self.hover,
            );
        }
        let _ = self.renderer.end(if self.resizing { 0 } else { 1 });
        let _ = self.composer.commit();
    }

    fn on_close_request(&mut self) -> CloseAction {
        // 弹出窗口的"关闭"就是销毁它自己。
        // 注意这里**不能**返回 `Exit` 之外的语义：`Exit` 在本窗口上
        // 只意味着 DestroyWindow，主窗口不受影响。
        CloseAction::Exit
    }

    fn wants_tray(&self) -> bool {
        false
    }
}
