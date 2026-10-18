//! 图标异步加载线程。
//!
//! 图标提取要走 shell，单个耗时 4~200ms 不等（快捷方式最慢，要解引用）。
//! 放在 UI 线程上会让窗口在首次显示和滚动时明显卡顿，所以单独开一条线程。
//!
//! ## 分工
//!
//! 工作线程只做**纯数据**提取，产出 [`IconBitmap`]（一块 BGRA 内存）。
//! 它**不碰 Direct2D** —— D2D 设备上下文不是线程安全的，
//! 而且 GPU 资源必须在渲染线程创建。转成 `ID2D1Bitmap` 是 UI 线程的事。
//!
//! ## 唤醒
//!
//! 提取完成后需要让 UI 重绘一次，否则结果要等到下一次用户操作才显示出来。
//! 这里通过 [`RedrawNotifier`] 往窗口投递消息来唤醒（`PostMessage` 是跨线程安全的）。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::error::Result;
use crate::platform::window::RedrawNotifier;

use super::icon::{self, IconBitmap};

/// 图标缓存的键：同一个文件在不同尺寸下是**不同的位图**，必须分开存。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub path: PathBuf,
    pub size: u32,
    /// 按**图片**取（缩略图）而不是按**图标**取。
    ///
    /// 两者是不同的 shell 路径：不加 `SIIGBF_ICONONLY` 才会解码图片本身，
    /// 加了则返回"该文件的类型图标"。用户"更换图标"选了一张 PNG 时，
    /// 若仍按图标取，拿到的是通用的图片文件图标 —— 看起来就是"导入后看不到图"。
    ///
    /// 放进键里而不是运行时判断：同一个路径两种取法结果不同，
    /// 混用会让缓存返回错误的位图。
    pub as_image: bool,
}

impl Key {
    /// 按**图标**取（快捷方式、exe、文件夹都走这个）。
    pub fn new(path: impl Into<PathBuf>, size: u32) -> Self {
        Self {
            path: path.into(),
            size,
            as_image: false,
        }
    }

    /// 按**图片**取（用户导入的自定义图标）。
    pub fn image(path: impl Into<PathBuf>, size: u32) -> Self {
        Self {
            path: path.into(),
            size,
            as_image: true,
        }
    }
}

/// 加载结果。
pub struct Loaded {
    pub key: Key,
    pub result: Result<IconBitmap>,
}

pub struct Loader {
    /// 发给工作线程的请求。`None` 用于让它退出（Drop 时发生）。
    tx: Option<Sender<Key>>,
    rx: Receiver<Loaded>,
    /// 已经发出去、还没收回来的键。避免每帧重复请求同一个图标。
    in_flight: HashSet<Key>,
    /// **已经失败过**的键。提取不成功的一律不再重试。
    ///
    /// 这张表是必须的，不是优化。缺了它就会出现"每帧重试"：
    /// `poll()` 会把结果从 `in_flight` 里摘掉，而失败的图标不会进缓存，
    /// 于是下一帧 `collect_icons` 发现缓存里没有 → 再请求一次 →
    /// 工作线程再跑一次注定失败的 shell 提取 → 如此往复。
    ///
    /// 拖动时每帧都重绘，这个环就被驱动到帧率 —— 实测 10 个方格里有 7 个
    /// 图标处于这种状态，30 帧内重复请求 **211 次**。加载线程被这种白工占满，
    /// 界面自然又卡又不跟手，而且这笔开销**完全不体现在"绘制耗时"里**。
    ///
    /// 用"满了就整体清空"来限制规模，顺带获得周期性重试的机会
    /// （比如某个文件后来才有图标）。换图标尺寸会自然生成新键，不受影响。
    failed: HashSet<Key>,
    /// 工作线程句柄，Drop 时 join，避免线程泄漏
    join: Option<std::thread::JoinHandle<()>>,
}

/// `failed` 的规模上限。超过就整体清空：既封住内存，又给失败项一个重试窗口。
const FAILED_MEMO: usize = 256;

impl Loader {
    /// 启动加载线程。`notify` 用于在结果就绪时唤醒 UI。
    pub fn spawn(notify: RedrawNotifier) -> Self {
        let (req_tx, req_rx) = channel::<Key>();
        let (res_tx, res_rx) = channel::<Loaded>();

        let handle = std::thread::Builder::new()
            .name("icon-loader".into())
            .spawn(move || worker(req_rx, res_tx, notify))
            .expect("创建图标加载线程失败");

        Self {
            tx: Some(req_tx),
            rx: res_rx,
            in_flight: HashSet::new(),
            failed: HashSet::new(),
            join: Some(handle),
        }
    }

    /// 请求一个图标。已在途、或**已经失败过**的会被忽略。
    pub fn request(&mut self, key: Key) {
        if self.failed.contains(&key) {
            return;
        }
        if self.in_flight.contains(&key) {
            return;
        }
        // 发送失败说明工作线程已经没了。这时不标记 in_flight，
        // 否则这个键会永远卡在"在途"状态，图标再也加载不出来。
        if let Some(tx) = &self.tx {
            if tx.send(key.clone()).is_ok() {
                self.in_flight.insert(key);
            }
        }
    }

    /// 取回已完成的结果（不阻塞）。调用方负责把它们转成 D2D 位图。
    ///
    /// **失败的键在这里被记下并从此不再请求**（见 `failed` 的说明）：
    /// 调用方拿到失败结果时只会"静默跳过"，它没有能力阻止下一次请求，
    /// 所以这件事必须由本类型负责 —— 否则就是每帧重试。
    pub fn poll(&mut self) -> Vec<Loaded> {
        let mut out = Vec::new();
        while let Ok(loaded) = self.rx.try_recv() {
            self.in_flight.remove(&loaded.key);
            if loaded.result.is_err() {
                if self.failed.len() >= FAILED_MEMO {
                    // 整体清空：封住内存，同时给所有失败项一次重试机会。
                    self.failed.clear();
                }
                self.failed.insert(loaded.key.clone());
            }
            out.push(loaded);
        }
        out
    }

    /// 还在排队/提取中的请求数。自检用来判断"是否已全部落地"。
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        // 关掉发送端，工作线程的 `for key in rx` 就会结束
        self.tx = None;
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// 工作线程主体。
fn worker(req_rx: Receiver<Key>, res_tx: Sender<Loaded>, notify: RedrawNotifier) {
    // shell 的图标接口要求 COM。加载线程用 MTA：
    // 它不做任何需要消息泵的操作，MTA 更省事也更快。
    // 初始化失败也继续跑 —— extract 会各自返回错误，比整条线程静默退出好定位。
    let _com = crate::shell::com::ComGuard::mta();

    // 同一个路径会被反复请求（滚动来回），这里记一份已提取的结果，
    // 省掉重复的 shell 调用。真正的内存预算由 UI 侧的 LRU 缓存管，
    // 这里只是"最近提取过的原始像素"，所以限制条数即可。
    const WORKER_MEMO: usize = 64;
    let mut memo: std::collections::HashMap<Key, IconBitmap> = std::collections::HashMap::new();
    let mut memo_order: std::collections::VecDeque<Key> = std::collections::VecDeque::new();

    while let Ok(key) = req_rx.recv() {
        let result = match memo.get(&key) {
            Some(cached) => Ok(cached.clone()),
            None => {
                let extracted = if key.as_image {
                    icon::extract_image(&key.path, key.size)
                } else {
                    icon::extract(&key.path, key.size)
                };
                if let Ok(bitmap) = &extracted {
                    memo.insert(key.clone(), bitmap.clone());
                    memo_order.push_back(key.clone());
                    while memo_order.len() > WORKER_MEMO {
                        if let Some(old) = memo_order.pop_front() {
                            memo.remove(&old);
                        }
                    }
                }
                extracted
            }
        };

        if res_tx.send(Loaded { key, result }).is_err() {
            // UI 侧已经销毁，没人要结果了
            break;
        }
        notify.request_redraw();
    }
}
