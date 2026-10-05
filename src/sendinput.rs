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
//! キー割り当て（[`crate::actions`]）からは、任意のキーの組み合わせの列（[`send_key_sequence`]）や
//! 文字（[`send_text`]）も送る。
//!
//! 送出する入力には [`EXTRA_INFO_SIGNATURE`] を `dwExtraInfo` として付与し、
//! 自分が送ったキーをフック側で確実に無視できるようにする。

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_LWIN,
    VK_MENU, VK_RETURN, VK_RWIN, VK_SHIFT, VK_TAB,
};

use crate::remap_logic::{self, Hotkey};

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
    let mut inputs = Vec::with_capacity(12);
    if held.alt || held.win {
        inputs.push(key(VK_MENU_MASK, false));
        inputs.push(key(VK_MENU_MASK, true));
    }
    if held.alt {
        inputs.push(key(VK_MENU.0, true)); // Alt 解放（押し直しはしない）
    }
    if held.win {
        // Win は settings.json で指定した場合だけ。左右どちらか分からないので両方離す。
        inputs.push(key(VK_LWIN.0, true));
        inputs.push(key(VK_RWIN.0, true));
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

/// キー割り当ての「キーを送る」: 押されたままの修飾キー（`held`）をいったん離してから、
/// `keys` を左から順に 1 つずつ送る（それぞれ修飾キーを押す → キーを押して離す → 修飾キーを離す）。
///
/// 押されたままの修飾キーは押し直さない（Alt や Win を押し直すと、離したときにメニューや
/// スタートが開いてしまうため）。利用者が後で物理的に離したときの解放は、そのまま通って害はない。
pub fn send_key_sequence(held: &Hotkey, keys: &[Hotkey]) {
    let mut inputs = release_inputs(held);
    for combo in keys {
        inputs.extend(combo_inputs(combo));
    }
    send(&inputs);
}

/// キー割り当ての「文字を入力する」: 押されたままの修飾キーを離してから、文字を入力する。
/// 改行は Enter、タブは Tab として送る（`\r\n` はまとめて 1 回の Enter）。
pub fn send_text(held: &Hotkey, text: &str) {
    let mut inputs = release_inputs(held);
    let mut buf = [0u16; 2];
    for c in text.chars() {
        match c {
            '\r' => {}
            '\n' => {
                inputs.push(key(VK_RETURN.0, false));
                inputs.push(key(VK_RETURN.0, true));
            }
            '\t' => {
                inputs.push(key(VK_TAB.0, false));
                inputs.push(key(VK_TAB.0, true));
            }
            c => {
                for unit in c.encode_utf16(&mut buf) {
                    inputs.push(unicode(*unit, false));
                    inputs.push(unicode(*unit, true));
                }
            }
        }
    }
    send(&inputs);
}

/// Alt や Win を押したまま割り当てたキーを押したとき、キーを送らない動作（プログラムを開く・
/// 何もしない など）のあとで Alt や Win を離すと、メニューやスタートが開いてしまう。
/// それを防ぐため、何も割り当てられていないキーを 1 回打鍵する（修飾キーは離さない）。
pub fn suppress_menu(held: &Hotkey) {
    if held.alt || held.win {
        send(&[key(VK_MENU_MASK, false), key(VK_MENU_MASK, true)]);
    }
}

/// 押されたままの修飾キー（`held`）を離すだけ（書式なし貼り付けなどの前に使う）。
pub fn release_modifiers(held: &Hotkey) {
    send(&release_inputs(held));
}

fn send(inputs: &[INPUT]) {
    if inputs.is_empty() {
        return;
    }
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// 押されたままの修飾キーを離す入力。Alt と Win は、離したときにメニューやスタートが
/// 開かないよう、先に何も割り当てられていないキーを打鍵する。
fn release_inputs(held: &Hotkey) -> Vec<INPUT> {
    let mut inputs = Vec::new();
    if held.alt || held.win {
        inputs.push(key(VK_MENU_MASK, false));
        inputs.push(key(VK_MENU_MASK, true));
    }
    if held.alt {
        inputs.push(key(VK_MENU.0, true));
    }
    if held.win {
        // 左右どちらを押しているか分からないので、両方の解放を送る。
        inputs.push(key(VK_LWIN.0, true));
        inputs.push(key(VK_RWIN.0, true));
    }
    if held.ctrl {
        inputs.push(key(VK_CONTROL.0, true));
    }
    if held.shift {
        inputs.push(key(VK_SHIFT.0, true));
    }
    inputs
}

/// 1 つの組み合わせ（例 Ctrl+Alt+V）を送る入力。修飾キーを押し、キーを押して離し、
/// 修飾キーを逆の順に離す。
fn combo_inputs(combo: &Hotkey) -> Vec<INPUT> {
    let mut modifiers = Vec::new();
    if combo.ctrl {
        modifiers.push(VK_CONTROL.0);
    }
    if combo.shift {
        modifiers.push(VK_SHIFT.0);
    }
    if combo.alt {
        modifiers.push(VK_MENU.0);
    }
    if combo.win {
        modifiers.push(VK_LWIN.0);
    }
    let mut inputs: Vec<INPUT> = modifiers.iter().map(|vk| key(*vk, false)).collect();
    let vk = combo.vk as u16;
    let extended = remap_logic::is_extended_key(combo.vk);
    inputs.push(key_ex(vk, false, extended));
    inputs.push(key_ex(vk, true, extended));
    inputs.extend(modifiers.iter().rev().map(|vk| key(*vk, true)));
    inputs
}

/// 1 つのキーボード入力（押下/解放）を表す `INPUT` を作る。
fn key(vk: u16, up: bool) -> INPUT {
    key_ex(vk, up, false)
}

/// 文字（UTF-16 の 1 単位）をそのまま入力する `INPUT` を作る（キー配置や IME に左右されない）。
fn unicode(unit: u16, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: EXTRA_INFO_SIGNATURE,
            },
        },
    }
}

/// キーボード入力を作る。`extended` は矢印や Home などの拡張キーの印を付けるか。
fn key_ex(vk: u16, up: bool, extended: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
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
