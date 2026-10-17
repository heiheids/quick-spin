//! 配色方案。
//!
//! 一套配色 = 一个 [`Scheme`]；多套装进 [`SchemeBook`]，用户可增删（对应"支持更换多色彩配色"）。
//!
//! 关键取舍：悬停、按下、发光这些**状态色不写进配置文件**，
//! 而是由基色与强调色实时派生（见 [`Scheme::finalize`]）。
//! 用户只改一个 `accent` 就能整盘换色，不会出现"改了主色却漏改悬停色导致配色割裂"。
//!
//! 内置方案（含设计稿的复刻）以代码形式分发，不写进配置文件，避免配置臃肿。

// 渲染层接入前，部分取色方法还没有调用方。渲染落地后请删掉本行。
#![allow(dead_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// 配色方案天然要暴露它所用的颜色类型，所以这里公开转出，
// 上层只需 `use config::theme::{Backdrop, Color, Scheme}` 一处即可。
pub use super::color::{Color, Gradient};

/// 用户未指定配色、或指定的名字不存在时，回退到这套内置方案。
pub const DEFAULT_SCHEME: &str = "classic_blue";

/// 窗口背景的模糊方式。
///
/// 各选项的系统要求与降级逻辑在 `platform::acrylic` 里处理，这里只描述"用户想要什么"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backdrop {
    /// 不模糊，只有半透明底色。兼容性最好、开销最低。
    None,
    /// 亚克力毛玻璃，**色调可自定义**。默认选项。
    Acrylic,
    /// Win11 Mica，材质跟随系统壁纸，色调不可自定义。
    Mica,
    /// Win10 风格的系统模糊。
    Blur,
}

impl Default for Backdrop {
    fn default() -> Self {
        Self::Acrylic
    }
}

/// 一整套配色方案。字段名与设计稿元素一一对应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Scheme {
    /// 方案显示名，用于设置界面（例如"深蓝金"）。
    pub name: String,

    // ---- 面板 ----
    /// 面板底色渐变。设计稿是深蓝竖向渐变。
    pub panel: Gradient,
    /// 外描边双色渐变。设计稿是金 → 蓝。
    pub border: Gradient,
    /// 外发光强度 `0.0~1.0`。
    pub glow_strength: f32,
    /// 外发光扩散半径（逻辑像素）。
    pub glow_radius: f32,
    /// 外描边宽度（逻辑像素）。
    pub border_width: f32,

    // ---- 文字 ----
    pub title_text: Color,
    pub text_primary: Color,
    pub text_secondary: Color,

    /// 强调色。悬停底色、选中描边、发光都由它派生。
    pub accent: Color,

    // ---- 侧栏 ----
    /// 侧栏底色（叠在面板之上）。
    pub sidebar_bg: Color,
    /// 悬停态叠色的基准色。
    ///
    /// 原名 `tab_icon_bg`（选项卡图标方块底色）—— 选项卡已改为纯文字按钮，
    /// 图标方块不复存在，但这个颜色仍在派生 `tab_hover`（悬停叠色）。
    /// 按现在真正的用途改名，免得下次有人以为它还管着图标。
    pub hover_base: Color,
    /// 选项卡文字。
    pub tab_label: Color,
    /// 选项卡选中态描边（设计稿里是蓝紫发光描边）。
    pub tab_active_border: Color,

    // ---- 内容区 ----
    /// 方格底色。
    pub tile_bg: Color,

    // ---- 派生值，不参与配置读写 ----
    #[serde(skip)]
    pub tile_hover: Color,
    #[serde(skip)]
    pub tile_active: Color,
    /// 强调色的柔和版本。标题栏按钮与设置齿轮的悬停底色都用它。
    #[serde(skip)]
    pub accent_soft: Color,
    #[serde(skip)]
    pub tab_hover: Color,
    /// 浮层（右键菜单、设置面板）**外侧**那一圈描边。
    ///
    /// **刻意与 `tile_hover` 同色。** 需求原话："把菜单外圈浅色边框颜色调成
    /// 和图标浅色外圈一样"，指的是"鼠标划过方格图标时那圈浅色高亮"。
    ///
    /// 两者本质上是同一种东西 —— 叠在面板上的一层浅色 —— 所以直接取同一个值：
    /// 各存一份颜色迟早会漂开，而"一样"恰恰就是这条需求的内容本身。
    ///
    /// 也因为它是个半透明叠色，画在菜单外沿时与背后的面板混出来的颜色，
    /// 和"鼠标停在方格上"看到的底色是**同一个计算结果**，肉眼看不出差别。
    #[serde(skip)]
    pub overlay_ring: Color,
}

impl Default for Scheme {
    /// 默认值就是设计稿的复刻（内置方案 `classic_blue`）。
    fn default() -> Self {
        Self {
            name: "墨蓝".to_string(),

            panel: Gradient {
                from: Color::new(0xE6, 0x0B, 0x15, 0x24),
                to: Color::new(0xE6, 0x16, 0x2A, 0x4A),
                angle: 90.0,
            },
            // 边框统一成一个颜色（黑色）。
            //
            // 原来是**金色 → 蓝色的渐变**，而选项卡的选中框又是另一种蓝、
            // 未选中的框直接拿文字色当描边 —— 交界处同时出现三种无关的颜色，
            // 看着就像"颜色断了一截"。
            //
            // 仍然用 `Gradient` 类型是刻意的：`from == to` 时它等价于纯色，
            // 这样**不必改结构、不必动 `panel.rs` 的绘制代码**。
            // 想恢复渐变层次时，把两个端点改回不同颜色即可。
            border: Gradient {
                from: Color::new(0xFF, 0x00, 0x00, 0x00),
                to: Color::new(0xFF, 0x00, 0x00, 0x00),
                angle: 135.0,
            },
            glow_strength: 0.75,
            glow_radius: 8.0,
            border_width: 1.5,

            title_text: Color::new(0xFF, 0xF0, 0xF4, 0xFA),
            text_primary: Color::new(0xFF, 0xEA, 0xF0, 0xFA),
            text_secondary: Color::new(0xFF, 0x9F, 0xB3, 0xD0),
            accent: Color::new(0xFF, 0x3A, 0x7B, 0xF0),

            sidebar_bg: Color::new(0x40, 0x00, 0x00, 0x00),
            hover_base: Color::new(0x1A, 0xFF, 0xFF, 0xFF),
            tab_label: Color::new(0xFF, 0xDC, 0xE6, 0xF5),
            tab_active_border: Color::new(0xFF, 0x00, 0x00, 0x00),

            tile_bg: Color::new(0x14, 0xFF, 0xFF, 0xFF),

            tile_hover: Color::TRANSPARENT,
            tile_active: Color::TRANSPARENT,
            accent_soft: Color::TRANSPARENT,
            tab_hover: Color::TRANSPARENT,
            overlay_ring: Color::TRANSPARENT,
        }
    }
}

/// 悬停叠色的不透明度。
///
/// **不能沿用 `tile_bg` / `hover_base` 自己的 alpha**（0x12~0x1C，约 7%~11%）。
/// 那个量级是"方格底色"该有的：它只要在面板上压出一层若有若无的底，
/// 不该抢内容的注意力。但悬停是**唯一的"鼠标在这里"信号** ——
/// 7% 的叠色落在右键菜单、设置面板这种**不透明**底上，只有两三个色阶的差别，
/// 实测反馈正是"划过完全没反应"。
///
/// 0x3D（约 24%）是在"看得见"和"不喧宾夺主"之间取的档：
/// 叠在面板上约提亮 20~35 个色阶，一眼能看出来，又不至于盖住图标和文字。
const HOVER_ALPHA: u8 = 0x3D;

impl Scheme {
    /// 派生交互态颜色。
    ///
    /// **配置载入后、以及用户新增方案时必须调用**，否则悬停态会是全透明。
    /// 由一个**基础色**派生出整套主题。
    ///
    /// 需求："点选一个颜色后就根据基础色变化出深一点的侧边栏色，更深的边框"。
    ///
    /// 分工很明确：
    ///
    /// * **面板**＝基础色本身（两端各偏一档做出层次）
    /// * **侧栏**＝基础色压暗一档
    /// * **边框**＝在 `finalize()` 里按面板再压暗一档（所以这里不设）
    /// * **强调色**＝基础色提亮，保证在面板上看得清
    /// * **文字三级色**＝**按基础色亮度自动选深色系或浅色系**
    ///
    /// 最后一条是这套做法最大的收益：手工配方案时，"暗底配浅字、亮底配深字"
    /// 每套都要重复判断一遍，而这是最容易配错的地方（浅灰和奶白那两套
    /// 就专门处理过叠色）。派生之后这里只写一次，不可能再配错。
    pub fn from_base(base: Color) -> Self {
        let base = base.with_alpha(255);
        let mut s = Self::default();
        // 名字里带上颜色本身（`自定义 #RRGGBB`）。
        //
        // 这**只是为了让 `--diagnose` 和日志能看出当前是哪套颜色**，
        // 不再是缓存失效的依据 —— 面板绘制器已经改成直接比对参与绘制的颜色
        //（见 `panel.rs` 的 `PanelKey`）。当初靠名字失效时，名字一旦固定，
        // 换颜色就不会重建笔刷，表现为"只有一部分界面跟着变色"。
        s.name = format!("自定义 {}", base.to_hex());

        // 面板：基础色本身，一端略亮一端略暗，避免纯平的一块颜色。
        // alpha 用 0xE6 与内置方案一致 —— 它要留给背景材质透出来。
        s.panel = Gradient {
            from: base.lighten(0.06).with_alpha(0xE6),
            to: base.darken(0.06).with_alpha(0xE6),
            angle: 90.0,
        };

        // 侧栏：基础色**压暗一档**，是叠在面板之上的一层。
        // 暗底用半透明白做叠加色，亮底必须用半透明黑 ——
        // 白底上叠白是什么都看不出来的。
        let dark_panel = base.luminance() < 0.5;
        s.sidebar_bg = base.darken(0.25).with_alpha(if dark_panel { 0x40 } else { 0x20 });

        if dark_panel {
            s.title_text = Color::new(0xFF, 0xF0, 0xF4, 0xFA);
            s.text_primary = Color::new(0xFF, 0xEA, 0xF0, 0xFA);
            s.text_secondary = Color::new(0xFF, 0x9F, 0xB3, 0xD0);
            s.tab_label = Color::new(0xFF, 0xDC, 0xE6, 0xF5);
            s.hover_base = Color::new(0x1A, 0xFF, 0xFF, 0xFF);
            s.tile_bg = Color::new(0x14, 0xFF, 0xFF, 0xFF);
        } else {
            s.title_text = Color::new(0xFF, 0x1B, 0x20, 0x28);
            s.text_primary = Color::new(0xFF, 0x24, 0x2A, 0x34);
            s.text_secondary = Color::new(0xFF, 0x63, 0x6E, 0x7E);
            s.tab_label = Color::new(0xFF, 0x2E, 0x36, 0x42);
            s.hover_base = Color::new(0x14, 0x00, 0x00, 0x00);
            s.tile_bg = Color::new(0x12, 0x00, 0x00, 0x00);
        }

        // 强调色提亮：直接拿基础色当强调色的话，暗底上根本看不出来。
        s.accent = base.lighten(0.45);
        s.glow_strength = 0.35;

        // **必须在这里就 finalize，不能交给调用方。**
        //
        // 边框、选项卡选中框、悬停叠色、强调弱化色都是 `finalize()` 派生的，
        // 而 `Self::default()` 里它们是 `Color::TRANSPARENT`。这里先前直接返回
        // `s`，两个调用方（启动载入、点色块换色）也都没补上 ——
        // 结果是**只要用了自定义基础色，整套交互态就是全透明的**：
        // 划过菜单、设置面板、方格什么都不显示（探针实测输出
        // `tile_hover=00000000 tab_hover=00000000 tile_active=00000000 accent_soft=00000000`），
        // 而且"选中"的方格底色也没了，只剩一圈描边。
        //
        // 放在函数内部而不是每个调用点各写一次：这是个"漏一次就静默失效、
        // 且不报任何错"的坑，只有让它不可能被漏掉才算修好。
        // `finalize()` 读的是 `tile_bg` / `panel` / `accent` 这些原始字段，
        // 重复调用是幂等的，所以外层再 finalize 一次也无害。
        s.finalize();
        s
    }

    pub fn finalize(&mut self) {
        // ---- 悬停叠色：统一的"提亮白纱" ----
        //
        // 需求原话："高亮方式为颜色明显变淡"。所以悬停一律往**白色**混，
        // 与强调色无关 —— 强调色由用户在调色板里选，选了偏暗或与底色接近的色，
        // 混出来的悬停色就和底色分不出来（早先往强调色混，反馈正是"不够明显"）。
        //
        // 两件事必须同时做，少一件就还是看不见：
        //
        // 1. **RGB 要推到接近纯白**。基色（`tile_bg` / `hover_base`）是 7% 的
        //    黑纱（浅底）或白纱（深底），RGB 本身就近似无彩色。直接拿它往白
        //    混 20%，得到的仍是深灰 —— 叠在面板上其实是**压暗**，不是提亮。
        // 2. **alpha 要抬到 `HOVER_ALPHA`**，理由见该常量的说明。
        //
        // 只改第 1 件（往白混）而沿用 7% 的 alpha，就是上一版的做法：
        // 叠色本身变了，但太透明，落在不透明底上只有两三个色阶的差别。
        //
        // 提亮方向对深色底和浅色底都成立：深色底提亮、浅色底变淡，
        // 两种都是需求里的"变淡"，所以不需要按底色亮度分两套。
        let hover_white = Color::new(0xFF, 0xFF, 0xFF, 0xFF);
        self.tile_hover = self.tile_bg.with_alpha(HOVER_ALPHA).mix_toward(hover_white, 0.90);
        self.tile_active = self.tile_bg.mix_toward(self.accent, 0.55);
        self.accent_soft = self.accent.with_alpha(self.accent.a() / 3);
        // 选项卡走同一条规则，只是基色换成 `hover_base` ——
        // 保留"每套方案由自己的基色派生"这个结构不变。两档基色一个近白、
        // 一个近黑，推到 0.90 之后都落在白色附近，所以两边强度是一致的。
        //
        // 选项卡的选中态不用这个色（它有自己的 active 底色与描边），
        // 所以悬停和选中不会互相干扰。
        self.tab_hover = self.hover_base.with_alpha(HOVER_ALPHA).mix_toward(hover_white, 0.90);

        // 浮层外圈的描边：**直接取方格悬停高亮的颜色**。
        //
        // 需求要的就是两者一样（见 `overlay_ring` 的字段说明），
        // 所以这里不做任何再派生 —— 取同一个值，它们就不可能不同步。
        //
        // 必须放在 `tile_hover` 之后：这一行读的就是它刚算出来的结果。
        self.overlay_ring = self.tile_hover;

        // ---- 边框：统一取"同色系更深色" ----
        //
        // 需求："边框统一为同色系更深色"。
        //
        // **派生而不是每个方案手写**：原来每套方案的 `border` / `tab_active_border`
        // 都是各自写死的，结果同一个界面上出现了渐变边框、蓝紫描边、文字色描边
        // 三四种互不相关的颜色，交界处看着像"颜色断了一截"。
        // 派生之后，所有方案自动满足同一条规则，新增方案也不会漏配。
        //
        // 算法：面板色**直接再压暗一档**（需求："更深的边框"）。
        //
        // 不混强调色：混了会把色相带走，边框就不再是"同色系更深"了。
        // （早先的版本混了一半强调色，那是为了在近黑的面板上让边框可见；
        // 但代价是边框偏色，与需求的描述不符。）
        let panel_base = self.panel.from.mix_toward(self.panel.to, 0.5).with_alpha(255);
        let deeper = panel_base.darken(0.35);

        // 面板边框：仍然用 `Gradient` 类型（`from == to` 等价纯色），
        // 这样不必改结构、不必动 `panel.rs` 的绘制代码。
        self.border = Gradient {
            from: deeper,
            to: deeper,
            angle: self.border.angle,
        };
        // 选项卡选中框用同一个颜色。选中与未选中的差别只靠**线的粗细和透明度**，
        // 而不是色相 —— 色相一多，交界处就会互相打架。
        self.tab_active_border = deeper;
    }

    /// 套用全局透明度后的面板渐变。
    ///
    /// 透明度只作用于面板底色：侧栏、方格的叠色本身带自己的 alpha，
    /// 它们是"相对面板的加强"，跟着一起变淡反而会让层级关系消失。
    pub fn panel_with_opacity(&self, opacity: f32) -> Gradient {
        self.panel.scale_alpha(opacity)
    }

    /// "平面版本"：把面板渐变两端的不透明度拉满，并**关掉外发光**。
    ///
    /// 用于关掉玻璃效果时 —— 此时应当得到一块**实心、无外溢**的面板，
    /// 而不是"去掉模糊但仍然透出桌面、外面还挂着一圈发光"的半成品。
    /// 那两种观感差别很大：后者看起来像没画完。
    ///
    /// 只改面板与发光，不动侧栏/方格的叠色：那些是相对面板的加强色，
    /// 叠在实心面板上依然成立。
    pub fn flat_variant(&self) -> Self {
        let mut s = self.clone();
        s.panel.from = s.panel.from.with_alpha(255);
        s.panel.to = s.panel.to.with_alpha(255);
        // 发光画在面板**之外**，必须一起关掉，否则窗口边缘仍会有一圈外溢
        s.glow_strength = 0.0;
        s
    }

    /// 亚克力着色调。
    ///
    /// 取面板渐变中点，保证系统模糊层与我们绘制的底色观感一致；
    /// 否则面板调成蓝色而亚克力还是系统灰，会出现两层色偏。
    pub fn acrylic_tint(&self, opacity: f32) -> Color {
        self.panel_with_opacity(opacity).midpoint()
    }

    /// 面板整体明度，供判断是否需要反色文字。
    pub fn panel_luminance(&self) -> f32 {
        self.panel.midpoint().luminance()
    }
}

/// 配色方案集：内置方案 + 用户自定义方案（**同名覆盖内置**）。
pub struct SchemeBook {
    schemes: BTreeMap<String, Scheme>,
}

impl SchemeBook {
    /// 用用户配置里的方案构建方案集。
    pub fn new(user_schemes: &BTreeMap<String, Scheme>) -> Self {
        // 内置方案在 `builtin_schemes()` 里已经逐个走过 `finalized()`，这里不必再来一遍。
        let mut schemes = builtin_schemes();

        for (key, user) in user_schemes {
            let mut s = user.clone();
            // 派生值不落盘，所以每次读入都要重算
            s.finalize();
            schemes.insert(key.clone(), s);
        }

        Self { schemes }
    }

    /// 取方案。名字不存在时回退到内置默认方案，绝不返回 `None`。
    pub fn resolve(&self, key: &str) -> &Scheme {
        self.schemes
            .get(key)
            .or_else(|| self.schemes.get(DEFAULT_SCHEME))
            .expect("内置默认方案必须存在")
    }

    pub fn contains(&self, key: &str) -> bool {
        self.schemes.contains_key(key)
    }

    /// 供设置界面列出可选方案：`(键, 显示名)`。
    pub fn listing(&self) -> Vec<(&str, &str)> {
        self.schemes
            .iter()
            .map(|(k, s)| (k.as_str(), s.name.as_str()))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.schemes.len()
    }
}

/// 内置方案集合。默认英文、中文两套语言都不需要额外文件即可用。
/// 冰蓝：冷调青蓝，比墨蓝亮一档。
fn ice_blue() -> Scheme {
    let mut s = Scheme::default();
    s.name = "冰蓝".to_string();
    s.panel = Gradient {
        from: Color::new(0xE6, 0x0E, 0x18, 0x26),
        to: Color::new(0xE6, 0x16, 0x2A, 0x40),
        angle: 90.0,
    };
    s.glow_strength = 0.35;
    s.accent = Color::new(0xFF, 0x4F, 0xC3, 0xF7);
    s.title_text = Color::new(0xFF, 0xEC, 0xF7, 0xFD);
    s.text_primary = Color::new(0xFF, 0xE4, 0xF2, 0xFB);
    s.text_secondary = Color::new(0xFF, 0x93, 0xB4, 0xC9);
    s.sidebar_bg = Color::new(0x40, 0x00, 0x08, 0x12);
    s.hover_base = Color::new(0x1C, 0xFF, 0xFF, 0xFF);
    s.tab_label = Color::new(0xFF, 0xD6, 0xEA, 0xF7);
    s.tile_bg = Color::new(0x18, 0xFF, 0xFF, 0xFF);
    s
}

/// 鲜橙：暖橙，暗底，适合醒目。
fn orange() -> Scheme {
    let mut s = Scheme::default();
    s.name = "鲜橙".to_string();
    s.panel = Gradient {
        from: Color::new(0xE6, 0x1A, 0x12, 0x0A),
        to: Color::new(0xE6, 0x2A, 0x1C, 0x10),
        angle: 90.0,
    };
    s.glow_strength = 0.40;
    s.accent = Color::new(0xFF, 0xFF, 0x8A, 0x3D);
    s.title_text = Color::new(0xFF, 0xFD, 0xF2, 0xE8);
    s.text_primary = Color::new(0xFF, 0xFA, 0xEE, 0xE2);
    s.text_secondary = Color::new(0xFF, 0xC9, 0xA8, 0x8A);
    s.sidebar_bg = Color::new(0x40, 0x12, 0x08, 0x00);
    s.hover_base = Color::new(0x1C, 0xFF, 0xFF, 0xFF);
    s.tab_label = Color::new(0xFF, 0xF2, 0xDF, 0xCC);
    s.tile_bg = Color::new(0x18, 0xFF, 0xFF, 0xFF);
    s
}

/// 淡灰：明亮中性灰，最不抢眼。
///
/// 亮色主题的叠色要改成**半透明黑**：暗色主题用的半透明白放到白面板上
/// 什么都看不出来，悬停和方格底色会一起消失。
fn gray() -> Scheme {
    let mut s = Scheme::default();
    s.name = "淡灰".to_string();
    s.panel = Gradient {
        from: Color::new(0xF2, 0xF0, 0xF2, 0xF5),
        to: Color::new(0xF2, 0xE0, 0xE4, 0xEA),
        angle: 90.0,
    };
    s.glow_strength = 0.20;
    s.accent = Color::new(0xFF, 0x6B, 0x7A, 0x90);
    s.title_text = Color::new(0xFF, 0x1B, 0x20, 0x28);
    s.text_primary = Color::new(0xFF, 0x24, 0x2A, 0x34);
    s.text_secondary = Color::new(0xFF, 0x63, 0x6E, 0x7E);
    s.sidebar_bg = Color::new(0x14, 0x00, 0x00, 0x00);
    s.hover_base = Color::new(0x14, 0x00, 0x00, 0x00);
    s.tab_label = Color::new(0xFF, 0x2E, 0x36, 0x42);
    s.tile_bg = Color::new(0x12, 0x00, 0x00, 0x00);
    s
}

/// 奶白：暖白，像纸。
fn cream() -> Scheme {
    let mut s = Scheme::default();
    s.name = "奶白".to_string();
    s.panel = Gradient {
        from: Color::new(0xF2, 0xFB, 0xF7, 0xEF),
        to: Color::new(0xF2, 0xF2, 0xEA, 0xDC),
        angle: 90.0,
    };
    s.glow_strength = 0.22;
    s.accent = Color::new(0xFF, 0xC9, 0xA2, 0x27);
    s.title_text = Color::new(0xFF, 0x2A, 0x24, 0x18);
    s.text_primary = Color::new(0xFF, 0x33, 0x2C, 0x1E);
    s.text_secondary = Color::new(0xFF, 0x76, 0x6A, 0x54);
    s.sidebar_bg = Color::new(0x14, 0x00, 0x00, 0x00);
    s.hover_base = Color::new(0x14, 0x00, 0x00, 0x00);
    s.tab_label = Color::new(0xFF, 0x3C, 0x34, 0x24);
    s.tile_bg = Color::new(0x12, 0x00, 0x00, 0x00);
    s
}

/// 牛油果绿：低饱和绿，柔和。
fn avocado() -> Scheme {
    let mut s = Scheme::default();
    s.name = "牛油果绿".to_string();
    s.panel = Gradient {
        from: Color::new(0xE6, 0x10, 0x1A, 0x12),
        to: Color::new(0xE6, 0x1A, 0x2A, 0x1E),
        angle: 90.0,
    };
    s.glow_strength = 0.35;
    s.accent = Color::new(0xFF, 0x7C, 0xB3, 0x42);
    s.title_text = Color::new(0xFF, 0xEE, 0xF7, 0xE8);
    s.text_primary = Color::new(0xFF, 0xE6, 0xF2, 0xE0);
    s.text_secondary = Color::new(0xFF, 0x9C, 0xB8, 0x90);
    s.sidebar_bg = Color::new(0x40, 0x00, 0x0A, 0x04);
    s.hover_base = Color::new(0x1C, 0xFF, 0xFF, 0xFF);
    s.tab_label = Color::new(0xFF, 0xDA, 0xEC, 0xD2);
    s.tile_bg = Color::new(0x18, 0xFF, 0xFF, 0xFF);
    s
}

/// 设置面板里"基础色"色块网格的候选色。
///
/// **按色相分行、明度分列**排（配合布局里的列数），手写而不是用 HSV 生成：
/// 生成出来的颜色在中间明度会发灰、饱和度也不受控，而手写能挑到好看的档位。
///
/// 顺序就是显示顺序，布局按每行 8 个铺开。
pub const BASE_PALETTE: &[&str] = &[
    // 蓝
    "#0A1A2F", "#123A63", "#1D5FA8", "#3A8FE0", "#7FC0F5", "#C7E4FA", "#0E2A45", "#2B6FBF",
    // 青
    "#062B2E", "#0E5A60", "#159AA4", "#2FC4D0", "#8CE6EE", "#D2F6F9", "#0A3D42", "#1F8791",
    // 绿
    "#0A2411", "#14522A", "#1F8A43", "#3FBF6B", "#96E3B4", "#D8F5E4", "#0F3A1E", "#2A9E55",
    // 牛油果 / 黄绿
    "#1A2410", "#3D5220", "#6E8F35", "#A3C55C", "#CFE39C", "#EEF5D6", "#2A3A18", "#84A845",
    // 橙
    "#2A1608", "#5E3012", "#A85A1E", "#E08A38", "#F5C182", "#FBE6C9", "#48240C", "#C47428",
    // 红
    "#2E0A0E", "#66141C", "#B02430", "#E04A55", "#F59AA0", "#FBD7DA", "#4D0E14", "#CC3844",
    // 紫
    "#1E0A2E", "#44145E", "#7A24A8", "#AE4AE0", "#D49AF5", "#EDD7FB", "#330E48", "#9428C4",
    // 灰阶
    "#000000", "#14161A", "#282C33", "#4A5058", "#767D88", "#A8AFB8", "#D6DAE0", "#F2F4F7",
];

/// 色块网格每行几个。改它要同步 `layout.rs` 里的同名常量。
pub const BASE_PALETTE_COLS: usize = 8;

fn builtin_schemes() -> BTreeMap<String, Scheme> {
    let mut map = BTreeMap::new();
    map.insert("classic_blue".to_string(), finalized(Scheme::default()));
    // 这里只有"墨蓝"加五套派生方案。早先手调的午夜灰 / 紫蓝霓虹 / 浅色已删除：
    // 界面上够不着（色板取代了循环切换），且它们的边框是手写死的、
    // 早已被 `finalize()` 的派生覆盖，显示出来已不是当初调的样子。
    map.insert("ice_blue".to_string(), finalized(ice_blue()));
    map.insert("orange".to_string(), finalized(orange()));
    map.insert("gray".to_string(), finalized(gray()));
    map.insert("cream".to_string(), finalized(cream()));
    map.insert("avocado".to_string(), finalized(avocado()));
    map
}

fn finalized(mut s: Scheme) -> Scheme {
    s.finalize();
    s
}
