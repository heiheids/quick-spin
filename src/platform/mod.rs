//! Win32 平台层。
//!
//! 全工程**只有这个目录**直接调用 Win32 API。这样做的价值：
//! 上层模块（`render` / `ui` / `app`）不依赖平台细节，
//! 将来要换渲染后端或做单元测试时，不必在业务代码里到处找系统调用。

pub mod acrylic;
pub mod autostart;
pub mod drop_target;
pub mod hittest;
pub mod hotkey;
pub mod icon;
pub mod icon_loader;
pub mod memory;
pub mod single_instance;
pub mod tray;
pub mod window;
