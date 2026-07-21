//! キー送出（Ctrl+Shift+V）。
//!
//! フックが Ctrl+B を検知した時点では **Ctrl は物理的に押されたまま** なので、
//! ここでは Shift を足して V を打鍵するだけでよい。結果として送出先アプリからは
//! `Ctrl(物理) + Shift + V` の同時押しとして解釈される。
//!
//! 送出する入力には [`EXTRA_INFO_SIGNATURE`] を `dwExtraInfo` として付与し、
//! 自分が送ったキーをフック側で確実に無視できるようにする。

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_SHIFT,
};

/// 自分が SendInput で送出した入力であることを示す署名。
/// フックの `KBDLLHOOKSTRUCT.dwExtraInfo` と照合して自己入力を除外する。
/// （"ATAI" にちなんだ任意の非ゼロ値）
pub const EXTRA_INFO_SIGNATURE: usize = 0x0A7A_1A57;

/// 仮想キーコード 'V'
const VK_V: u16 = 0x56;

/// Ctrl+Shift+V を送出する（Ctrl は物理押下中である前提）。
pub fn send_ctrl_shift_v() {
    let inputs = [
        key(VK_SHIFT.0, false), // Shift 押下
        key(VK_V, false),       // V 押下
        key(VK_V, true),        // V 解放
        key(VK_SHIFT.0, true),  // Shift 解放
    ];

    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
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
