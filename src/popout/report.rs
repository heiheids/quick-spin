//! 弹出子窗口的"我自己关了"回报通道。
//!
//! # 为什么需要它
//!
//! 关闭有两条路，但只有一条会回到装配层：
//!
//! * `App::toggle_popout`（主窗口右键菜单）—— 装配层自己发起的，它知道；
//! * **弹窗自己右上角的 ✕**、Alt+F4 —— 窗口层销毁自己，**装配层不知道**。
//!
//! 后者会让 `App::popouts` 里留着一个已经死掉的窗口：右键菜单的文案是按
//! "表里有没有"生成的，于是仍然显示「关闭子窗口」（点下去毫无反应，窗口早没了），
//! `state.popout.windows[key].open` 也还是 `true` —— 下次启动会把它当成开着的
//! 恢复出来。
//!
//! # 为什么是回报，不是每帧存活探测
//!
//! 每帧对每个弹窗调一次 `IsWindow` 是"每帧 N 次 syscall"，而且它把"谁关了"
//! 变成**轮询推断**：查到窗口没了，还得再猜是哪一个。回报是事件驱动的，
//! 只在真的关窗时写一次，代价与窗口数无关。
//!
//! 同一个问题在 `platform::drop_target` 那里已经解过一次 —— 拖放目标落完盘，
//! 把路径攒进静态缓冲等上层取走。这里**刻意沿用同一个模式**：
//! 谁产生的状态谁攒着，上层在一个明确的时机取走。两处长得一样，
//! 以后见到第三处也不用重新想。
//!
//! # 线程
//!
//! 窗口过程与上层的事件处理都在同一个 UI 线程上（消息循环所在线程），
//! 所以这里并不存在真正的并发；`Mutex` 只是为了让可变静态合法。

use std::sync::Mutex;

/// 已经自己关掉的弹窗的**选项卡 key**，等上层取走。
///
/// 存 key 而不是窗口句柄：上层是按选项卡 key 存表的（`App::popouts`），
/// 句柄在窗口销毁之后就失效了，key 才是它认得的东西。
static CLOSED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// "这个弹窗的位置 / 吸附 / 钉住状态变了，请落盘"的选项卡 key。
///
/// 与 [`CLOSED`] 分开两条通道：两者的收尾动作完全不同（一条要摘表项、
/// 一条只更新记录），合成一条还得在上层先分辨是哪一种。
///
/// 为什么需要它：拖动脱离吸附、点吸附/钉住按钮都发生在**弹窗自己**的事件
/// 处理里，而 `state.toml` 归装配层管。窗口层直接写状态会让"谁负责落盘"
/// 变成两处，且窗口层也不该知道 `state` 的形状。
static CHANGED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// 记下"这个弹窗关掉了"。由窗口层在**销毁时**调用。
///
/// 放在销毁时而不是 `on_close_request` 里：后者只有走 `WM_CLOSE` 的路径才会
/// 被调用，而销毁覆盖**所有**路径（✕、Alt+F4、程序自己请求关闭）。
pub fn report_closed(tab_key: &str) {
    lock(&CLOSED).push(tab_key.to_string());
}

/// 记下"这个弹窗的状态变了，请落盘"。
///
/// 由窗口层在**用户动作之后**调用（拖完松手、点了吸附/钉住）。
/// **不要每帧调**：每一条回报都会让装配层写一次 `state.toml`。
pub fn report_changed(tab_key: &str) {
    lock(&CHANGED).push(tab_key.to_string());
}

/// 取走并清空全部"已关闭"回报。上层在收尾点调用（见 `App::adopt_closed_popouts`）。
///
/// 缓冲为空时 `drain(..).collect()` 不会分配内存，所以调用它本身很便宜。
pub fn take_closed() -> Vec<String> {
    lock(&CLOSED).drain(..).collect()
}

/// 取走并清空全部"状态已变"回报（见 `App::adopt_popout_states`）。
pub fn take_changed() -> Vec<String> {
    lock(&CHANGED).drain(..).collect()
}

/// 取那个静态缓冲。
///
/// 中毒（持锁时 panic）时取回内部值继续用：回报丢了最多是"菜单文案旧一帧"
/// 或者"下次启动多恢复一个窗口"，不该让整个程序跟着崩。
fn lock(buf: &'static Mutex<Vec<String>>) -> std::sync::MutexGuard<'static, Vec<String>> {
    match buf.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
