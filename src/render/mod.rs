//! 渲染层：Direct2D + DirectComposition。
//!
//! 这一层只接受绘制任务，**不查询配置、不调用 Win32 消息 API**，
//! 因此可以独立演进（将来换后端、加单元测试都不牵动业务代码）。

pub mod composer;
pub mod device;
pub mod icon_cache;
pub mod painters;
pub mod text;

pub use device::Renderer;
pub use text::{Role, TextEngine};
