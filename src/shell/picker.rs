//! 供右键菜单使用的 shell 交互：选择图标文件。
//!
//! 注意这里**只有只读操作**。按架构文档 §9，右键菜单的删除/重命名/更换图标
//! 都只改程序内的覆盖表，不触碰用户的 `.lnk` 文件 ——
//! 所以本模块不会出现 `SHFileOperationW` 或 `IShellLinkW` 的写入路径。

use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FileOpenDialog, IFileOpenDialog,
    SIGDN_FILESYSPATH,
};
use windows::core::w;

use crate::error::{AppError, Result, Win32Ext};

/// 用户取消对话框时 `IFileDialog::Show` 返回的 HRESULT。
///
/// 这是**正常路径**，不是错误。不单独识别的话，用户每次点"取消"
/// 都会在控制台刷一行"失败"，把真正的故障淹没掉。
const ERROR_CANCELLED: i32 = 0x8007_04C7u32 as i32;

/// 弹出"选择图标文件"对话框。取消时返回 `Ok(None)`。
///
/// 调用线程必须已初始化 COM，且应使用 STA（对话框需要消息泵）。
pub fn pick_icon_file(owner: Option<HWND>) -> Result<Option<PathBuf>> {
    // SAFETY: COM 调用。CLSID/接口由 windows crate 提供，参数都是值类型。
    unsafe {
        // 第二个参数是 COM 聚合用的外部 IUnknown，**不是**对话框的父窗口。
        // 传 None（不聚合）；父窗口通过下面 `Show(owner)` 传。
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL)
            .ctx("CoCreateInstance(FileOpenDialog)")?;

        // 必须设 FORCEFILESYSTEM：否则用户可能选中"此电脑"之类的 shell 项，
        // 拿到的路径转换会失败。PATHMUSTEXIST/FILEMUSTEXIST 则挡掉手工乱输的路径。
        let mut options = dialog.GetOptions().ctx("IFileDialog::GetOptions")?;
        options |= FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_FILEMUSTEXIST;
        dialog.SetOptions(options).ctx("IFileDialog::SetOptions")?;

        // 图标可能来自这些容器。**必须把图片格式列出来** ——
        // 初版只写了 exe/dll/ico/lnk，用户想选 PNG 时在对话框里根本看不到
        // 自己的图片文件（不是"选不了"，而是列表里压根不显示，很容易误判成程序没实现）。
        // 图片经 shell 缩略图接口解码缩放，见 `platform::icon::extract_image`。
        let filters = [
            COMDLG_FILTERSPEC {
                pszName: w!("图标与图片"),
                pszSpec: w!("*.exe;*.dll;*.ico;*.lnk;*.png;*.jpg;*.jpeg;*.bmp;*.gif;*.webp"),
            },
            COMDLG_FILTERSPEC {
                pszName: w!("图标与程序"),
                pszSpec: w!("*.exe;*.dll;*.ico;*.lnk"),
            },
            COMDLG_FILTERSPEC {
                pszName: w!("图片"),
                pszSpec: w!("*.png;*.jpg;*.jpeg;*.bmp;*.gif;*.webp"),
            },
            COMDLG_FILTERSPEC {
                pszName: w!("所有文件"),
                pszSpec: w!("*.*"),
            },
        ];
        dialog
            .SetFileTypes(&filters)
            .ctx("IFileDialog::SetFileTypes")?;
        dialog
            .SetTitle(w!("选择图标"))
            .ctx("IFileDialog::SetTitle")?;

        match dialog.Show(owner) {
            Ok(()) => {}
            Err(e) if e.code().0 == ERROR_CANCELLED => return Ok(None),
            Err(e) => return Err(AppError::from(e)),
        }

        let item = dialog.GetResult().ctx("IFileDialog::GetResult")?;
        let wide = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ctx("IShellItem::GetDisplayName")?;
        let text = wide.to_string().map_err(|_| AppError::Win32 {
            context: "IShellItem::GetDisplayName 返回了非法 UTF-16",
            code: 0,
        });
        // 这个字符串是 shell 分配的，必须用 CoTaskMemFree 释放。
        // 不释放就是每次选图标泄漏一个路径字符串。
        CoTaskMemFree(Some(wide.0 as *const core::ffi::c_void));

        Ok(Some(PathBuf::from(text?)))
    }
}
