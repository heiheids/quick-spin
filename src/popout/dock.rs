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
//!
//! ## 堆叠（2026-10-18 修）
//!
//! 早先每个吸附中的窗口都算同一个位置（右吸附 = 贴主窗口右边缘、上边对齐），
//! 计算里**没有"那一侧已经站了别人"这个概念** —— 于是多个窗口一起吸附时
//! **完全重叠**（使用者实测报障）。
//!
//! 现在：沿主窗口那一侧竖直排成一条链（首尾相接），一列排满就向外另起一列；
//! 链里被拖走留下的空位由后来的窗口回填。为此输入多了一项
//! [`DockRequest::occupied`]，由装配层给出（只有它持有 `popouts` 那张表）。

use super::state::DockSide;
use crate::platform::window::Rect;

/// 算一次吸附所需的全部输入。
#[derive(Debug, Clone, Copy)]
pub struct DockRequest<'a> {
    /// 主窗口矩形（屏幕物理像素）。
    pub main: Rect,
    /// 这个弹窗的尺寸（物理像素）。
    pub size: (i32, i32),
    /// **期望**贴哪一侧。
    ///
    /// 只有拿不到 [`Self::current`] 时才用它 —— 创建时窗口还没出现，
    /// 也就无从判断"它现在在哪一边"。
    pub side: DockSide,
    /// 主窗口所在显示器的工作区（扣掉任务栏）。`None` = 拿不到。
    pub work: Option<Rect>,
    /// 这个窗口**当前**左上角（物理像素）。`None` = 还没有位置。
    ///
    /// 用来判断"该吸哪一侧"：使用者的意图是**拖到哪边就吸哪边**
    ///（原话："拖动到某个窗口方向，再点吸附就近对齐……"）。
    pub current: Option<(i32, i32)>,
    /// **已经排在链上的窗口矩形**，不含自己。
    ///
    /// 为什么由外面给：只有装配层持有 `popouts` 那张表。这个函数仍然是纯的 ——
    /// 它只知道几何，不知道谁是窗口。
    pub occupied: &'a [Rect],
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
/// 2. **吸哪一侧**以窗口当前中心相对主窗口中心的方向为准（拖到右边吸右边）；
///    窗口还没有位置时用调用方给的期望侧。
/// 3. 那一侧水平放不下、而另一侧放得下 → 翻到另一侧。
/// 4. 沿这一侧找一个**不与 `occupied` 相交**的槽位（见 [`place_in_columns`]）。
/// 5. 一列都放不下（主窗口比工作区还宽之类）时**保持这一侧**，把位置夹进
///    工作区 —— 宁可和主窗口重叠，也不要把弹窗推出屏幕。
pub fn dock_position(req: &DockRequest) -> Option<Docked> {
    let work = req.work?;
    let (w, h) = (req.size.0.max(1), req.size.1.max(1));

    // 该吸哪一侧：拖到哪边就吸哪边。用中点判，不用左边缘 ——
    // 窗口很宽时左边缘可能已经在主窗口右侧，而它视觉上仍横跨主窗口。
    let want = match req.current {
        Some((cx, _)) => {
            if cx + w / 2 < req.main.center_x() {
                DockSide::Left
            } else {
                DockSide::Right
            }
        }
        None => req.side,
    };

    let right_x = req.main.right();
    let left_x = req.main.x - w;
    let fits_right = right_x + w <= work.right();
    let fits_left = left_x >= work.x;

    // 期望的侧优先；只有在它放不下**而另一侧放得下**时才翻。
    let side = match want {
        DockSide::Right if !fits_right && fits_left => DockSide::Left,
        DockSide::Left if !fits_left && fits_right => DockSide::Right,
        other => other,
    };

    if let Some((x, y)) = place_in_columns(req, work, side, w, h) {
        return Some(Docked { x, y, side });
    }

    // 这一侧一列都排不下：夹进工作区（这是"唯一把它拉回屏幕内"的动作）。
    let raw_x = match side {
        DockSide::Right => right_x,
        DockSide::Left => left_x,
    };
    let x = clamp(raw_x, work.x, work.right() - w);
    let y = clamp(req.main.y, work.y, work.bottom() - h);
    Some(Docked { x, y, side })
}

/// 在 `side` 那一侧逐列找一个不与 `occupied` 相交的槽位。
///
/// * **第 0 列贴着主窗口**，第 `c` 列在第 `c-1` 列的更外侧；
/// * 每列从主窗口上边缘开始往下排，遇到挡路的矩形就滑到它的下边缘 ——
///   于是链上窗口首尾相接，而**链里空出来的位置会被回填**
///  （判定"空位"就是"这里与所有已吸附窗口都不相交"，不需要另记一份空位表）；
/// * 一列排到工作区底边以下就换下一列；
/// * 列的间距取"自己与这一侧已占用矩形的最大宽度"：窗口宽度可能各不相同，
///   用固定间距会让宽窗口压到邻列。
///
/// 返回 `None` 表示这一侧一列都放不下（调用方退回"夹进工作区"）。
fn place_in_columns(
    req: &DockRequest,
    work: Rect,
    side: DockSide,
    w: i32,
    h: i32,
) -> Option<(i32, i32)> {
    let anchor = match side {
        DockSide::Right => req.main.right(),
        DockSide::Left => req.main.x,
    };

    // 列间距：只看落在锚点**外侧**的那些矩形（另一侧的、或压在主窗口上的不算）。
    let mut pitch = w.max(1);
    for r in req.occupied {
        let outward = match side {
            DockSide::Right => r.x >= anchor,
            DockSide::Left => r.right() <= anchor,
        };
        if outward {
            pitch = pitch.max(r.w);
        }
    }

    // 起点：与主窗口上边缘对齐，再夹进工作区（保证整窗可见）。
    let start_y = clamp(req.main.y, work.y, work.bottom() - h);

    for col in 0..MAX_COLUMNS {
        let x = match side {
            DockSide::Right => anchor + col * pitch,
            DockSide::Left => anchor - w - col * pitch,
        };
        // 这一列整体已经出界 → 再往外更不行，收工。
        if x + w > work.right() || x < work.x {
            return None;
        }

        let mut y = start_y;
        loop {
            if y + h > work.bottom() {
                break; // 这一列排满了，换下一列
            }
            let cand = Rect { x, y, w, h };
            // 滑到"所有挡路的矩形"里最靠上的那个下边缘 —— 于是接到链尾。
            let mut next = i32::MAX;
            for r in req.occupied {
                if overlaps(&cand, r) {
                    next = next.min(r.bottom());
                }
            }
            if next == i32::MAX {
                return Some((x, y)); // 没人挡 → 就是这里
            }
            if next <= y {
                // 防御：正常情况下算不出这种结果（相交蕴含 `r.bottom() > y`）。
                // 万一将来改坏了，宁可放弃也不在这里死循环。
                return None;
            }
            y = next;
        }
    }
    None
}

/// 两个矩形是否相交。**相切不算相交**（半开区间）——
/// 吸附链要求的"上一个的下边缘 = 下一个的上边缘"正是相切。
fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

/// 最多试多少列就放弃。
///
/// 横向的越界判断已经能终止循环（列只会越来越远），这个上限只是保险：
/// 万一 `pitch` 退化成很小的值，也不至于在一条极宽的显示器上转上千圈。
const MAX_COLUMNS: i32 = 32;

/// 夹紧到 `[lo, hi]`，**`lo > hi` 时不 panic**。
///
/// 标准库的 `i32::clamp` 在 `min > max` 时会 panic，而这里 `lo > hi` 是能
/// 真实发生的：弹窗比工作区还宽（用户把窗口拉到比屏幕还大，或者工作区被
/// 竖排任务栏切得很窄）。那种情况下取 `lo`（贴左/上边）——
/// 至少有一个角在屏幕内，而不是崩掉整个进程。
fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    v.max(lo).min(hi.max(lo))
}
