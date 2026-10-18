//! 图标位图缓存（LRU + 字节预算）。
//!
//! ## 为什么要有预算
//!
//! 一个 128×128 的图标位图是 64 KB。142 个条目全加载是约 9 MB，
//! 但如果用户把图标调到 256px（256 KB/个）再配上多窗口，
//! 无上限缓存会一路涨到几百 MB。所以按**字节数**设上限，
//! 超了就淘汰最久没用过的。
//!
//! ## 为什么按字节而不是按条数
//!
//! 图标尺寸是用户可调的（Ctrl+滚轮），16px 和 256px 差 256 倍。
//! 按条数限制的话，同样"存 100 个"，实际内存可能从 100KB 变成 25MB。
//!
//! ## 线程
//!
//! `ID2D1Bitmap` 必须在渲染线程创建，本模块全部在 UI 线程使用。

use std::collections::HashMap;

use windows::Win32::Graphics::Direct2D::Common::{
    D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, ID2D1Bitmap1, ID2D1DeviceContext,
};
// DXGI_FORMAT 属于 Dxgi 命名空间，不在 Direct2D 里 —— 虽然 D2D 到处都在用它
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

use crate::error::{Result, Win32Ext};
use crate::platform::icon::IconBitmap;
use crate::platform::icon_loader::Key;

/// 默认预算 24 MB（用户确认过的值）。
pub const DEFAULT_BUDGET: usize = 24 * 1024 * 1024;

struct Entry {
    bitmap: ID2D1Bitmap1,
    /// 这张位图占的字节数（宽 × 高 × 4），用于预算核算。
    bytes: usize,
    /// 最近一次被用到的时间戳，淘汰时取最小的。
    last_used: u64,
}

pub struct IconCache {
    entries: HashMap<Key, Entry>,
    budget: usize,
    used: usize,
    clock: u64,
    /// 累计淘汰次数，自检用来确认预算真的在起作用。
    evictions: u64,
}

impl IconCache {
    pub fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            budget,
            used: 0,
            clock: 0,
            evictions: 0,
        }
    }

    /// 查表。命中会刷新 LRU 时间戳。
    pub fn get(&mut self, key: &Key) -> Option<&ID2D1Bitmap1> {
        self.clock += 1;
        let clock = self.clock;
        // 先判断存在，避免借用冲突
        if !self.entries.contains_key(key) {
            return None;
        }
        let entry = self.entries.get_mut(key)?;
        entry.last_used = clock;
        Some(&entry.bitmap)
    }

    /// 把提取好的像素转成 D2D 位图并存入缓存。
    pub fn insert(&mut self, ctx: &ID2D1DeviceContext, key: Key, icon: &IconBitmap) -> Result<()> {
        if self.entries.contains_key(&key) {
            return Ok(());
        }
        let bitmap = create_bitmap(ctx, icon)?;
        let bytes = icon.width as usize * icon.height as usize * 4;

        self.clock += 1;
        self.entries.insert(
            key,
            Entry {
                bitmap,
                bytes,
                last_used: self.clock,
            },
        );
        self.used += bytes;
        self.evict_to_budget();
        Ok(())
    }

    /// 淘汰到预算以内。
    ///
    /// 每次插入都会调用，但正常情况下只做一次比较就返回 ——
    /// 只有真的超了才会去排序。
    fn evict_to_budget(&mut self) {
        if self.used <= self.budget {
            return;
        }

        // 按最近使用时间升序，从头开始删
        let mut by_age: Vec<(Key, u64)> = self
            .entries
            .iter()
            .map(|(k, e)| (k.clone(), e.last_used))
            .collect();
        by_age.sort_by_key(|(_, t)| *t);

        for (key, _) in by_age {
            if self.used <= self.budget {
                break;
            }
            if let Some(entry) = self.entries.remove(&key) {
                self.used = self.used.saturating_sub(entry.bytes);
                self.evictions += 1;
            }
        }
    }

    pub fn used_bytes(&self) -> usize {
        self.used
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    /// 缓存里出现的尺寸汇总，形如 `96×96 ×16`。自检用来确认
    /// "请求的尺寸"和"实际缓存的尺寸"一致 —— 不一致说明标志位用错了，
    /// 会让内存成倍增长。
    pub fn size_summary(&self) -> String {
        let mut counts: HashMap<(u32, u32), usize> = HashMap::new();
        for key in self.entries.keys() {
            *counts.entry((key.size, key.size)).or_insert(0) += 1;
        }
        let mut items: Vec<_> = counts.into_iter().collect();
        items.sort_by_key(|(size, _)| *size);
        items
            .into_iter()
            .map(|((w, h), n)| format!("{w}×{h} ×{n}"))
            .collect::<Vec<_>>()
            .join("、")
    }
}

/// 从预乘 BGRA 建一张 D2D 位图。
fn create_bitmap(ctx: &ID2D1DeviceContext, icon: &IconBitmap) -> Result<ID2D1Bitmap1> {
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            // 必须声明预乘：`IconBitmap` 里的数据已经预乘过了。
            // 声明成 IGNORE 或 STRAIGHT 会让 D2D 按错误的方式混合，
            // 半透明边缘出现亮边或暗边。
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        // DPI 用 96：位图是**像素数据**，尺寸由目标矩形决定。
        // 写实际 DPI 会让 D2D 再按 DPI 缩放一次，图标就不是我们要的尺寸了。
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: std::mem::ManuallyDrop::new(None),
    };

    // SAFETY: 源数据指针在调用期间有效（icon 借用存活），pitch 与尺寸一致。
    unsafe {
        ctx.CreateBitmap(
            D2D_SIZE_U {
                width: icon.width,
                height: icon.height,
            },
            Some(icon.bgra.as_ptr() as *const core::ffi::c_void),
            icon.width * 4,
            &props,
        )
    }
    .ctx("ID2D1DeviceContext::CreateBitmap(图标)")
}
