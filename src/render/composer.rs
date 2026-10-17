//! DirectComposition 视觉树。
//!
//! 交换链画面通过 `IDCompositionVisual` 提交给窗口，全程**不经过 CPU 拷贝**。
//! 这是相对"分层窗口 + `UpdateLayeredWindow`"的决定性优势 ——
//! 后者每次更新都要把整张位图从内存拷进系统，拖动窗口时必掉帧、必留残影。

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGISwapChain1};

use crate::error::{Result, Win32Ext};

/// 一个窗口的合成器。
///
/// 交换链是它的内容，窗口是它的目标。窗口尺寸变化**不需要**通知合成器：
/// 视觉内容的尺寸由交换链缓冲决定，合成器只负责把它贴到窗口上。
pub struct Composer {
    device: IDCompositionDevice,
    _visual: IDCompositionVisual,
    /// 持有 target 是必须的：它若被释放，视觉树与窗口的关联就断了。
    _target: IDCompositionTarget,
}

impl Composer {
    /// 把交换链挂到一个窗口上。
    pub fn attach(
        dxgi_device: &IDXGIDevice,
        hwnd: HWND,
        content: &IDXGISwapChain1,
    ) -> Result<Self> {
        // SAFETY: 所有接口都来自同一条设备链，生命周期由返回值持有；
        // hwnd 由调用方保证是一个有效窗口。
        unsafe {
            let device: IDCompositionDevice =
                DCompositionCreateDevice(dxgi_device).ctx("DCompositionCreateDevice")?;

            // topmost = true：把这个视觉放在窗口内容层的最上面
            let target = device
                .CreateTargetForHwnd(hwnd, true)
                .ctx("IDCompositionDevice::CreateTargetForHwnd")?;

            let visual = device
                .CreateVisual()
                .ctx("IDCompositionDevice::CreateVisual")?;

            visual
                .SetContent(content)
                .ctx("IDCompositionVisual::SetContent")?;
            target
                .SetRoot(&visual)
                .ctx("IDCompositionTarget::SetRoot")?;

            // 不 Commit 的话上面这些改动一个都不会生效
            device.Commit().ctx("IDCompositionDevice::Commit")?;

            Ok(Self {
                device,
                _visual: visual,
                _target: target,
            })
        }
    }

    /// 提交一批视觉改动。改完视觉属性后必须调用。
    pub fn commit(&self) -> Result<()> {
        // SAFETY: device 由 self 持有，必定有效。
        unsafe { self.device.Commit() }.ctx("IDCompositionDevice::Commit")
    }
}
