//! 全工程统一的错误类型。
//!
//! 约定：各模块只返回 `crate::error::Result`，不各自造错误类型，
//! 也不在业务代码里 `unwrap()`。这样出错信息能带上下文，排查时不用猜。

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// 带文件路径的 IO 错误。定位问题时路径比错误码更有用。
    #[error("IO 操作失败 [{path}]: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// 没有具体路径的 IO 错误。
    #[error("IO 操作失败: {0}")]
    BareIo(#[from] std::io::Error),

    #[error("配置解析失败 (TOML 语法或字段类型不对): {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("配置序列化失败: {0}")]
    ConfigWrite(#[from] toml::ser::Error),

    /// Win32 调用失败。`context` 说明当时在做什么。
    #[error("Windows API 调用失败: {context} (HRESULT 0x{code:08X})")]
    Win32 { context: &'static str, code: i32 },

    /// shell 启动失败。
    ///
    /// 单独一个变体是因为 `ShellExecuteW` 的返回值**不是** HRESULT，
    /// 而是 ≤32 的约定错误码（0=内存不足、2=找不到文件、5=拒绝访问…）。
    /// 混进 `Win32` 变体打印成 "HRESULT 0x00000002" 会误导排查。
    #[error("启动失败: {message}（ShellExecute 返回 {code}）")]
    ShellExecute { message: &'static str, code: i32 },

    /// 路径转 UTF-16 时发现内嵌 NUL 等非法内容。
    #[error("路径无法转换为合法的 UTF-16 字符串: {0}")]
    BadPath(PathBuf),
}

impl AppError {
    /// 给 IO 错误补上出错的文件路径。
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

impl From<windows::core::Error> for AppError {
    fn from(e: windows::core::Error) -> Self {
        Self::Win32 {
            context: "未标注上下文的 Win32 调用",
            code: e.code().0,
        }
    }
}

/// 给 Win32 / COM 调用结果补上"这一步在干什么"的上下文。
///
/// 用法：`unsafe { SomeApi(...) }.ctx("创建窗口")?`
pub trait Win32Ext<T> {
    fn ctx(self, context: &'static str) -> Result<T>;
}

impl<T> Win32Ext<T> for windows::core::Result<T> {
    fn ctx(self, context: &'static str) -> Result<T> {
        self.map_err(|e| AppError::Win32 {
            context,
            code: e.code().0,
        })
    }
}
