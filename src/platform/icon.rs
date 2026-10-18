//! 图标提取：把任意路径的图标取成 Direct2D 能直接用的像素。
//!
//! ## 为什么用 `IShellItemImageFactory` 而不是 `SHGetImageList`
//!
//! 常规做法是 `SHGetFileInfoW(SHGFI_SYSICONINDEX)` 拿系统图标索引，
//! 再用 `SHGetImageList(SHIL_JUMBO)` 取 256×256 的大图标。
//!
//! 但 **`windows` crate 0.62.2 没有生成 `IImageList`**（`Win32::UI::Shell` 里
//! 找不到它，只有 `Win32::System::Mmc` 下同名但无关的类型），
//! 所以那条路走不通。
//!
//! `IShellItemImageFactory::GetImage` 反而更合适：
//! * 直接按请求尺寸返回 `HBITMAP`，不用自己缩放；
//! * `.lnk`、shell 命名空间项（文件资源管理器、控制面板）、普通 exe 一视同仁；
//! * 由系统决定用哪一种图标来源（内嵌资源 / 图标索引 / 缩略图）。
//!
//! ## 线程与 COM
//!
//! 本模块**不**初始化 COM —— 由调用方保证当前线程已 `CoInitialize`。
//! 图标提取会被放在后台线程里做，避免拖慢界面。
//!
//! ## 预乘 alpha
//!
//! `GetDIBits` 给出的 BGRA 里 alpha 通道是**直通（straight）**的，
//! 而 Direct2D 要求**预乘（premultiplied）**。
//! 不换算的话，半透明边缘会因为颜色分量偏亮而出现一圈亮边。
//! 所以这里统一做一次预乘，上层拿到就能直接 `CreateBitmap`。

use std::path::Path;

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DIBSECTION,
    DeleteDC, DeleteObject, GetDIBits, GetObjectW, HGDIOBJ,
};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF, SIIGBF_ICONONLY,
};
use windows::core::HSTRING;

use crate::error::{Result, Win32Ext};

/// 允许的尺寸范围。
///
/// 下限 16：再小系统也只会给 16×16，没有意义。
/// 上限 256：这是 shell 图标的最大档（"超大图标"），
/// 再往上只会得到拉伸后的模糊图，白占内存。
const MIN_SIZE: u32 = 16;
const MAX_SIZE: u32 = 256;

/// 提取出来的图标位图。
#[derive(Clone)]
pub struct IconBitmap {
    pub width: u32,
    pub height: u32,
    /// 长度 = `width * height * 4`，BGRA 排列，**已预乘 alpha**。
    /// 可直接喂给 `ID2D1DeviceContext::CreateBitmap`。
    pub bgra: Vec<u8>,
}

impl IconBitmap {
    /// 占用的字节数。图标缓存按这个算总预算。
    pub fn bytes(&self) -> usize {
        self.bgra.len()
    }

    /// alpha 非零的像素占比（0.0~1.0）。用于自检判断"是不是拿到了一张有效图像"。
    pub fn opaque_ratio(&self) -> f32 {
        if self.bgra.is_empty() {
            return 0.0;
        }
        let opaque = self.bgra.chunks_exact(4).filter(|p| p[3] != 0).count();
        opaque as f32 / (self.bgra.len() / 4) as f32
    }

    /// 检查是否满足预乘的上界：任一颜色分量都不应超过 alpha。
    ///
    /// 预乘的定义决定了 `channel <= alpha` 恒成立；
    /// 违反说明数据是直通 alpha 或转换时算错了。
    pub fn premultiply_violations(&self) -> usize {
        self.bgra
            .chunks_exact(4)
            .filter(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3])
            .count()
    }
}

/// 从**图片文件**（PNG / JPG / BMP / GIF…）提取缩放到指定边长的位图。
///
/// 与 [`extract`] 的区别只在标志位：这里**不加** `SIIGBF_ICONONLY`，
/// 所以 shell 会返回图片本身的缩略图而不是"文件的类型图标"。
///
/// 复用同一条转换链路（`IShellItemImageFactory` → HBITMAP → 预乘 BGRA），
/// 因此**不需要**引入 WIC 或 GDI+ 之类的解码库 —— 解码、缩放、
/// 旋转（按 EXIF）全部由 shell 完成，还自带缩略图缓存。
///
/// 非图片文件（exe/dll/lnk）调用它也能用，只是拿到的是类型图标，
/// 所以"更换图标"其实可以不区分文件类型，一律走这里。
pub fn extract_image(path: &Path, size: u32) -> Result<IconBitmap> {
    let size = size.clamp(MIN_SIZE, MAX_SIZE);
    let path_str = path.to_string_lossy();
    let wide = HSTRING::from(path_str.as_ref());

    let factory: IShellItemImageFactory =
        unsafe { SHCreateItemFromParsingName(&wide, None) }.ctx("SHCreateItemFromParsingName")?;

    // SAFETY: COM 调用。
    let hbm = unsafe {
        factory.GetImage(
            SIZE {
                cx: size as i32,
                cy: size as i32,
            },
            // 默认值 0 = `SIIGBF_RESIZETOFIT`，即"缩放到指定尺寸"。
            // 不加 ICONONLY 才会走缩略图路径。
            SIIGBF(0),
        )
    }
    .ctx("IShellItemImageFactory::GetImage(图片)")?;

    let pixels = bitmap_to_bgra(hbm);
    // SAFETY: hbm 由 GetImage 返回，所有权归我们，此后不再使用。
    unsafe {
        let _ = DeleteObject(HGDIOBJ(hbm.0));
    }
    pixels
}

/// 把图标写成**.png 文件**（直通 alpha，不带任何背景）。
///
/// 与 [`write_preview_png`] 的区别：那个是给人看的调试图（合成在棋盘格上），
/// 这个是给程序自己用的存储格式，必须保留原始 alpha，
/// 否则再次读回来时透明区域会变成棋盘格图案。
pub fn write_png(icon: &IconBitmap, path: &Path) -> std::io::Result<()> {
    let w = icon.width as usize;
    let h = icon.height as usize;

    let mut raw = Vec::with_capacity(h * (1 + w * 4));
    for y in 0..h {
        raw.push(0u8);
        for x in 0..w {
            let i = (y * w + x) * 4;
            let (b, g, r, a) = (
                icon.bgra[i] as u32,
                icon.bgra[i + 1] as u32,
                icon.bgra[i + 2] as u32,
                icon.bgra[i + 3] as u32,
            );
            // 从预乘还原成直通，PNG 存的是直通 alpha
            let (sr, sg, sb) = if a == 0 {
                (0, 0, 0)
            } else {
                (
                    (r * 255 / a).min(255),
                    (g * 255 / a).min(255),
                    (b * 255 / a).min(255),
                )
            };
            raw.extend_from_slice(&[sr as u8, sg as u8, sb as u8, a as u8]);
        }
    }

    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 位、RGBA、无压缩/滤波/隔行
    push_chunk(&mut png, b"IHDR", &ihdr);
    push_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    push_chunk(&mut png, b"IEND", &[]);

    std::fs::write(path, png)
}

/// 从路径提取图标。
///
/// 调用线程必须已初始化 COM。
pub fn extract(path: &Path, size: u32) -> Result<IconBitmap> {
    let size = size.clamp(MIN_SIZE, MAX_SIZE);

    let path_str = path.to_string_lossy();
    let wide = HSTRING::from(path_str.as_ref());

    // SAFETY: COM 调用；`wide` 在本次调用期间存活。
    //
    // 这里不用 SHGFI_USEFILEATTRIBUTES：我们要的是**真实**图标，
    // 不是按扩展名猜出来的通用图标。
    let factory: IShellItemImageFactory =
        unsafe { SHCreateItemFromParsingName(&wide, None) }.ctx("SHCreateItemFromParsingName")?;

    // SAFETY: COM 调用。
    //
    // 标志位说明：
    // * `SIIGBF_ICONONLY` —— 只要图标，不要缩略图。否则图片文件会返回缩略图，
    //   与"所有条目都是图标"的视觉一致性冲突。
    //
    // **刻意不加 `SIIGBF_BIGGERSIZEOK`**：它的语义是"有更大尺寸就用更大的"，
    // 于是请求 96px 会拿回 256×256 的图 —— 内存变成所需的 7 倍，
    // 而且让"按尺寸分键"的缓存预算失去意义（不同请求尺寸实际都缓存同一张大图）。
    // 缩放到目标尺寸交给 shell 自己的缩放器，质量足够，
    // 而我们的方格本来就是画在固定尺寸里。
    let hbm = unsafe {
        factory.GetImage(
            SIZE {
                cx: size as i32,
                cy: size as i32,
            },
            SIIGBF_ICONONLY,
        )
    }
    .ctx("IShellItemImageFactory::GetImage")?;

    // 从这一行起，无论走哪条路径都必须释放 hbm。
    // 这个函数会被几百个条目调用，泄漏会直接体现在任务管理器里。
    let pixels = bitmap_to_bgra(hbm);
    // SAFETY: hbm 由 GetImage 返回，所有权归我们，且此后不再使用。
    unsafe {
        let _ = DeleteObject(HGDIOBJ(hbm.0));
    }
    pixels
}

/// 把 `HBITMAP` 读成预乘 BGRA。不负责释放 `hbm`。
///
/// 优先走 DIB section 直读，失败才退回 `GetDIBits`。原因见 [`read_dib_section`]。
fn bitmap_to_bgra(hbm: windows::Win32::Graphics::Gdi::HBITMAP) -> Result<IconBitmap> {
    // 这两个都是安全函数，各自内部处理了 unsafe —— 这里不需要 unsafe 块
    if let Some(icon) = read_dib_section(hbm) {
        return Ok(icon);
    }
    read_via_get_dibits(hbm)
}

/// 直接读 DIB section 内部的像素指针。
///
/// **这是首选的读法。** `IShellItemImageFactory::GetImage` 返回的是一张
/// 32 位 DIB section，alpha 通道是有效的；但 `GetDIBits` 在转换时
/// **可能把 alpha 字节清零**（32bpp + `BI_RGB` 的第四字节不受文档保证），
/// 结果是整张图几乎全透明、图标渲染出来看不见。
///
/// 直读 `dsBm.bmBits` 拿到的是原始像素，alpha 原样保留，还省掉一次拷贝。
/// 返回 `None` 表示这张位图不是 DIB section（或有其它意外），交给调用方走退路。
fn read_dib_section(hbm: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<IconBitmap> {
    // SAFETY: 调用方保证 hbm 有效；结构体是栈上局部量。
    unsafe {
        let mut dib = DIBSECTION::default();
        let got = GetObjectW(
            HGDIOBJ(hbm.0),
            std::mem::size_of::<DIBSECTION>() as i32,
            Some(&mut dib as *mut DIBSECTION as *mut core::ffi::c_void),
        );
        // 只有填满整个 DIBSECTION 才说明确实是 DIB section
        if got != std::mem::size_of::<DIBSECTION>() as i32 {
            return None;
        }
        let bits = dib.dsBm.bmBits as *const u8;
        if bits.is_null() {
            return None;
        }

        let width = dib.dsBmih.biWidth;
        // biHeight 为负表示自上而下 —— 那正是 D2D 期望的行序
        let raw_height = dib.dsBmih.biHeight;
        let top_down = raw_height < 0;
        let height = raw_height.unsigned_abs();
        if width <= 0 || height == 0 {
            return None;
        }
        let width = width as usize;
        let height = height as usize;

        // 32bpp 的行距按 4 字节对齐，宽本身就满足，所以就是 width*4
        let pitch = width * 4;
        let mut buffer = vec![0u8; pitch * height];

        if top_down {
            std::ptr::copy_nonoverlapping(bits, buffer.as_mut_ptr(), buffer.len());
        } else {
            // 自下而上：逐行倒着拷
            for row in 0..height {
                let src = bits.add((height - 1 - row) * pitch);
                std::ptr::copy_nonoverlapping(src, buffer.as_mut_ptr().add(row * pitch), pitch);
            }
        }

        premultiply(&mut buffer);
        repair_empty_alpha(&mut buffer);

        Some(IconBitmap {
            width: width as u32,
            height: height as u32,
            bgra: buffer,
        })
    }
}

/// 退路：用 `GetDIBits` 读。位图不是 DIB section 时才会走到这里。
fn read_via_get_dibits(hbm: windows::Win32::Graphics::Gdi::HBITMAP) -> Result<IconBitmap> {
    // SAFETY: 整个函数体都是 GDI 调用；hdc 在末尾关闭。
    unsafe {
        let mut bmp = BITMAP::default();
        let got = GetObjectW(
            HGDIOBJ(hbm.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bmp as *mut BITMAP as *mut core::ffi::c_void),
        );
        if got == 0 {
            return Err(crate::error::AppError::Win32 {
                context: "GetObjectW(图标位图)",
                code: 0,
            });
        }

        let width = bmp.bmWidth.max(1) as u32;
        let height = bmp.bmHeight.max(1) as u32;

        let hdc = CreateCompatibleDC(None);
        if hdc.is_invalid() {
            return Err(crate::error::AppError::Win32 {
                context: "CreateCompatibleDC(读取图标)",
                code: 0,
            });
        }

        // 把工作放进闭包，保证 hdc 一定被 DeleteDC —— 提前 return 也不漏。
        let out = (|| -> Result<IconBitmap> {
            // biHeight 取**负值**表示自上而下的行序。
            // 正值是 Windows 默认的自下而上，那样还得再翻转一次内存，
            // 而 D2D 期望的正是自上而下 —— 用负高度省掉一次整块内存的搬运。
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    // 这个版本里字段是裸 u32，不是 BI_COMPRESSION 新类型
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };

            let mut buffer = vec![0u8; width as usize * height as usize * 4];

            let lines = GetDIBits(
                hdc,
                hbm,
                0,
                height,
                Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
                &mut info,
                DIB_RGB_COLORS,
            );
            if lines == 0 {
                return Err(crate::error::AppError::Win32 {
                    context: "GetDIBits(读取图标像素)",
                    code: 0,
                });
            }

            premultiply(&mut buffer);
            repair_empty_alpha(&mut buffer);

            Ok(IconBitmap {
                width,
                height,
                bgra: buffer,
            })
        })();

        let _ = DeleteDC(hdc);
        out
    }
}

/// 直通 alpha → 预乘 alpha。
///
/// Direct2D 的位图默认按预乘解释；喂直通数据会得到一圈亮边。
#[inline]
fn premultiply(buffer: &mut [u8]) {
    for px in buffer.chunks_exact_mut(4) {
        let a = px[3] as u32;
        // 整型乘除比浮点快，且 8 位量级下舍入误差无关紧要
        px[0] = (px[0] as u32 * a / 255) as u8;
        px[1] = (px[1] as u32 * a / 255) as u8;
        px[2] = (px[2] as u32 * a / 255) as u8;
    }
}

/// 整张图 alpha 全 0 时补成不透明。
///
/// 少数旧式图标只给了 XOR/AND 掩码、没写 alpha 通道，
/// 直接交给 D2D 会**整个图标不可见**。这类图标的掩码信息我们没读，
/// 退化成"当作不透明"至少能看见图形，比什么都没有强。
fn repair_empty_alpha(buffer: &mut [u8]) {
    let all_zero = buffer.chunks_exact(4).all(|px| px[3] == 0);
    if all_zero {
        for px in buffer.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
}

// ---------------------------------------------------------------------------
// 调试图导出
// ---------------------------------------------------------------------------

/// 把图标写成 PNG 预览，**合成在棋盘格背景上**。
///
/// 为什么值得专门写一个 PNG 编码器（而不是只看统计数字）：
/// 图标提取出错的方式都是"数值看着合理但图像不对"—— 上下颠倒、
/// alpha 丢失、行距算错导致斜切。这类问题只有真的看一眼才能确认，
/// 而看数字只能猜到"有效像素比例偏低"这种间接信号。
///
/// 棋盘格背景的用处：alpha 正确时能看清图标边缘与背景的过渡；
/// alpha 全丢时会看到纯棋盘格，一眼可辨。
///
/// 只用于命令行自检，不进主程序。
pub fn write_preview_png(icon: &IconBitmap, path: &Path) -> std::io::Result<()> {
    const CHECKER: u32 = 8;

    let w = icon.width as usize;
    let h = icon.height as usize;

    // RGBA 扫描行，每行前加一个 filter 字节（0 = 不滤波）
    let mut raw = Vec::with_capacity(h * (1 + w * 4));
    for y in 0..h {
        raw.push(0u8);
        for x in 0..w {
            let i = (y * w + x) * 4;
            let (b, g, r, a) = (
                icon.bgra[i] as u32,
                icon.bgra[i + 1] as u32,
                icon.bgra[i + 2] as u32,
                icon.bgra[i + 3] as u32,
            );

            // 还原成直通色，才能和背景做正常的 alpha 混合
            let (sr, sg, sb) = if a == 0 {
                (0, 0, 0)
            } else {
                (
                    (r * 255 / a).min(255),
                    (g * 255 / a).min(255),
                    (b * 255 / a).min(255),
                )
            };

            // 棋盘格：浅灰 / 深灰
            let light = ((x / CHECKER as usize) + (y / CHECKER as usize)) % 2 == 0;
            let bg = if light { 0xC8u32 } else { 0x96u32 };

            let mix = |c: u32| ((c * a + bg * (255 - a)) / 255) as u8;
            raw.extend_from_slice(&[mix(sr), mix(sg), mix(sb), 255]);
        }
    }

    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 位、RGBA、无压缩/滤波/隔行
    push_chunk(&mut png, b"IHDR", &ihdr);
    push_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    push_chunk(&mut png, b"IEND", &[]);

    std::fs::write(path, png)
}

fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// 用 zlib 的"存储块"（不压缩）打包。调试图不值得为此引入压缩库。
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // zlib 头：默认压缩级别、无字典
    let mut rest = data;
    loop {
        let take = rest.len().min(65535);
        let last = take == rest.len();
        out.push(if last { 1 } else { 0 });
        out.extend_from_slice(&(take as u16).to_le_bytes());
        out.extend_from_slice(&(!(take as u16)).to_le_bytes());
        out.extend_from_slice(&rest[..take]);
        rest = &rest[take..];
        if last {
            break;
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
