//! 颜色与渐变。
//!
//! 颜色统一用 `0xAARRGGBB` 单值表示：alpha 通道本身就是"透明化"旋钮，
//! 一个值同时表达颜色和透明度，不需要额外维护一堆 opacity 字段。

// 本模块是绘图基础件，其中一部分（to_abgr / to_colorref / endpoints 等）
// 要等渲染层接入后才会被调用。渲染落地后请删掉本行。
#![allow(dead_code)]

use std::fmt;

use serde::de::{self, Deserializer, Visitor};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

/// `0xAARRGGBB` 颜色。
///
/// 序列化统一写成 `"0xAARRGGBB"`；反序列化额外兼容
/// `0xCC1E1E2E`（TOML 十六进制整数）、`"#CC1E1E2E"`、`"CC1E1E2E"` 和十进制整数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    pub const TRANSPARENT: Self = Self(0);
    pub const WHITE: Self = Self(0xFFFF_FFFF);

    pub const fn new(a: u8, r: u8, g: u8, b: u8) -> Self {
        Self(((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32))
    }

    pub const fn a(self) -> u8 {
        (self.0 >> 24) as u8
    }
    pub const fn r(self) -> u8 {
        (self.0 >> 16) as u8
    }
    pub const fn g(self) -> u8 {
        (self.0 >> 8) as u8
    }
    pub const fn b(self) -> u8 {
        self.0 as u8
    }

    /// 只改透明度、保留色相。
    pub const fn with_alpha(self, a: u8) -> Self {
        Self((self.0 & 0x00FF_FFFF) | ((a as u32) << 24))
    }

    /// 按倍率缩放透明度，供全局 opacity 这类滑杆使用。
    pub fn scale_alpha(self, factor: f32) -> Self {
        let a = (self.a() as f32 * factor.clamp(0.0, 1.0)).round() as u8;
        self.with_alpha(a)
    }

    /// 亚克力着色要求的 **ABGR** 布局（`AccentPolicy::GradientColor`）。
    ///
    /// 这是 `SetWindowCompositionAttribute` 的坑点：它既不是 ARGB 也不是 COLORREF，
    /// 而是把 R 和 B 对调的 ABGR。集中在这里转一次，避免各处重复踩坑。
    pub const fn to_abgr(self) -> u32 {
        ((self.a() as u32) << 24)
            | ((self.b() as u32) << 16)
            | ((self.g() as u32) << 8)
            | (self.r() as u32)
    }

    /// `COLORREF`（`0x00BBGGRR`），GDI 与部分 DWM 接口使用。
    pub const fn to_colorref(self) -> u32 {
        ((self.b() as u32) << 16) | ((self.g() as u32) << 8) | (self.r() as u32)
    }

    /// 朝另一色线性插值，`t ∈ [0,1]`；alpha 保持自身不变。
    pub fn mix_toward(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let lerp = |s: u8, d: u8| (s as f32 + (d as f32 - s as f32) * t).round() as u8;
        Self::new(
            self.a(),
            lerp(self.r(), other.r()),
            lerp(self.g(), other.g()),
            lerp(self.b(), other.b()),
        )
    }

    /// 朝白色提亮。
    pub fn lighten(self, t: f32) -> Self {
        self.mix_toward(Self::new(self.a(), 255, 255, 255), t)
    }

    /// 朝黑色压暗。
    pub fn darken(self, t: f32) -> Self {
        self.mix_toward(Self::new(self.a(), 0, 0, 0), t)
    }

    /// 人眼感知亮度 `0.0~1.0`。用于判断某个底色上该配深色还是浅色文字。
    pub fn luminance(self) -> f32 {
        (0.2126 * self.r() as f32 + 0.7152 * self.g() as f32 + 0.0722 * self.b() as f32) / 255.0
    }

    /// 解析 `#RRGGBB` / `RRGGBB` / `#RGB` 形式的十六进制颜色。
    ///
    /// 配置里存的是文本（人要能读写），所以需要这个入口。
    /// 解析失败返回 `None` 而不是回退到某个默认色：**静默回退会掩盖拼写错误**，
    /// 用户改了个错字却看到颜色没变，会以为是程序坏了。
    pub fn from_hex(text: &str) -> Option<Self> {
        let t = text.trim().trim_start_matches('#');
        let parse = |s: &str| u8::from_str_radix(s, 16).ok();
        match t.len() {
            // #RGB 简写：每位重复一次（F → FF），这是 CSS 的既有约定
            3 => {
                let mut it = t.chars();
                let r = it.next()?;
                let g = it.next()?;
                let b = it.next()?;
                let expand = |c: char| {
                    let v = c.to_digit(16)? as u8;
                    Some(v * 16 + v)
                };
                Some(Self::new(0xFF, expand(r)?, expand(g)?, expand(b)?))
            }
            6 => Some(Self::new(
                0xFF,
                parse(&t[0..2])?,
                parse(&t[2..4])?,
                parse(&t[4..6])?,
            )),
            _ => None,
        }
    }

    /// 序列化成 `#RRGGBB`，与 [`Self::from_hex`] 互逆。
    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r(), self.g(), self.b())
    }
}

impl Serialize for Color {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("0x{:08X}", self.0))
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ColorVisitor;

        impl<'de> Visitor<'de> for ColorVisitor {
            type Value = Color;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("0xAARRGGBB 颜色，例如 \"0xCC1E1E2E\" 或 0xCC1E1E2E")
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Color, E> {
                u32::try_from(v)
                    .map(Color)
                    .map_err(|_| E::custom("颜色值超出 u32 范围"))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Color, E> {
                u32::try_from(v)
                    .map(Color)
                    .map_err(|_| E::custom("颜色值必须是非负整数"))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Color, E> {
                parse_color(v).ok_or_else(|| E::custom(format!("无法解析颜色: {v}")))
            }
        }

        deserializer.deserialize_any(ColorVisitor)
    }
}

/// 解析颜色文本。
///
/// 带 `0x` / `#` 前缀按十六进制；无前缀且正好 8 位十六进制字符也按十六进制；
/// 其余按十进制。因此 `"0xCC1E1E2E"`、`"#CC1E1E2E"`、`"CC1E1E2E"` 都可用。
fn parse_color(text: &str) -> Option<Color> {
    let t = text.trim();
    let t = t.strip_prefix('#').unwrap_or(t);

    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).ok().map(Color);
    }
    if t.len() == 8 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        return u32::from_str_radix(t, 16).ok().map(Color);
    }
    t.parse::<u32>().ok().map(Color)
}

/// 线性渐变。设计稿的面板底色与外描边都是双色渐变。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gradient {
    pub from: Color,
    pub to: Color,
    /// 角度（度）。`0` = 自左向右，`90` = 自上而下。
    pub angle: f32,
}

impl Default for Gradient {
    fn default() -> Self {
        Self {
            from: Color::new(0xE6, 0x0B, 0x15, 0x24),
            to: Color::new(0xE6, 0x16, 0x2A, 0x4A),
            angle: 90.0,
        }
    }
}

impl Gradient {
    /// 按倍率缩放两端透明度，供全局 opacity 使用。
    pub fn scale_alpha(self, factor: f32) -> Self {
        Self {
            from: self.from.scale_alpha(factor),
            to: self.to.scale_alpha(factor),
            angle: self.angle,
        }
    }

    /// 中点色。用于派生亚克力着色调等"单色"场景。
    pub fn midpoint(self) -> Color {
        self.from.mix_toward(self.to, 0.5)
    }

    /// 换算出 Direct2D 线性渐变笔刷需要的起止点。
    ///
    /// 入参是目标矩形 `(left, top, right, bottom)`，返回 `(起点, 终点)`。
    pub fn endpoints(self, rect: (f32, f32, f32, f32)) -> ((f32, f32), (f32, f32)) {
        let (l, t, r, b) = rect;
        let (cx, cy) = ((l + r) / 2.0, (t + b) / 2.0);

        // 角度 0 指向 +x，90 指向 +y（屏幕坐标 y 向下）
        let rad = self.angle.to_radians();
        let (dx, dy) = (rad.cos(), rad.sin());

        // 取包围盒在方向向量上的投影半径，保证渐变正好铺满矩形
        let half = ((r - l) * dx.abs() + (b - t) * dy.abs()) / 2.0;
        (
            (cx - dx * half, cy - dy * half),
            (cx + dx * half, cy + dy * half),
        )
    }
}
