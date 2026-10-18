//! 弹出子窗口的**内容快照**与**图标管线**（方案 A：每个窗口自己一套）。
//!
//! ## 为什么每个窗口自己一份 `Loader` + `IconCache`
//!
//! 共享主窗口那一份的话，就要在"另一个窗口的绘制路径"里拿到主窗口的
//! `&mut`，于是要么引入 `RefCell`、要么面对借用别名 —— 两条路都把
//! "谁在什么时候动缓存"变成隐式约定，出错的症状还是那种"偶尔画错一张"
//! 的难查问题。分开之后每个窗口自给自足，代价是每个窗口多一条加载线程
//! 和一份位图缓存（上限见 [`POPOUT_BUDGET`]，实测一屏的量级不到 1 MB）。
//!
//! ## 两个窗口的图标为什么能一致
//!
//! 不是靠共享状态，而是靠**同一套规则**：
//!
//! * **图标来源**由主窗口算（只有它知道覆盖表与 library），规则只有一处
//!   （`App::icon_source`）——"更换图标"优先，否则用 `.lnk` 本身；
//! * **键的构造**只有一处（[`IconSource::key`] + [`pixel_size`]）：
//!   DIP → 物理像素、按图片取还是按图标取，两个窗口走的是同一个函数。
//!
//! 所以"同一个条目在两个窗口里图标不一样"这类问题在结构上就不会发生。
//!
//! ## 内容快照
//!
//! [`Content`] 是主窗口推来的那一份数据（名称 + 图标来源 + 图标逻辑边长）。
//! 图标来源放在它里面，是因为"要画什么"和"图标从哪来"本来就是同一件事 ——
//! 分成两个类型只会让推送点多一次拆装。

use std::path::PathBuf;

use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap1, ID2D1DeviceContext};

use crate::platform::icon_loader::{Key, Loader};
use crate::platform::window::RedrawNotifier;
use crate::render::icon_cache::IconCache;

/// 弹出窗口自己的位图缓存上限。
///
/// 一屏的需求可以**按布局推算**（这是计算值，不是实测值 —— 弹出窗口那条路径
/// 自检跑不到，需要真实界面确认）：方格边长与图标边长同增同减，所以视口里的
/// 图标总量几乎恒定 —— 方格 16px 时一屏约 130 格（16×16×4 ≈ 1 KB/张，
/// 合计约 0.13 MB）；方格 256px 时一屏只有 1~2 格（约 256 KB/张，合计约
/// 0.26 MB）；200% DPI 下位图面积 ×4，也就是 ≤ 1.2 MB。
///
/// 所以这个数的意义不是"单屏需要多少"，而是**来回切几个选项卡的余量**，
/// 以及"多开几个窗口也不会无限涨"的上限。8 MB 对一屏的需求有几十倍富余。
///
/// **注意真正的内存大头可能不在这里**：加载线程自己还留着一份原始像素备忘
///（`icon_loader` 里的 `WORKER_MEMO`，每个窗口一份）。它的条数与像素边长
/// 才是大图标尺寸下的主导项 —— 详见 `docs/ARCHITECTURE.md` §7.2。
pub const POPOUT_BUDGET: usize = 8 * 1024 * 1024;

/// 一条条目的图标来源。
///
/// `as_image` 与 [`Key`] 里那个字段是同一个意思，但**必须由主窗口决定**：
/// 只有它知道用户有没有给这一条"更换图标"。弹出窗口照做即可。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconSource {
    pub path: PathBuf,
    /// 按**图片**取（用户"更换图标"选的是一张图）而不是按**图标**取。
    ///
    /// 两者是不同的 shell 路径：不加 `SIIGBF_ICONONLY` 才会解码图片本身。
    /// 混用会拿到"该文件的类型图标"，表现为"导入的图片看不到"。
    pub as_image: bool,
}

impl IconSource {
    /// 按**图标**取（快捷方式、exe、文件夹、shell 命名空间项都走这个）。
    pub fn icon(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            as_image: false,
        }
    }

    /// 按**图片**取（用户导入的自定义图标）。
    pub fn image(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            as_image: true,
        }
    }

    /// 按这个来源构造缓存键。
    ///
    /// 两个窗口都走这一个函数，所以键不可能对不上。
    pub fn key(&self, pixel_size: u32) -> Key {
        if self.as_image {
            Key::image(self.path.clone(), pixel_size)
        } else {
            Key::new(self.path.clone(), pixel_size)
        }
    }
}

/// 要画的一条：名称 + 图标来源 + **启动需要的东西**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub source: IconSource,
    /// 条目的 `.lnk` 本身。
    ///
    /// **启动必须用它，不能用 `source.path`。** 用户"更换图标"之后
    /// `source.path` 指向的是**那张图片**，拿它去启动会打开一张图片，
    /// 而不是启动程序 —— 而且不报错，只是行为莫名其妙。
    pub lnk: PathBuf,
    /// 已解析好的目标（`.lnk` 指向的 exe / 文件夹 / shell 项）。
    ///
    /// 文件夹要据此绕开 shell 的 DDE（见 `shell::launch::open_shortcut`），
    /// 所以它必须与 `lnk` 一起推过来，不能在弹窗这边重新解析一次 ——
    /// 重新解析要多一次 COM 调用，而且会与主窗口的结果不一致。
    pub target: PathBuf,
}

/// 主窗口推来的一份内容快照。
///
/// `icon_size` 是主窗口当前的图标**逻辑边长**（DIP）。它必须一起推过来：
/// 弹出窗口只在创建时拿到过一次图标尺寸，之后用户用 `Ctrl+滚轮` 改大小，
/// 它自己不重算就永远停在旧尺寸。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Content {
    pub icon_size: i32,
    pub items: Vec<Item>,
}

/// 请求图标时用的**物理像素**边长。
///
/// 与主窗口 `App::collect_icons` 是同一条规则（同一处夹紧区间）：
/// 按物理像素提取，绘制时才 1:1 对得上 —— 否则 150% 缩放下位图会被放大而发虚。
/// 用物理像素做键也顺带处理了跨显示器拖动：DPI 变了就是另一个键，
/// 不会错误复用低分辨率的旧位图。
pub fn pixel_size(icon_dip: i32, dpi: f32) -> u32 {
    ((icon_dip.max(0) as f32 * dpi / 96.0).round() as i64).clamp(16, 256) as u32
}

/// 一个弹出子窗口的图标状态。
pub struct PopoutIcons {
    loader: Loader,
    cache: IconCache,
    /// 与当前内容一一对应的键。**派生数据**：内容或像素边长变了才重建。
    keys: Vec<Key>,
    /// `keys` 已过期，下次 [`PopoutIcons::collect`] 重建。
    ///
    /// 用一个显式标志而不是"比较内容"：比较要么每帧做一次 O(n) 的路径比对
    ///（每帧做重活），要么退化成"长度 + 某个数"的签名 —— 而签名会漏掉
    /// "条目数没变、但换了内容"（同名改名、两个选项卡对调），
    /// 那正是这个窗口上一轮踩过的坑。置位的人（`Popout`）本来就知道是不是变了。
    stale: bool,
    /// 当前该用多大的位图（物理像素）。
    pixel_size: u32,
}

impl PopoutIcons {
    /// 建一条自己的加载线程。`notify` 用来在图标就绪时唤醒这个窗口重绘。
    pub fn new(notify: RedrawNotifier, pixel_size: u32) -> Self {
        Self {
            loader: Loader::spawn(notify),
            cache: IconCache::new(POPOUT_BUDGET),
            keys: Vec::new(),
            // 初始为空内容，第一次 `collect` 自然会把键建起来
            stale: true,
            pixel_size,
        }
    }

    /// 内容变了。下次绘制时按新内容重建键。
    ///
    /// 由 `Popout` 在收到新快照时调用 —— **不要每帧调**：
    /// 它会丢弃"这批键已经建好"的事实，导致每帧重建（每次都要克隆整张路径表）。
    pub fn invalidate(&mut self) {
        self.stale = true;
    }

    /// 方格边长变了（拖动窗口尺寸、或用户改了图标大小）。
    ///
    /// 尺寸没变时什么都不做：这个方法会在拖动边框时被每帧调用。
    pub fn set_pixel_size(&mut self, pixel_size: u32) {
        if pixel_size == self.pixel_size {
            return;
        }
        self.pixel_size = pixel_size;
        // 键里带着尺寸，所以尺寸一变，旧键全部作废
        self.stale = true;
    }

    /// 收异步结果、转成 D2D 位图存进缓存。
    ///
    /// **必须在 [`PopoutIcons::collect`] 与绘制之前调用完**：`insert` 会触发
    /// LRU 淘汰，边画边淘汰可能把正要画的那张位图释放掉（与主窗口同一条规则）。
    pub fn poll_and_insert(&mut self, ctx: &ID2D1DeviceContext) {
        for item in self.loader.poll() {
            match item.result {
                Ok(bitmap) => {
                    if let Err(e) = self.cache.insert(ctx, item.key, &bitmap) {
                        eprintln!("弹出窗口：图标转位图失败: {e}");
                    }
                }
                // 提取失败（文件没了、路径无图标）：那一格只画底色。
                // **仍然留一条日志** —— 否则用户看到"某个方格一直空着"时，
                // 我们没有任何线索知道是哪一条、为什么。
                // 失败次数由 `Loader` 自己记账（试满就不再请求），不会刷屏。
                Err(e) => crate::diag::log(&format!(
                    "弹出窗口图标提取失败: {} — {e}",
                    item.key.path.display()
                )),
            }
        }
    }

    /// 取前 `count` 条的位图；缺的顺手发起异步加载。
    ///
    /// 返回的 `Vec` 与 `items` 的前 `count` 项一一对应，`None` 表示还没加载好。
    /// 返回的是 `ID2D1Bitmap1` 的**克隆**（COM 引用计数 +1），
    /// 这样绘制代码不必同时借用这个结构 —— 与主窗口 `collect_icons` 同样的理由。
    pub fn collect(&mut self, items: &[Item], count: usize) -> Vec<Option<ID2D1Bitmap1>> {
        if self.stale {
            self.rebuild_keys(items);
        }

        let n = count.min(self.keys.len());
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let key = &self.keys[i];
            match self.cache.get(key) {
                Some(bitmap) => out.push(Some(bitmap.clone())),
                None => {
                    // 已经在途、或已试满次数的会被 `Loader` 自己挡掉，
                    // 所以这里不必再判一次 —— 每帧调它也不会重复提取。
                    self.loader.request(key.clone());
                    out.push(None);
                }
            }
        }
        out
    }

    /// 按当前内容与像素边长重建键表。
    ///
    /// 只在过期时调用（见 [`PopoutIcons::stale`] 的说明）。
    fn rebuild_keys(&mut self, items: &[Item]) {
        self.keys.clear();
        self.keys.reserve(items.len());
        for it in items {
            self.keys.push(it.source.key(self.pixel_size));
        }
        self.stale = false;
    }
}
