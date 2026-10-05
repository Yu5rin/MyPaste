//! 低レベルキーボードフック（WH_KEYBOARD_LL）。
//!
//! 対象アプリ（既定は Excel）が最前面かつ ON 状態のときに限り、設定したキーの
//! 組み合わせ（既定は `Ctrl+B`）を握りつぶして `Ctrl+Shift+V` を送出する。
//! それ以外のキーやアプリはそのまま通過させる。
//!
//! キーの組み合わせと対象アプリは [`configure`] で起動中にも差し替えられる
//! （設定画面で保存したとき）。
//!
//! ON/OFF 状態はメモリ上（[`ENABLED`]）にのみ保持し、終了時にリセットされる。

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::RwLock;

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::remap_logic::Hotkey;
use crate::{excel_check, sendinput};

/// ON/OFF 状態（起動時 ON、メモリのみ・終了時リセット）。
static ENABLED: AtomicBool = AtomicBool::new(true);

/// 設置済みフックハンドルの生ポインタ値。0 は未設置。
static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);

/// リマップ中のキー（仮想キーコード。0 はリマップしていない）。押しっぱなし
/// （オートリピート）で何度も貼り付けが走らないよう、最初の押下でのみ送出し、
/// 離すまでの押下と、対応する解放を握りつぶすために使う。
static ACTIVE_VK: AtomicU32 = AtomicU32::new(0);

/// 起動するキーの組み合わせ（[`Hotkey::pack`] の値）。既定は Ctrl+B。
static HOTKEY: AtomicU32 = AtomicU32::new(0x0142);

/// 対象アプリのプロセス名（大文字）。
static TARGET_APPS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// キーの組み合わせと対象アプリを設定する（起動時と、設定画面で保存したとき）。
pub fn configure(hotkey: Hotkey, target_apps: Vec<String>) {
    HOTKEY.store(hotkey.pack(), Ordering::SeqCst);
    match TARGET_APPS.write() {
        Ok(mut apps) => *apps = target_apps,
        Err(poisoned) => *poisoned.into_inner() = target_apps,
    }
}

/// 現在のキーの組み合わせ。
pub fn hotkey() -> Hotkey {
    Hotkey::unpack(HOTKEY.load(Ordering::SeqCst))
}

/// 最前面のウィンドウが対象アプリか。
fn is_target_foreground() -> bool {
    let apps = match TARGET_APPS.read() {
        Ok(apps) => apps,
        Err(poisoned) => poisoned.into_inner(),
    };
    excel_check::is_target_foreground(&apps)
}

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

/// 押されている修飾キーが、設定した組み合わせとちょうど一致するか（Win キーは不可）。
fn modifiers_match(hotkey: &Hotkey) -> bool {
    is_down(VK_CONTROL) == hotkey.ctrl
        && is_down(VK_SHIFT) == hotkey.shift
        && is_down(VK_MENU) == hotkey.alt
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

        // 自分が送った入力・注入入力は対象外。
        if !injected && !is_self {
            let hotkey = hotkey();
            match message {
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    // リマップ中のキーの押しっぱなし（オートリピート）は、修飾キーの
                    // 状態にかかわらず離すまで握りつぶす（再送もしない）。
                    if kb.vkCode == ACTIVE_VK.load(Ordering::SeqCst) {
                        return LRESULT(1);
                    }
                    // 対象アプリかつ ON かつ修飾キーが一致するときだけリマップする。
                    if kb.vkCode == hotkey.vk && modifiers_match(&hotkey) {
                        if is_enabled() && is_target_foreground() {
                            ACTIVE_VK.store(kb.vkCode, Ordering::SeqCst);
                            sendinput::send_paste_values(&hotkey);
                            log::debug!("{} -> Ctrl+Shift+V を送りました", hotkey.format());
                            return LRESULT(1);
                        }
                        // リマップしなかった理由を記録に残す（「効かない」ときの調査用）。
                        // 記録が OFF のときはプロセス名を調べる手間もかけない。
                        if log::log_enabled!(log::Level::Debug) {
                            if is_enabled() {
                                log::debug!(
                                    "{} はそのまま通しました（前面のアプリ {} は対象外）",
                                    hotkey.format(),
                                    excel_check::foreground_process_name()
                                        .unwrap_or_else(|| "不明".into())
                                );
                            } else {
                                log::debug!("{} はそのまま通しました（OFF のため）", hotkey.format());
                            }
                        }
                    }
                }
                // 押下を握りつぶしていた場合は、対応する解放も握りつぶす。
                WM_KEYUP | WM_SYSKEYUP
                    if kb.vkCode != 0
                        && ACTIVE_VK
                            .compare_exchange(kb.vkCode, 0, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok() =>
                {
                    return LRESULT(1);
                }
                _ => {}
            }
        }
    }

    CallNextHookEx(None, code, wparam, lparam)
}
