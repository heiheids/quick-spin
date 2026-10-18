# Quick spin

<img width="623" height="1193" alt="2026-10-18_112911" src="https://github.com/user-attachments/assets/842a4ec5-0751-484e-9238-178e7e1af947" />
<img width="595" height="1189" alt="2026-10-18_112827" src="https://github.com/user-attachments/assets/211cb692-21b9-4a86-bf54-52d8f0b7503c" />
<img width="624" height="1215" alt="2026-10-18_112815" src="https://github.com/user-attachments/assets/d5ffda8e-9e71-48a5-bc76-02909ea4493d" />

低内存、界面透明可调色的 Windows 快捷方式启动器。界面用 Direct2D 完全自绘，
经 DirectComposition 提交给 DWM，不使用任何 UI 框架。

- 发布版体积约 **1.3 MB**（全量 LTO + 单代码生成单元 + 剥离符号）
- 空载内存约 **52 MB**（私有提交；含 D3D11/D2D 设备与驱动分配）
- 启动界面与设备创建在百毫秒量级，运行中**不出现拖影与闪烁**

## 许可证（请先读这一段）

本项目使用 [PolyForm Noncommercial License 1.0.0](LICENSE)：**源码公开，但禁止商用。**

需要说明的是，**这不是 OSI 定义的「开源」**。OSI 的开源定义明确要求不得歧视任何
使用领域，而带「禁止商用」条款的许可证做不到这一点。因此 GitHub 会把本项目标记为
*source-available* 而不是 *open source*。

允许的用途包括：个人学习与研究、业余项目、慈善机构、教育机构、公共研究机构、
公共安全与卫生机构、环保组织、政府机构。商业使用需要另行取得授权。

## 功能

- **手动添加**：把 `.lnk` 拖进窗口即可（会复制一份到程序自己的 `shortcuts\` 目录，
  源快捷方式被移动或删除都不会让条目失效）
- **分组**：左侧选项卡，可拖动排序；条目可拖到选项卡上整批移动
- **Ctrl 多选 + 批量删除 / 批量移动到分组**
- **弹出子窗口**：在选项卡上点右键，可以把一个分组**单独开成一个窗口** ——
  真图标、点方格即启动、可给这个窗口**单独配色**；默认吸附在主窗口侧面跟着走，
  拖开即脱离；也可以「钉住」让它不随主窗口收进托盘。窗口的位置 / 大小 / 吸附 /
  钉住 / 配色都会记住，下次启动自动恢复。
- **删除只在本程序内生效**：只写状态文件的覆盖表（`hidden = true`），
  **绝不删除或移动你自己的文件**
- 托盘图标（左键显隐 / 右键菜单）、全局热键、开机自启、单实例
- 设置面板：外圈玻璃（Acrylic）、窗口置顶、启动后隐藏、开机自启、热键录制、
  64 色基色调色板
- 图标与缩略图缓存，预算 24 MB，超预算按 LRU 淘汰
- 支持 AUMID 启动（例如 Windows 11 的「画图」没有 `mspaint.exe`，
  只能通过 `shell:AppsFolder\<AUMID>` 启动）

## 环境要求

- Windows 10 / 11（x64）
- Rust 稳定版工具链（edition 2024，需要较新的版本）
- MSVC 构建工具
- Windows SDK（`build.rs` 需要 `rc.exe` 把 `assets/app.ico` 编进 exe 图标资源；
  会自己在 `%ProgramFiles(x86)%\Windows Kits\10\bin\<版本>\x64\` 下查找）

## 构建

```powershell
cargo build --release
# 产物：target\release\quick-spin.exe

# 跑一遍图形链路自检并实测内存（不显示窗口）
cargo run --release -- --selftest
```

## 使用

```
quick-spin.exe                  启动界面
quick-spin.exe --selftest       跑一遍图形链路并实测内存（不显示窗口）
quick-spin.exe --diagnose       扫描快捷方式并打印统计（不启界面）
quick-spin.exe --open <关键词>   按名称启动一个快捷方式
quick-spin.exe --help           显示完整帮助
```

界面操作：

| 操作 | 效果 |
|---|---|
| 拖入 `.lnk` | 添加条目 |
| 单击图标 | 启动 |
| Ctrl + 单击 | 多选 |
| 拖到左侧选项卡 | 把选中的整批移动到该分组 |
| 右键图标 | 重命名 / 移动到 / 打开所在位置 / 删除 |
| 拖动左侧选项卡 | 调整分组顺序 |
| Ctrl + 滚轮 | 调整图标大小 |
| 右键左侧选项卡 | 弹出子窗口（再点一次关闭）—— 该分组的独立窗口 |
| 弹窗里单击方格 / 名字 | 启动那一条 |
| 拖弹窗标题栏 | 移动；**拖开就脱离吸附**，不再跟着主窗口走 |
| 弹窗标题栏四个按钮 | 配色 / 吸附开关 / 钉住开关 / 关闭 |

## 数据位置

**数据与 exe 放在同一个目录**（便携 / 绿色软件布局）：`config.toml`、`state.toml`、
`shortcuts\`、`icon-cache\`、`lang\`。

整个文件夹拷走即用，备份程序目录时会连配置一起备份到。

> 两点代价，都是有意的取舍：
> 1. exe 所在目录必须**可写**。放进 `C:\Program Files\` 这类位置会写不进去——
>    本项目定位是便携工具，不打算支持那种安装方式。
> 2. 开发时 exe 在 `target\release\`，**`cargo clean` 会把配置和快捷方式一起删掉**。
>    要在开发期保住数据，把 exe 复制到一个单独目录再运行。
>
> 从旧版本（数据在 `%APPDATA%\QuickSpin` 或更早的 `%APPDATA%\QuickLaunch`）
> 升级时会自动迁移一次，并同步改写状态文件里记录绝对路径的键。

## 目录结构

```
build.rs                    编译期调用 rc.exe 打包图标资源
assets/app.ico              程序图标（16/24/32/48/64/128/256，7 种尺寸）
src/
  main.rs                   入口、命令行参数、--selftest
  paths.rs                  全部文件位置（数据目录 = exe 所在目录）
  error.rs                  全工程统一错误类型
  config/                   config.toml 的模型、配色、主题、读写
  state.rs                  state.toml：运行时状态与覆盖表
  model/                    条目模型与视图过滤
  shell/                    .lnk 解析、快捷方式扫描、ShellExecute 启动
  platform/                 窗口、托盘、热键、单实例、自启、拖放目标
  render/                   D3D11 / DXGI / D2D1 / DirectComposition 与各绘制器
  ui/layout.rs              布局计算与命中测试
docs/ARCHITECTURE.md        框架设计文档
```

## 技术要点

- **渲染链路**：D3D11 设备 → DXGI 交换链（`CreateSwapChainForComposition`，
  `FLIP_SEQUENTIAL`，`B8G8R8A8_UNORM`，BufferCount=2）→ D2D1 DeviceContext →
  `IDCompositionVisual` → DWM
- **无闪烁的前提**：窗口带 `WS_EX_NOREDIRECTIONBITMAP`（不分配重定向位图），
  自绘内容直接由 DWM 合成；`WM_NCCALCSIZE` 返回 0 实现无边框
- **配置与状态分离**：`config.toml` 存用户意图，`state.toml` 存运行时状态
  （隐藏、分组、窗口几何），两者分开便于把状态当作可丢弃的覆盖层
- **不引入动画**：所有状态切换都是即时绘制，避免逐帧重绘带来的占屏与功耗

## 依赖

[`windows`](https://crates.io/crates/windows) 0.62 / `windows-core` 0.62 /
`windows-numerics` 0.3、`serde`、`toml`、`thiserror`。
