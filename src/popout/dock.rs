//! 吸附跟随的**几何**：一个纯函数，没有窗口、没有 HWND。
//!
//! ## 为什么要单独一个模块
//!
//! 吸附的位置在**两个地方**要用：创建弹窗时第一次摆位、主窗口移动时跟随。
//! 这两处各写一份数学必然漂移（本工程反复吃过这个亏：键的构造、图标来源、
//! 显示名规则都因为"两处各写一份"出过 bug）。所以计算只在这里定义一次，
//! 调用方只负责把输入凑齐、把结果交给 `SetWindowPos`。
//!
//! 顺带的好处是它能被 `--selftest` 完全覆盖 —— 纯 `i32` 算术，
//! 不需要真的建窗口。而"真的跟着动了没有"只能人工验（自检走不到第二个窗口）。

use super::state::DockSide;
use crate::platform::window::Rect;

/// 算一次吸附所需的全部输入。
#[derive(Debug, Clone, Copy)]
pub struct DockRequest {
    /// 主窗口矩形（屏幕物理像素）。
    pub main: Rect,
    /// 弹窗尺寸（物理像素）。
    pub size: (i32, i32),
    /// 使用者**期望**贴哪一侧。
    pub side: DockSide,
    /// 主窗口所在显示器的工作区（扣掉任务栏）。`None` = 拿不到。
    pub work: Option<Rect>,
}

/// 算出来的落位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Docked {
    pub x: i32,
    pub y: i32,
    /// **实际**落在哪一侧（可能与期望的不同：那一侧放不下时会翻边）。
    ///
    /// 注意它是结果而不是新的期望：`PopoutWindow::dock_side` 存的始终是
    /// 使用者的意图，翻边不该改掉那个意图（见该字段的说明）。
    pub side: DockSide,
}

/// 算出吸附位置；`None` 表示"这次不要摆"。
///
/// ## 规则
///
/// 1. **拿不到工作区就不动**（返回 `None`）。猜一个坐标的后果是把窗口丢到
///    看不见的地方，比"这帧不跟随"糟糕得多。
/// 2. 水平方向先试**期望的那一侧**：右吸附 = 弹窗左边缘贴主窗口右边缘
///    （贴着，不重叠）。那一侧放不下就翻到另一侧。
/// 3. 两侧都放不下（主窗口比工作区还宽之类）时**保持期望的侧**，把位置夹进
///    工作区 —— 宁可和主窗口重叠，也不要把弹窗推出屏幕。
/// 4. 垂直方向与主窗口顶部对齐，再夹进工作区，保证整窗可见。
pub fn dock_position(req: &DockRequest) -> Option<Docked> {
    let work = req.work?;
    let (w, h) = (req.size.0.max(1), req.size.1.max(1));

    let right_x = req.main.right();
    let left_x = req.main.x - w;
    let fits_right = right_x + w <= work.right();
    let fits_left = left_x >= work.x;

    // 期望的侧优先；只有在它放不下**而另一侧放得下**时才翻。
    let side = match req.side {
        DockSide::Right if !fits_right && fits_left => DockSide::Left,
        DockSide::Left if !fits_left && fits_right => DockSide::Right,
        other => other,
    };

    let raw_x = match side {
        DockSide::Right => right_x,
        DockSide::Left => left_x,
    };
    // 两侧都放不下时这一步是"唯一把它拉回屏幕内"的动作。
    let x = clamp(raw_x, work.x, work.right() - w);
    let y = clamp(req.main.y, work.y, work.bottom() - h);

    Some(Docked { x, y, side })
}

/// 夹紧到 `[lo, hi]`，**`lo > hi` 时不 panic**。
///
/// 标准库的 `i32::clamp` 在 `min > max` 时会 panic，而这里 `lo > hi` 是能
/// 真实发生的：弹窗比工作区还宽（用户把窗口拉到比屏幕还大，或者工作区被
/// 竖排任务栏切得很窄）。那种情况下取 `lo`（贴左/上边）——
/// 至少有一个角在屏幕内，而不是崩掉整个进程。
fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    v.max(lo).min(hi.max(lo))
}
