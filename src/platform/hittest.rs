//! 无边框窗口的命中测试。
//!
//! ## 为什么必须在 `WM_NCHITTEST` 里**同步**返回
//!
//! 系统要知道"这一击是拖动窗口、拉边框，还是普通点击"，才能决定后续怎么处理。
//! 不能把这个判断丢进事件队列等主循环 —— 等主循环处理时系统已经按默认行为走完了。
//!
//! ## 为什么这是"不拖影"的关键
//!
//! 标题栏返回 `HTCAPTION` 之后，**窗口平移由系统（DWM）完成**，
//! 它搬的是已经合成好的画面，天然没有残影。
//! 如果改成自己在 `WM_MOUSEMOVE` 里 `SetWindowPos`，鼠标位置与窗口位置总差一帧，
//! 快速拖动时就会看到边框拖出残影 —— 这是这类程序最常见的视觉缺陷。

/// 一个屏幕/客户区矩形（物理像素）。故意用简单结构体而不是 `RECT`，
/// 是为了让这个模块不依赖 `windows` crate，逻辑可以单独测。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RectI {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl RectI {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    /// 两个矩形是否**相交**（相切也算）。
    ///
    /// 框选多选用它判断"这个方格被选框划到了没"。用相交而不是包含：
    /// 用户只要划到方格的一角就该选中它 —— 要求整个方格都被框住的话，
    /// 方格和图标一样大，随手一划几乎选不中任何东西。
    ///
    /// 宽或高为 0 的"线状框"同样有效（纯竖直/水平拖动也要能选中），
    /// 因为这里是半开区间比较，不是先算面积。
    pub const fn intersects(&self, other: &Self) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    /// 转成绘制层要用的 `(left, top, width, height)` 浮点矩形。
    pub fn as_f32(self) -> (f32, f32, f32, f32) {
        (self.x as f32, self.y as f32, self.w as f32, self.h as f32)
    }
}

/// 命中测试的结果，取值与 `WM_NCHITTEST` 的返回值一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// 普通客户区，交给上层当点击处理
    Client,
    /// 标题栏，交给系统拖动窗口
    Caption,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Hit {
    /// 转成 `WM_NCHITTEST` 要返回的数值。
    pub const fn as_nchittest(self) -> isize {
        match self {
            Self::Client => 1,      // HTCLIENT
            Self::Caption => 2,     // HTCAPTION
            Self::Left => 10,       // HTLEFT
            Self::Right => 11,      // HTRIGHT
            Self::Top => 12,        // HTTOP
            Self::TopLeft => 13,    // HTTOPLEFT
            Self::TopRight => 14,   // HTTOPRIGHT
            Self::Bottom => 15,     // HTBOTTOM
            Self::BottomLeft => 16, // HTBOTTOMLEFT
            Self::BottomRight => 17, // HTBOTTOMRIGHT
        }
    }
}

/// 标题栏里最多允许几个"不触发拖动"的交互区域。
///
/// 用固定数组而不是 `Vec`：命中测试在鼠标移动时高频触发，不能每次都分配。
///
/// **加标题栏按钮时必须回头确认这个数够不够**：超限的排除区域会被
/// `exclude` 静默丢弃，现象是"按钮画出来了但点不动"，而且没有任何报错 ——
/// 因为点击被 `WM_NCHITTEST` 判成 `HTCAPTION`，应用层根本收不到
/// `WM_LBUTTONDOWN`。（标题文字的可点区域就这么被吃掉过一次。）
///
/// 现在同时存在的有：隐藏桌面图标、添加、最小化、关闭四个按钮，
/// 加上标题文字，共 5 个。留到 8 是为了以后再加按钮时不必立刻改这里。
pub const MAX_CAPTION_EXCLUSIONS: usize = 8;

/// 窗口层每帧从上层取回的命中测试参数，全部是**逻辑像素**。
#[derive(Debug, Clone, Copy, Default)]
pub struct HitTest {
    /// 标题栏高度
    pub title_bar_height: i32,
    /// 缩放边框宽度。`0` 表示禁用缩放。
    pub resize_border: i32,
    /// 标题栏内要当作普通客户区处理的矩形（例如最小化/关闭按钮）。
    /// 不排除它们的话，点按钮会被当成拖窗口。
    pub exclusions: [RectI; MAX_CAPTION_EXCLUSIONS],
    pub exclusion_count: usize,
}

impl HitTest {
    /// 追加一个排除区域。
    ///
    /// 超出上限时**在调试版里直接断言**而不是默默丢弃：静默丢弃的后果
    ///（某个按钮点不动）完全没有指向性，排查起来很费劲。
    pub fn exclude(&mut self, rect: RectI) {
        debug_assert!(
            self.exclusion_count < MAX_CAPTION_EXCLUSIONS,
            "标题栏排除区域超出上限 {}：请调大 MAX_CAPTION_EXCLUSIONS，\
             否则多出来的那块区域会被当成标题栏拖动、点击收不到",
            MAX_CAPTION_EXCLUSIONS
        );
        if self.exclusion_count < MAX_CAPTION_EXCLUSIONS {
            self.exclusions[self.exclusion_count] = rect;
            self.exclusion_count += 1;
        }
    }

    fn is_excluded(&self, x: i32, y: i32, scale: f32) -> bool {
        // 排除区域与 title_bar_height 一样是逻辑像素，必须一起换算，
        // 否则在 150% 缩放下按钮热区会偏到左边一大截。
        self.exclusions[..self.exclusion_count].iter().any(|r| {
            let sx = (r.x as f32 * scale).round() as i32;
            let sy = (r.y as f32 * scale).round() as i32;
            let sw = (r.w as f32 * scale).round() as i32;
            let sh = (r.h as f32 * scale).round() as i32;
            x >= sx && x < sx + sw && y >= sy && y < sy + sh
        })
    }
}

/// 判断客户区坐标 `(x, y)` 落在哪里。
///
/// `dpi` 用于把逻辑像素换算成物理像素 —— 配置里的尺寸都是逻辑值，
/// 而在 150% 缩放的屏幕上物理鼠标坐标是逻辑值的 1.5 倍。
pub fn hit_test(cfg: &HitTest, x: i32, y: i32, width: i32, height: i32, dpi: u32) -> Hit {
    let scale = dpi as f32 / 96.0;
    let to_phys = |v: i32| (v as f32 * scale).round() as i32;

    let border = to_phys(cfg.resize_border);
    let caption_h = to_phys(cfg.title_bar_height);

    // 最大化状态下不该有缩放边（否则拖边缘会变成还原窗口，很别扭）。
    // 这里不做 IsZoomed 调用，保持本模块是纯逻辑、可单独测试；
    // 上层在窗口最大化时把 resize_border 置 0 即可。
    let resizable = border > 0;

    if resizable {
        let on_left = x < border;
        let on_right = x >= width - border;
        let on_top = y < border;
        let on_bottom = y >= height - border;

        match (on_left, on_right, on_top, on_bottom) {
            (true, _, true, _) => return Hit::TopLeft,
            (_, true, true, _) => return Hit::TopRight,
            (true, _, _, true) => return Hit::BottomLeft,
            (_, true, _, true) => return Hit::BottomRight,
            (true, _, _, _) => return Hit::Left,
            (_, true, _, _) => return Hit::Right,
            (_, _, true, _) => return Hit::Top,
            (_, _, _, true) => return Hit::Bottom,
            _ => {}
        }
    }

    if y < caption_h && !cfg.is_excluded(x, y, scale) {
        return Hit::Caption;
    }
    Hit::Client
}
