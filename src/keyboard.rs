//! 低レベルキーボードフック（WH_KEYBOARD_LL）。
//!
//! Excel が最前面かつ ON 状態のときに限り、`Ctrl+B` を握りつぶして
//! `Ctrl+Shift+V` を送出する。それ以外のキーやアプリはそのまま通過させる。
//!
//! ON/OFF 状態はメモリ上（[`ENABLED`]）にのみ保持し、終了時にリセットされる。

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::{excel_check, sendinput};

/// ON/OFF 状態（起動時 ON、メモリのみ・終了時リセット）。
static ENABLED: AtomicBool = AtomicBool::new(true);

/// 設置済みフックハンドルの生ポインタ値。0 は未設置。
static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);

/// Ctrl+B のリマップ中フラグ。押しっぱなし（オートリピート）で
/// 何度も貼り付けが走らないよう、最初の押下でのみ送出するために使う。
static B_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 監視対象キー: 'B'
const VK_B: u32 = 0x42;

/// 現在 ON かどうか。
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// ON/OFF を明示的に設定する。
#[allow(dead_code)]
pub fn set_enabled(value: bool) {
    ENABLED.store(value, Ordering::SeqCst);
}

/// ON/OFF を反転し、反転後の状態を返す。
pub fn toggle() -> bool {
    // fetch_xor で真をトグルし、反転後の値を返す。
    let previous = ENABLED.fetch_xor(true, Ordering::SeqCst);
    !previous
}

/// キーボードフックを設置する。メッセージループを回すスレッドから呼ぶこと。
pub fn install() -> windows::core::Result<()> {
    unsafe {
        let hmod = GetModuleHandleW(None)?;
        let hook = SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(low_level_keyboard_proc),
            HINSTANCE(hmod.0),
            0,
        )?;
        HOOK_HANDLE.store(hook.0 as isize, Ordering::SeqCst);
    }
    Ok(())
}

/// キーボードフックを解除する（リソースリーク防止のため終了時に必ず呼ぶ）。
pub fn uninstall() {
    let raw = HOOK_HANDLE.swap(0, Ordering::SeqCst);
    if raw != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(raw as *mut core::ffi::c_void));
        }
    }
}

/// 指定した仮想キーが現在押されているか。
fn is_down(vk: VIRTUAL_KEY) -> bool {
    // GetAsyncKeyState の最上位ビット（0x8000）が押下状態を表す。
    unsafe { (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
}

/// Ctrl のみが押されている（Shift/Alt/Win は押されていない）か。
fn ctrl_only() -> bool {
    is_down(VK_CONTROL)
        && !is_down(VK_SHIFT)
        && !is_down(VK_MENU)
        && !is_down(VK_LWIN)
        && !is_down(VK_RWIN)
}

/// 低レベルキーボードフックのコールバック。
unsafe extern "system" fn low_level_keyboard_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // code が負のときは処理せず次のフックへ渡す（Win32 の規約）。
    if code >= 0 {
        let message = wparam.0 as u32;
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);

        let injected = (kb.flags.0 & LLKHF_INJECTED.0) != 0;
        let is_self = kb.dwExtraInfo == sendinput::EXTRA_INFO_SIGNATURE;

        // 自分が送った入力・注入入力は対象外。対象キーは 'B' のみ。
        if !injected && !is_self && kb.vkCode == VK_B {
            match message {
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    // Excel かつ ON かつ Ctrl 単独のときだけリマップする。
                    if ctrl_only() && is_enabled() && excel_check::is_excel_foreground() {
                        // 最初の押下でのみ送出（オートリピートでは再送しない）。
                        if !B_ACTIVE.swap(true, Ordering::SeqCst) {
                            sendinput::send_ctrl_shift_v();
                            log::debug!("Ctrl+B -> Ctrl+Shift+V (Excel, ON)");
                        }
                        // 押下中はオリジナルの Ctrl+B を常に破棄する。
                        return LRESULT(1);
                    }
                }
                WM_KEYUP | WM_SYSKEYUP => {
                    // 押下を握りつぶしていた場合は、対応する解放も握りつぶす。
                    if B_ACTIVE.swap(false, Ordering::SeqCst) {
                        return LRESULT(1);
                    }
                }
                _ => {}
            }
        }
    }

    CallNextHookEx(None, code, wparam, lparam)
}
