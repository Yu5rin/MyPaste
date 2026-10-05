//! キー送出（Ctrl+Shift+V）。
//!
//! フックが設定したキー（既定は Ctrl+B）を検知した時点では、その修飾キーは
//! **物理的に押されたまま** である。そこで、足りない修飾キー（Ctrl / Shift）だけを
//! 押して V を打鍵し、押した分だけ離す。既定の Ctrl+B なら Shift を足すだけで、
//! 送出先アプリからは `Ctrl(物理) + Shift + V` の同時押しとして解釈される。
//!
//! Alt を含む組み合わせでは、Alt が押されたままだと `Ctrl+Alt+Shift+V` になって
//! しまうため、先に Alt の解放を送る。ただし Alt を単独で押して離したように見えると
//! アプリのメニュー（Excel ではリボンのキー操作）が開くので、その前に何も割り当て
//! られていないキー（[`VK_MENU_MASK`]）を 1 回打鍵して、それを防ぐ。
//!
//! 送出する入力には [`EXTRA_INFO_SIGNATURE`] を `dwExtraInfo` として付与し、
//! 自分が送ったキーをフック側で確実に無視できるようにする。

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT,
};

use crate::remap_logic::Hotkey;

/// 自分が SendInput で送出した入力であることを示す署名。
/// フックの `KBDLLHOOKSTRUCT.dwExtraInfo` と照合して自己入力を除外する。
/// （"ATAI" にちなんだ任意の非ゼロ値）
pub const EXTRA_INFO_SIGNATURE: usize = 0x0A7A_1A57;

/// 仮想キーコード 'V'
const VK_V: u16 = 0x56;

/// Alt の解放でメニューが開くのを防ぐために打鍵する、何も割り当てられていないキー
/// （0xE8 は未割り当て）。
const VK_MENU_MASK: u16 = 0xE8;

/// Ctrl+Shift+V を送出する。`held` は押されたままの修飾キー（設定したキーの組み合わせ）。
pub fn send_paste_values(held: &Hotkey) {
    unsafe {
        SendInput(&paste_values_inputs(held), std::mem::size_of::<INPUT>() as i32);
    }
}

/// 送出する入力の並びを組み立てる。
fn paste_values_inputs(held: &Hotkey) -> Vec<INPUT> {
    let mut inputs = Vec::with_capacity(10);
    if held.alt {
        inputs.push(key(VK_MENU_MASK, false));
        inputs.push(key(VK_MENU_MASK, true));
        inputs.push(key(VK_MENU.0, true)); // Alt 解放（押し直しはしない）
    }
    if !held.ctrl {
        inputs.push(key(VK_CONTROL.0, false));
    }
    if !held.shift {
        inputs.push(key(VK_SHIFT.0, false));
    }
    inputs.push(key(VK_V, false));
    inputs.push(key(VK_V, true));
    if !held.shift {
        inputs.push(key(VK_SHIFT.0, true));
    }
    if !held.ctrl {
        inputs.push(key(VK_CONTROL.0, true));
    }
    inputs
}

/// 1 つのキーボード入力（押下/解放）を表す `INPUT` を作る。
fn key(vk: u16, up: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if up {
        flags |= KEYEVENTF_KEYUP;
    }

    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: EXTRA_INFO_SIGNATURE,
            },
        },
    }
}
