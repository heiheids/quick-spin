//! DirectWrite 文本。
//!
//! ## 缓存策略
//!
//! * **文本格式**（字号 / 字重 / 对齐）按角色创建一次后缓存复用；
//! * **文本布局**依赖具体字符串，每次绘制重建。
//!
//! 布局重建看似浪费，实际不是：我们是**按需重绘**（无动画，见架构设计 §3.3），
//! 绘制只发生在状态变化时，空闲时一帧都不画。所以这点分配不在热路径上。
//! 真正要避免的是每帧重建格式对象 —— 那才是会被反复触发的那种浪费。

use std::collections::HashMap;

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows_numerics::Vector2;

use crate::config::theme::Color;
use crate::error::{Result, Win32Ext};

use super::painters::to_d2d;

/// 界面字体。
///
/// 选 `Microsoft YaHei UI` 是因为它同时覆盖中英文且是 Win7 之后系统自带，
/// 不存在"用户没装这个字体"的问题。
const FONT_FAMILY: windows::core::PCWSTR = windows::core::w!("Microsoft YaHei UI");

/// 文本语言标签。影响 DirectWrite 的字形回退选择，
/// 用 `zh-CN` 时中英文都能正确落到合适字形。
const LOCALE: windows::core::PCWSTR = windows::core::w!("zh-CN");

/// 文本角色。用它代替到处传字号与对齐参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// 窗口标题
    Title,
    // 下面三个角色要等侧栏与方格网格接入后才会被构造。
    // 它们与 Title 共用同一套字号/对齐/缓存逻辑，先留在这里比事后再加更清楚。
    // 侧栏与网格落地后请删掉这三个 allow。
    /// 侧栏选项卡文字
    #[allow(dead_code)]
    Tab,
    /// 方格下方的名称
    #[allow(dead_code)]
    Tile,
    /// 次要说明文字
    #[allow(dead_code)]
    Caption,
    /// 正文（左对齐）。设置项的标签用这个。
    Body,
    /// 设置面板的文字。
    ///
    /// 单独一个角色而不是直接把 [`Role::Body`] 调大：`Body` 还被就地编辑框等
    /// 地方用着，改它会连带改掉那些地方的字号。这里只需要设置面板更大一号。
    Setting,
}

impl Role {
    /// 字号（逻辑像素）。
    fn size(self) -> f32 {
        match self {
            // 22 → 18 → 14 → 16（顶部缩过两次后，需求要求再放大一号）。
            // 这个角色只用于窗口标题，改它不影响别处。
            Self::Title => 16.0,
            // 侧栏选项卡是纯文字按钮，全靠字号撑可读性。
            // 13 → 14.5 是需求里的"大 1 号"：13→14 的差别几乎看不出来。
            // 上限受按钮高度约束（22px），再大会顶到框线。
            Self::Tab => 14.5,
            Self::Tile => 12.5,
            Self::Caption => 11.0,
            Self::Body => 13.0,
            // 比 Body 大 2 号（13 → 15）
            Self::Setting => 15.0,
        }
    }

    fn weight(self) -> DWRITE_FONT_WEIGHT {
        match self {
            // 标题略重，与设计稿一致
            Self::Title => DWRITE_FONT_WEIGHT_BOLD,
            _ => DWRITE_FONT_WEIGHT_NORMAL,
        }
    }

    /// 水平对齐。标题与正文靠左、其余居中。
    fn alignment(self) -> DWRITE_TEXT_ALIGNMENT {
        match self {
            // 设置面板的标题与标签都靠左
            Self::Title | Self::Body | Self::Setting => DWRITE_TEXT_ALIGNMENT_LEADING,
            _ => DWRITE_TEXT_ALIGNMENT_CENTER,
        }
    }
}

/// DirectWrite 文本引擎。
pub struct TextEngine {
    factory: IDWriteFactory,
    /// 文本格式缓存。键是角色 —— 字号与对齐都由角色决定，不会变。
    formats: HashMap<Role, IDWriteTextFormat>,
    /// 文字颜色的实色笔刷，按需创建一次。
    brush: Option<ID2D1SolidColorBrush>,
}

impl TextEngine {
    pub fn new() -> Result<Self> {
        // SAFETY: 无参构造，返回接口由 self 持有。
        let factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.ctx("DWriteCreateFactory")?;

        Ok(Self {
            factory,
            formats: HashMap::new(),
            brush: None,
        })
    }

    /// 取（必要时创建）某个角色的文本格式。
    fn format(&mut self, role: Role) -> Result<IDWriteTextFormat> {
        if let Some(existing) = self.formats.get(&role) {
            return Ok(existing.clone());
        }

        // SAFETY: 字体名与语言都是 'static 宽字符串；
        // fontcollection 传 None 表示用系统字体集合。
        let format = unsafe {
            self.factory.CreateTextFormat(
                FONT_FAMILY,
                None::<&IDWriteFontCollection>,
                role.weight(),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                role.size(),
                LOCALE,
            )
        }
        .ctx("IDWriteFactory::CreateTextFormat")?;

        // SAFETY: COM 方法，参数是值类型。
        unsafe {
            format
                .SetTextAlignment(role.alignment())
                .ctx("SetTextAlignment")?;
            format
                .SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)
                .ctx("SetParagraphAlignment")?;
            // 不换行：超出可用宽度时裁掉，而不是把标题挤成两行
            format
                .SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)
                .ctx("SetWordWrapping")?;

            // 超出宽度时以省略号收尾。
            //
            // 不设这个的话，超长名称会被**硬裁**在方格边缘 ——
            // 字被切掉一半，看起来像渲染坏了，而不是"这个名字很长"。
            // 方格里的快捷方式名（"Developer Command Prompt for VS 2022"）
            // 经常超出，所以这个必须处理。
            let trimming = DWRITE_TRIMMING {
                // 按字符截断：中文名没有空格，按词截断会完全不截
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            };
            let ellipsis = self
                .factory
                .CreateEllipsisTrimmingSign(&format)
                .ctx("CreateEllipsisTrimmingSign")?;
            format
                .SetTrimming(&trimming, &ellipsis)
                .ctx("SetTrimming")?;
        }

        self.formats.insert(role, format.clone());
        Ok(format)
    }

    /// 确保文字笔刷已创建。需要可变的设备上下文首次调用。
    pub fn ensure(&mut self, ctx: &ID2D1DeviceContext) -> Result<()> {
        if self.brush.is_none() {
            let white = D2D1_COLOR_F {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            };
            // SAFETY: COM 调用；white 是栈上局部量。
            self.brush = Some(
                unsafe { ctx.CreateSolidColorBrush(&white, None) }.ctx("CreateSolidColorBrush")?,
            );
        }
        Ok(())
    }

    /// 在指定矩形内绘制一段文字。
    ///
    /// `rect` 是 `(left, top, width, height)`，文字按角色的对齐方式与垂直居中排布。
    pub fn draw(
        &mut self,
        ctx: &ID2D1DeviceContext,
        text: &str,
        role: Role,
        rect: (f32, f32, f32, f32),
        color: Color,
    ) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let Some(brush) = self.brush.clone() else {
            return Ok(());
        };

        let format = self.format(role)?;
        let wide: Vec<u16> = text.encode_utf16().collect();

        // SAFETY: 宽字符串与 format/brush 都活到调用结束。
        unsafe {
            let layout = self
                .factory
                .CreateTextLayout(&wide, &format, rect.2, rect.3)
                .ctx("IDWriteFactory::CreateTextLayout")?;

            brush.SetColor(&to_d2d(color));
            ctx.DrawTextLayout(
                Vector2 { X: rect.0, Y: rect.1 },
                &layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }

        Ok(())
    }
}
