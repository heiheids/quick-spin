//! Shell 交互层：解析 `.lnk`、扫描快捷方式来源、启动目标。
//!
//! 这一层只依赖 Win32/COM，不依赖窗口或渲染，
//! 因此可以单独跑在后台线程里。

pub mod com;
pub mod launch;
pub mod lnk;
pub mod picker;
pub mod scan;
