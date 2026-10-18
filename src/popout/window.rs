//! 弹出子窗口的窗口本体。
//!
//! 每个弹出窗口持有一整套自己的东西：`Window`（HWND）、`Renderer`（交换链）、
//! `PopoutPainter`、`TextEngine`、布局与配色，以及**自己的图标管线**
//!（`PopoutIcons`：一条加载线程 + 一份位图缓存，见 `icons.rs`）。
//! **不借用主窗口的任何运行时状态**（见 `mod.rs` 的选择 A），
//! 所以两层之间没有 `RefCell`、没有共享可变状态，也没有 `&mut` 别名问题。

use crate::config::theme::Scheme;
use crate::error::Result;
use crate::platform::hittest::HitTest;
use crate::platform::window::{CloseAction, Event, Geometry, Handler, Window};
use crate::popout::icons::{Content, PopoutIcons, pixel_size};
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
    /// 要显示的条目，与主窗口该选项卡的**可见条目**保持同步。
    ///
    /// 需求："弹出窗口要同步它在主窗口的内容 —— 弹出只是多了一种显示方式，
    /// 并不影响本身在主窗口的内容。" 也就是说弹出**不把选项卡从侧栏拿走**，
    /// 它是同一份数据的第二个视图，数量与顺序必须和主窗口一致。
    content: Content,
    /// 这个窗口自己的图标加载线程与位图缓存。
    icons: PopoutIcons,
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

        // 布局用**逻辑像素**：D2D 上下文的 DPI 已设成窗口 DPI，绘制时 DIP 会
        // 自动变成物理像素，所以这一层只跟 DIP 打交道。
        //（`client_size` 给的是物理像素，必须换算一次 —— 之前创建时直接拿物理
        //  尺寸去算布局，而 `Resized` 那条路是换算过的，两条路不一致：
        //  非 100% 缩放下刚创建出来的布局会整体偏大、右下角被裁掉一截。）
        let (lw, lh) = logical_size(cw, ch, window.scale());
        let layout = PopoutLayout::compute(lw, lh, icon_size);
        // 图标管线自己一份（见 `icons.rs`）：加载线程要在这个窗口上唤醒重绘。
        // 请求边长按 `icon_size` 算（与主窗口同一个来源），不是按算出来的方格边长。
        let icons = PopoutIcons::new(
            window.redraw_notifier(),
            pixel_size(icon_size, dpi as f32),
        );

        Ok(Self {
            window,
            renderer,
            composer,
            painter: PopoutPainter::new(),
            text: TextEngine::new()?,
            layout,
            scheme,
            title: title.to_string(),
            hover: None,
            resizing: false,
            content: Content {
                icon_size,
                items: Vec::new(),
            },
            icons,
        })
    }

    /// 用主窗口推来的快照更新内容。
    ///
    /// 返回内容是否**真的**变了：调用方据此决定要不要请求重绘 ——
    /// 内容没变还每帧重绘弹出窗口是白耗电。
    pub fn set_content(&mut self, content: Content) -> bool {
        if self.content == content {
            return false;
        }
        // 图标尺寸变了（主窗口 `Ctrl+滚轮`）：方格边长必须跟着变，
        // 否则图标与格子的关系会错位（图标比格子大一圈或小一圈）。
        // 弹出窗口自己也有一份尺寸，只在创建时拿到过一次，所以必须一起推过来。
        let icon_changed = self.content.icon_size != content.icon_size;
        self.content = content;
        // 内容换了 → 键表作废，下次绘制重建。
        // **作废的是键，不是缓存**：已经落地并命中同一批键的位图仍留在缓存里，
        // 不会因为换了个名字就把图标全部重新提取一遍。
        self.icons.invalidate();
        if icon_changed {
            self.relayout(self.window.client_size());
        }
        true
    }

    /// 按**物理**客户区尺寸重算布局（内部换算成 DIP），并同步图标请求边长。
    ///
    /// 这两件事必须一起做：请求的位图边长是从方格边长推出来的（1:1 绘制才清晰），
    /// 分开改就会出现"格子变了、位图还是旧尺寸"，图标被拉伸或缩得发虚。
    fn relayout(&mut self, phys: (i32, i32)) {
        let (lw, lh) = logical_size(phys.0, phys.1, self.window.scale());
        self.layout = PopoutLayout::compute(lw, lh, self.content.icon_size);
        self.sync_icon_size();
    }

    /// 让图标管线的请求边长跟上当前的图标尺寸与窗口 DPI。
    ///
    /// **取 `content.icon_size`（主窗口的图标尺寸）而不是 `layout.tile`。**
    /// 两个理由，方向相反但结论一致：
    ///
    /// * 需求要的是"同一个 `icon_size` 来源"—— 主窗口就是按 `icon_size` 取图的，
    ///   两边同一个来源才谈得上"图标一致"；
    /// * `layout.tile` 会被内容区宽度夹紧，也就是说**拖动窗口宽度时它在连续变化**。
    ///   拿它当键的输入，拖一次边框就会按每一帧的中间尺寸各请求一批图标，
    ///   而那一批尺寸永远不会再被用到 —— 白跑 shell 提取，正是"拖动变卡"的经典成因。
    ///
    /// 尺寸没变时它什么都不做（见 `PopoutIcons::set_pixel_size`）。
    fn sync_icon_size(&mut self) {
        let px = pixel_size(self.content.icon_size, self.window.dpi() as f32);
        self.icons.set_pixel_size(px);
    }

    /// 请求重绘一次（内容被推来后调用）。
    pub fn request_redraw(&self) {
        self.window.request_redraw();
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

/// 物理客户区尺寸 → 逻辑（DIP）尺寸。
///
/// 抽成一处是因为创建时、每次 `Resized`、每次 `DpiChanged` 都要做同一次换算。
/// 三处各写一份的后果只在非 100% 缩放下才暴露，而且症状很隐蔽：
/// 布局按物理算、绘制按 DIP 算，方格整体偏大、右下角被裁掉一截，
/// 看起来像"布局算法错了"，其实是单位没统一。
fn logical_size(phys_w: i32, phys_h: i32, scale: f32) -> (i32, i32) {
    if scale <= 0.0 {
        return (phys_w.max(1), phys_h.max(1));
    }
    (
        (phys_w as f32 / scale).round() as i32,
        (phys_h as f32 / scale).round() as i32,
    )
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
                // 布局用事件里的尺寸重算：`Resized` 带的就是物理像素，
                // 而 DIP 换算统一在 `relayout` 里做。
                //
                // 图标尺寸取 `content.icon_size`（主窗口那侧的真值）而不是
                // 上一次的方格边长：后者是被内容区宽度夹紧过的结果，
                // 拿它当输入会让"窗口缩窄再放宽"之后方格回不到原尺寸。
                self.relayout((width as i32, height as i32));
                true
            }
            // DPI 变了（把窗口拖到另一块缩放不同的显示器上）。
            //
            // 两件事都要做：布局的 DIP 尺寸变了（物理客户区没变但缩放系数变了），
            // 位图要按新的物理边长重新提取（键里带着尺寸，所以是另一批键）。
            // `relayout` 一次把两件都做了。
            Event::DpiChanged { .. } => {
                self.relayout(self.window.client_size());
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
        // 收异步结果 → 转 D2D 位图 → 进缓存。**必须在绘制之前做完**：
        // `insert` 会触发 LRU 淘汰，边画边淘汰可能把正要画的那张位图释放掉
        //（主窗口 `App::draw` 里有同一条规则的说明）。
        self.icons.poll_and_insert(&ctx);
        // **文字引擎也要 ensure。**
        //
        // 它的笔刷是懒创建的，没建之前 `TextEngine::draw` 会**静默返回**
        //（`let Some(brush) = self.brush.clone() else { return Ok(()) }`）——
        // 一个字都不画，也不报错、不留日志。主窗口在 `App::draw` 里调过这一句，
        // 这个窗口当时漏了，症状就是"图标都在，但标题和名称一个字都没有"。
        //
        // 失败不致命（与漏调时的表现相同：没有文字），所以这里不往上抛。
        let _ = self.text.ensure(&ctx);
        if self.painter.ensure(&ctx).is_ok() {
            // 只给**看得见**的格子取图标：看不见的既不该请求、也不该占缓存。
            // 判据与绘制共用 `visible_tiles`，两处不会算出不同的范围。
            let visible = self.layout.visible_tiles(self.content.items.len());
            let bitmaps = self.icons.collect(&self.content.items, visible);
            let _ = self.painter.draw(
                &ctx,
                &mut self.text,
                &self.layout,
                &self.scheme,
                &self.title,
                self.hover,
                &self.content.items,
                &bitmaps,
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
