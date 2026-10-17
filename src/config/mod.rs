//! 配置层：数据模型 + 配色 + 读写与热重载。
//!
//! 拆成四个文件各管一件事：
//! * [`color`] 只管颜色与渐变的表示和换算
//! * [`theme`] 只管配色方案（需求里的"透明化 + 多色彩配色"）
//! * [`model`] 只管配置字段结构
//! * [`store`] 只管文件读写与热重载（泛型实现，状态文件也复用它）

pub mod color;
pub mod model;
pub mod store;
pub mod theme;

pub use model::AppConfig;
pub use store::TomlStore;
pub use theme::SchemeBook;
// 配色派生的入口：上层只要这两个类型就能从基础色造出整套主题
pub use color::Color;
pub use theme::Scheme;

use crate::error::Result;
use crate::paths;

/// 用户配置仓库。载入时自动调用 [`AppConfig::normalize`] 夹紧越界值。
pub fn open() -> Result<TomlStore<AppConfig>> {
    TomlStore::open(paths::config_file()?, AppConfig::normalize)
}
