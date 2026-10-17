//! 全局快捷键：解析 "Ctrl+Alt+Q" 这类文本、注册到系统、并提供录制时要用的小工具。
//!
//! # 为什么存文本而不是位掩码
//!
//! 配置里存的是人能读懂的 `"Ctrl+Alt+Q"`。位掩码更紧凑，但排查问题时
//! 得在脑子里做一次反解；而这个值用户会想手改。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
    RegisterHotKey, UnregisterHotKey, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};

use crate::error::{AppError, Result};

/// 本程序只用一个全局热键，编号固定。
const HOTKEY_ID: i32 = 1;

/// `WM_HOTKEY` 的消息号。系统消息，不是自定义的。
pub const WM_HOTKEY_MSG: u32 = 0x0312;

/// 录制时读到的修饰键状态。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Modifiers {
    fn to_flags(self) -> HOT_KEY_MODIFIERS {
        let mut f = HOT_KEY_MODIFIERS(0);
        if self.ctrl {
            f |= MOD_CONTROL;
        }
        if self.alt {
            f |= MOD_ALT;
        }
        if self.shift {
            f |= MOD_SHIFT;
        }
        if self.win {
            f |= MOD_WIN;
        }
        f
    }
}

/// 读当前的修饰键状态。
///
/// 用 `GetKeyState` 而不是自己记按键序列：用户可能先按住 Ctrl 再切窗口回来，
/// 自己记账会漏掉"按下时不在我们窗口"的那些键。
pub fn current_modifiers() -> Modifiers {
    // SAFETY: `GetKeyState` 只读键盘状态，无副作用。
    unsafe {
        let down = |vk: i32| (GetKeyState(vk) as u16 & 0x8000) != 0;
        Modifiers {
            ctrl: down(VK_CONTROL.0 as i32),
            alt: down(VK_MENU.0 as i32),
            shift: down(VK_SHIFT.0 as i32),
            win: down(VK_LWIN.0 as i32) || down(VK_RWIN.0 as i32),
        }
    }
}

/// 某个虚拟键码是不是"修饰键本身"。
///
/// 录制时按到修饰键不能就此定下来 —— 用户按 Ctrl+Alt+Q 的过程中，
/// Ctrl 和 Alt 会各自产生一次 `WM_KEYDOWN`，那时还没有主键。
pub fn is_modifier_vk(vk: u32) -> bool {
    vk == VK_CONTROL.0 as u32
        || vk == VK_MENU.0 as u32
        || vk == VK_SHIFT.0 as u32
        || vk == VK_LWIN.0 as u32
        || vk == VK_RWIN.0 as u32
}

/// 把主键的虚拟键码转成显示名。不认识的键返回 `None`（即不接受）。
pub fn vk_name(vk: u32) -> Option<String> {
    match vk {
        // A-Z
        0x41..=0x5A => Some(((b'A' + (vk - 0x41) as u8) as char).to_string()),
        // 0-9
        0x30..=0x39 => Some(((b'0' + (vk - 0x30) as u8) as char).to_string()),
        // F1-F24
        0x70..=0x87 => Some(format!("F{}", vk - 0x70 + 1)),
        _ => None,
    }
}

/// 由修饰键与主键拼出规范文本，如 `"Ctrl+Alt+Q"`。
pub fn build_spec(mods: Modifiers, vk: u32) -> Option<String> {
    let name = vk_name(vk)?;
    let mut parts = Vec::new();
    if mods.ctrl {
        parts.push("Ctrl");
    }
    if mods.alt {
        parts.push("Alt");
    }
    if mods.shift {
        parts.push("Shift");
    }
    if mods.win {
        parts.push("Win");
    }
    // **至少要一个修饰键**：不带修饰的全局热键会抢走所有程序里的那个按键
    //（比如注册一个单独的 "Q"，全系统打字都会触发它）。
    if parts.is_empty() {
        return None;
    }
    parts.push(&name);
    Some(parts.join("+"))
}

/// 解析规范文本，得到修饰键标志与主键。
fn parse_spec(spec: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let mut mods = Modifiers::default();
    let mut vk: Option<u32> = None;

    for raw in spec.split('+') {
        let part = raw.trim();
        if part.is_empty() {
            continue;
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "alt" => mods.alt = true,
            "shift" => mods.shift = true,
            "win" | "super" => mods.win = true,
            other => {
                if vk.is_some() {
                    // 两个主键是无效组合
                    return None;
                }
                // 直接把 `Option` 存进去：`?` 在这里会把它拆成 `u32`，
                // 而字段本身是 `Option`，类型对不上。
                vk = parse_main_key(other);
            }
        }
    }

    // 与 `build_spec` 同一条约束：不允许没有修饰键的热键
    if !(mods.ctrl || mods.alt || mods.shift || mods.win) {
        return None;
    }
    let main = vk?;
    Some((mods.to_flags(), main))
}
fn parse_main_key(text: &str) -> Option<u32> {
    let bytes = text.as_bytes();
    if bytes.len() == 1 {
        let c = bytes[0].to_ascii_uppercase();
        if c.is_ascii_uppercase() {
            return Some(0x41 + (c - b'A') as u32);
        }
        if c.is_ascii_digit() {
            return Some(0x30 + (c - b'0') as u32);
        }
    }
    if let Some(num) = text.strip_prefix('f').or_else(|| text.strip_prefix('F')) {
        if let Ok(n) = num.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x70 + n - 1);
            }
        }
    }
    None
}

/// 注册全局热键。`spec` 为空表示只做注销。
///
/// **先注销再注册**：改键时若不先注销，旧组合会一直留在系统里，
/// 表现为"改了热键但旧的那个还能用"。
pub fn apply(hwnd: HWND, spec: &str) -> Result<()> {
    unregister(hwnd);

    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let Some((mods, vk)) = parse_spec(trimmed) else {
        return Err(AppError::Win32 {
            context: "热键文本无法解析（需形如 Ctrl+Alt+Q，且至少含一个修饰键）",
            code: 0,
        });
    };

    // `MOD_NOREPEAT`：不加的话按住不放会**反复触发**，
    // 对"呼出/隐藏窗口"这种切换动作会变成疯狂闪烁。
    // SAFETY: 只是向系统登记一个热键；句柄由调用方保证有效。
    unsafe { RegisterHotKey(Some(hwnd), HOTKEY_ID, mods | MOD_NOREPEAT, vk) }.map_err(|e| {
        // 最常见的失败原因是这个组合已被别的程序占用。
        // 必须让用户看到这条信息 —— 静默失败会让他以为热键生效了，
        // 一直按却毫无反应，然后来报"热键不好用"。
        AppError::Win32 {
            context: "RegisterHotKey 失败（该组合可能已被其它程序占用）",
            code: e.code().0,
        }
    })
}

/// 注销全局热键。没注册过时调用也是安全的。
pub fn unregister(hwnd: HWND) {
    // SAFETY: 只是解除登记；未注册时返回失败，忽略即可。
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
    }
}
