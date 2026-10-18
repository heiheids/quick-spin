//! 内嵌语言词典。
//!
//! 中文与英文随程序编译分发，因此不存在"语言文件丢了导致界面显示成键名"的问题。
//! 用户仍可在 `%APPDATA%\QuickSpin\lang\<locale>.toml` 放同名文件覆盖任意条目，
//! 也可以据此新增语言，无需重新编译（见 [`super::I18n`]）。
//!
//! 键名约定：`区域.用途`，例如 `settings.scheme`。

/// 一个词典就是一张静态表，零分配。
pub type Dict = &'static [(&'static str, &'static str)];

/// 简体中文。
pub const ZH_CN: Dict = &[
    // 窗口
    ("app.title", "Quick spin"),
    ("app.minimize", "最小化"),
    ("app.close", "关闭"),
    ("app.refresh", "重新扫描"),
    ("app.settings", "设置"),
    ("app.search", "搜索"),
    ("app.search_placeholder", "搜索快捷方式…"),
    // 选项卡
    ("tab.programs", "程序"),
    ("tab.documents", "文档"),
    ("tab.pictures", "图片"),
    ("tab.all", "全部"),
    ("tab.desktop", "桌面"),
    // 状态
    ("msg.scanning", "正在扫描快捷方式…"),
    ("msg.empty", "这里还没有快捷方式"),
    ("msg.empty_hint", "把快捷方式拖进来，或在设置里添加来源目录"),
    ("msg.target_missing", "目标文件不存在"),
    ("msg.load_failed", "加载失败"),
    ("msg.no_result", "没有匹配的结果"),
    ("msg.hotkey_conflict", "全局热键被其他程序占用"),
    // 右键菜单
    ("menu.launch", "打开"),
    ("menu.open_location", "打开文件所在位置"),
    ("menu.open_in_tab", "在新选项卡中打开"),
    ("menu.open_in_window", "在新窗口中打开"),
    ("menu.rename", "重命名"),
    ("menu.change_icon", "更换图标"),
    ("menu.remove", "从列表中移除"),
    ("menu.pin", "置顶"),
    ("menu.add_tab", "新建选项卡"),
    ("menu.remove_tab", "删除此选项卡"),
    ("menu.tab_settings", "选项卡设置"),
    // 设置面板
    ("settings.title", "设置"),
    ("settings.general", "常规"),
    ("settings.appearance", "外观"),
    ("settings.layout", "布局"),
    ("settings.sources", "来源"),
    ("settings.about", "关于"),
    ("settings.theme_custom", "主题色彩自定义"),
    // 弹出子窗口
    ("popout.palette", "配色"),
    ("popout.follow_main", "跟随主窗口"),
    ("settings.help", "使用说明"),
    ("settings.exit_program", "退出程序"),
    ("settings.about_update", "关于更新"),
    ("settings.opacity", "面板透明度"),
    ("settings.backdrop", "背景材质"),
    ("settings.outer_glass", "外圈玻璃效果"),
    ("settings.corner_radius", "圆角半径"),
    ("settings.language", "界面语言"),
    ("settings.icon_size", "图标大小"),
    ("settings.reset_icon_size", "恢复默认图标大小"),
    ("settings.autostart", "开机自动启动"),
    ("settings.always_on_top", "窗口置顶"),
    ("settings.start_hidden", "启动后隐藏到托盘"),
    ("settings.hide_after_launch", "启动程序后收起面板"),
    ("settings.single_instance", "只允许一个实例"),
    ("settings.show_tray", "显示托盘图标"),
    ("settings.hide_on_focus_lost", "失去焦点时自动隐藏"),
    ("settings.watch", "实时监视来源目录变化"),
    ("settings.recursive", "包含子文件夹"),
    ("settings.hotkey", "呼出热键"),
    ("settings.open_config", "打开配置文件"),
    ("settings.open_config_dir", "打开配置目录"),
    ("settings.save_default", "保存为默认配置"),
    ("settings.clear_cache", "清空图标缓存"),
    // 背景材质
    ("backdrop.none", "无（仅半透明）"),
    ("backdrop.acrylic", "亚克力毛玻璃"),
    ("backdrop.mica", "Mica（跟随系统）"),
    ("backdrop.blur", "系统模糊"),
    // 侧栏按钮样式
    ("sidebar.style", "侧栏按钮样式"),
    ("sidebar.style.icon_text", "图标 + 文字"),
    ("sidebar.style.icon_only", "仅图标"),
    ("sidebar.style.text_only", "仅文字"),
];

/// English.
pub const EN_US: Dict = &[
    // Window
    ("app.title", "Quick spin"),
    ("app.minimize", "Minimize"),
    ("app.close", "Close"),
    ("app.refresh", "Rescan"),
    ("app.settings", "Settings"),
    ("app.search", "Search"),
    ("app.search_placeholder", "Search shortcuts…"),
    // Tabs
    ("tab.programs", "Programs"),
    ("tab.documents", "Documents"),
    ("tab.pictures", "Pictures"),
    ("tab.all", "All"),
    ("tab.desktop", "Desktop"),
    // Status
    ("msg.scanning", "Scanning shortcuts…"),
    ("msg.empty", "No shortcuts here yet"),
    ("msg.empty_hint", "Drop a shortcut here, or add a source folder in Settings"),
    ("msg.target_missing", "Target file not found"),
    ("msg.load_failed", "Failed to load"),
    ("msg.no_result", "No matching results"),
    ("msg.hotkey_conflict", "Global hotkey is already taken"),
    // Context menu
    ("menu.launch", "Open"),
    ("menu.open_location", "Open file location"),
    ("menu.open_in_tab", "Open in new tab"),
    ("menu.open_in_window", "Open in new window"),
    ("menu.rename", "Rename"),
    ("menu.change_icon", "Change icon"),
    ("menu.remove", "Remove from list"),
    ("menu.pin", "Pin to top"),
    ("menu.add_tab", "New tab"),
    ("menu.remove_tab", "Delete this tab"),
    ("menu.tab_settings", "Tab settings"),
    // Settings panel
    ("settings.title", "Settings"),
    ("settings.general", "General"),
    ("settings.appearance", "Appearance"),
    ("settings.layout", "Layout"),
    ("settings.sources", "Sources"),
    ("settings.about", "About"),
    ("settings.help", "User guide"),
    ("settings.exit_program", "Exit"),
    ("settings.about_update", "Updates"),
    // 这一条原先只写在中文词典里，英文界面会直接显示键名
    //（`settings.theme_custom`）。补上。
    ("settings.theme_custom", "Theme colors"),
    // 弹出子窗口
    ("popout.palette", "Colors"),
    ("popout.follow_main", "Follow main window"),
    ("settings.scheme", "Color scheme"),
    ("settings.opacity", "Panel opacity"),
    ("settings.backdrop", "Backdrop material"),
    ("settings.outer_glass", "Outer glass effect"),
    ("settings.corner_radius", "Corner radius"),
    ("settings.language", "Language"),
    ("settings.icon_size", "Icon size"),
    ("settings.reset_icon_size", "Reset icon size"),
    ("settings.autostart", "Run at startup"),
    ("settings.always_on_top", "Always on top"),
    ("settings.start_hidden", "Start hidden in tray"),
    ("settings.hide_after_launch", "Collapse panel after launch"),
    ("settings.single_instance", "Single instance only"),
    ("settings.show_tray", "Show tray icon"),
    ("settings.hide_on_focus_lost", "Auto-hide when focus is lost"),
    ("settings.watch", "Watch source folders for changes"),
    ("settings.recursive", "Include subfolders"),
    ("settings.hotkey", "Toggle hotkey"),
    ("settings.open_config", "Open config file"),
    ("settings.open_config_dir", "Open config folder"),
    ("settings.save_default", "Save as default config"),
    ("settings.clear_cache", "Clear icon cache"),
    // Backdrop
    ("backdrop.none", "None (translucent only)"),
    ("backdrop.acrylic", "Acrylic blur"),
    ("backdrop.mica", "Mica (follows system)"),
    ("backdrop.blur", "System blur"),
    // Sidebar button style
    ("sidebar.style", "Sidebar button style"),
    ("sidebar.style.icon_text", "Icon + text"),
    ("sidebar.style.icon_only", "Icon only"),
    ("sidebar.style.text_only", "Text only"),
];
