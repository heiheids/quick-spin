//! 多语言。
//!
//! 设计要点：
//! * 中文、英文词典**编译进程序**，不存在"语言文件丢失导致界面显示成键名"的问题；
//! * 用户可在 `%APPDATA%\QuickSpin\lang\<locale>.toml` 覆盖任意条目，
//!   也可以据此新增语言，无需重新编译；
//! * [`I18n::t`] 返回 `&str`、不做分配。绘制列表只在失效时重建
//!   （见架构设计 §3.3），因此查表不会进入每帧热路径。

// 外部语言文件目录与语言列表要等设置界面接入后才会用到。
// 设置界面落地后请删掉本行。
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;

use windows::Win32::Globalization::GetUserDefaultLocaleName;

mod builtin;

// 供上层按需引用；`dict_for` 也通过这两个常量取表。
pub use builtin::{Dict, EN_US, ZH_CN};

/// 内置支持的语言：`(语言代码, 母语名)`。
pub const BUILTIN_LOCALES: &[(&str, &str)] = &[("zh-CN", "简体中文"), ("en-US", "English")];

/// 查不到译文时兜底的语言。
const FALLBACK_LOCALE: &str = "en-US";

/// Win32 `LOCALE_NAME_MAX_LENGTH`。
const LOCALE_NAME_MAX_LENGTH: usize = 85;

/// 语言表：内置词典 + 外部覆盖。
pub struct I18n {
    locale: String,
    primary: Dict,
    fallback: Dict,
    /// 外部语言文件的覆盖项，优先级高于内置词典。
    overrides: HashMap<String, String>,
}

impl I18n {
    /// 装载语言。`locale` 为空表示跟随系统；`dir` 为额外语言文件目录（`None` 表示不加载）。
    pub fn load(locale: &str, dir: Option<&Path>) -> Self {
        let locale = resolve_locale(locale);
        let overrides = dir.map(|d| load_overrides(d, &locale)).unwrap_or_default();

        Self {
            primary: dict_for(&locale),
            fallback: dict_for(FALLBACK_LOCALE),
            locale,
            overrides,
        }
    }

    pub fn locale(&self) -> &str {
        &self.locale
    }

    /// 取译文。查找顺序：外部覆盖 → 内置当前语言 → 内置英文 → 键名本身。
    ///
    /// 最后回落到键名是刻意的：漏译时界面显示 `settings.scheme` 这种半成品，
    /// 比显示空白更容易定位问题。
    ///
    /// 生命周期显式绑定：返回值可能来自内置静态表，也可能来自调用方传入的 `key`，
    /// 把两者统一到 `'a` 才能既不分配内存又让借用检查通过。
    pub fn t<'a>(&'a self, key: &'a str) -> &'a str {
        self.overrides
            .get(key)
            .map(String::as_str)
            .or_else(|| lookup(self.primary, key))
            .or_else(|| lookup(self.fallback, key))
            .unwrap_or(key)
    }

    /// 供设置界面列出的可选语言。
    pub fn available() -> Vec<(String, String)> {
        BUILTIN_LOCALES
            .iter()
            .map(|(code, name)| ((*code).to_string(), (*name).to_string()))
            .collect()
    }
}

/// 把 `"zh"`、`"zh-Hans"`、`"zh_CN"` 之类的写法归一到内置语言代码。
/// 传入空串表示跟随系统。
pub fn resolve_locale(locale: &str) -> String {
    let raw = locale.trim().replace('_', "-").to_ascii_lowercase();
    if raw.is_empty() {
        normalize_code(&system_locale_name())
    } else {
        normalize_code(&raw)
    }
}

/// 目前只内置中英两套词典，其余语言一律回落英文。
fn normalize_code(code: &str) -> String {
    if code.to_ascii_lowercase().starts_with("zh") {
        "zh-CN".to_string()
    } else {
        "en-US".to_string()
    }
}

fn dict_for(locale: &str) -> Dict {
    match locale {
        "zh-CN" => ZH_CN,
        _ => EN_US,
    }
}

/// 读取系统区域名，例如 `"zh-CN"`、`"en-US"`。
fn system_locale_name() -> String {
    let mut buf = [0u16; LOCALE_NAME_MAX_LENGTH];

    // SAFETY: 缓冲区长度按 API 要求的 LOCALE_NAME_MAX_LENGTH 给出；
    // 返回值为写入的字符数（含结尾 NUL），为 0 表示失败。
    let len = unsafe { GetUserDefaultLocaleName(&mut buf) };

    if len > 1 {
        let end = ((len as usize) - 1).min(buf.len());
        return String::from_utf16_lossy(&buf[..end]);
    }
    FALLBACK_LOCALE.to_string()
}

/// 载入外部覆盖文件 `<dir>\<locale>.toml`，内容是一张扁平表：
///
/// ```toml
/// "app.title" = "我的启动器"
/// "tab.programs" = "应用"
/// ```
///
/// 键名含点号，在 TOML 里必须加引号，否则会被当成嵌套表。
fn load_overrides(dir: &Path, locale: &str) -> HashMap<String, String> {
    let path = dir.join(format!("{locale}.toml"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        // 没有外部文件是正常情况，不是错误
        return HashMap::new();
    };
    toml::from_str::<HashMap<String, String>>(&text).unwrap_or_default()
}

/// 静态表线性查找。
///
/// 表只有几十项，且查表只发生在绘制列表失效时、不在每帧热路径上，
/// 因此不值得为它引入 HashMap 的构建开销与额外内存。
fn lookup(dict: Dict, key: &str) -> Option<&'static str> {
    dict.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}
