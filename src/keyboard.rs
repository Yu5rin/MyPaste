//! 低レベルキーボードフック（WH_KEYBOARD_LL）。
//!
//! 対象アプリ（既定は Excel）が最前面かつ ON 状態のときに限り、設定したキーの
//! 組み合わせ（既定は `Ctrl+B`）を握りつぶして `Ctrl+Shift+V` を送出する。
//! それ以外のキーやアプリはそのまま通過させる。
//!
//! キーの組み合わせと対象アプリは [`configure`] で起動中にも差し替えられる
//! （設定画面で保存したとき）。
//!
//! さらに、キー割り当て（[`crate::hotkey_rules`]）で登録したキーを捕まえて、
//! その動作の実行を [`crate::actions`] に頼む（[`set_rules`]）。値貼り付けのキーが
//! 先に効き、キー割り当ては上から順に調べて最初に当てはまったものだけを使う。
//!
//! クリップボードの履歴の一覧を出す操作（キーの組み合わせ、または Ctrl などの 2 回押し）も
//! ここで捕まえる（[`set_history_trigger`]）。キー割り当ての ON/OFF にはかかわらず効く。
//!
//! ON/OFF 状態はメモリ上（[`ENABLED`]）にのみ保持し、終了時にリセットされる。

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::RwLock;

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_MBUTTONDOWN,
    WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
};

use crate::clip_history::{DoubleTap, Modifier};
use crate::hotkey_rules::{Action, Rule};
use crate::remap_logic::Hotkey;
use crate::{actions, excel_check, sendinput};

/// ON/OFF 状態（起動時 ON、メモリのみ・終了時リセット）。
static ENABLED: AtomicBool = AtomicBool::new(true);

/// 設置済みフックハンドルの生ポインタ値。0 は未設置。
static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);

/// リマップ中のキー（仮想キーコード。0 はリマップしていない）。押しっぱなし
/// （オートリピート）で何度も貼り付けが走らないよう、最初の押下でのみ送出し、
/// 離すまでの押下と、対応する解放を握りつぶすために使う。
static ACTIVE_VK: AtomicU32 = AtomicU32::new(0);
/// [`ACTIVE_VK`] のキーを、最後に握りつぶした時刻（フックに届いた時刻。ミリ秒）。
static ACTIVE_TIME: AtomicU32 = AtomicU32::new(0);
/// 握りつぶしているキーの「離した」を見逃した（UAC の画面や、管理者として動くアプリの上で
/// 離された）とみなすまでの時間。押しっぱなしの間はオートリピートが届き続けるので、
/// これだけ途切れたら、もう離されている。
const ACTIVE_STALE_MS: u32 = 1500;

/// 握りつぶすキーを覚える。
fn set_active(vk: u32, time: u32) {
    ACTIVE_TIME.store(time, Ordering::SeqCst);
    ACTIVE_VK.store(vk, Ordering::SeqCst);
}

/// 起動するキーの組み合わせ（[`Hotkey::pack`] の値）。既定は Ctrl+B。
static HOTKEY: AtomicU32 = AtomicU32::new(0x0142);

/// 対象アプリのプロセス名（大文字）。
static TARGET_APPS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// クリップボードの履歴の一覧を出すキーの組み合わせ（[`Hotkey::pack`] の値。0 は使わない）。
static HISTORY_HOTKEY: AtomicU32 = AtomicU32::new(0);
/// 履歴の一覧を出す 2 回押しのキー（0 = 使わない、1 = Ctrl、2 = Shift、3 = Alt）と間隔（ミリ秒）。
static HISTORY_DOUBLE_TAP: AtomicU32 = AtomicU32::new(0);
static HISTORY_DOUBLE_TAP_MS: AtomicU32 = AtomicU32::new(400);

thread_local! {
    /// 2 回押しの判定（フックのスレッドだけで使う）。
    static DOUBLE_TAP: RefCell<DoubleTap> = RefCell::new(DoubleTap::default());
}

/// クリップボードの履歴の一覧を出す操作を決める。どちらも `None` なら、キーでは出さない。
pub fn set_history_trigger(hotkey: Option<Hotkey>, double_tap: Option<Modifier>, interval_ms: u32) {
    HISTORY_HOTKEY.store(hotkey.map_or(0, |h| h.pack()), Ordering::SeqCst);
    let code = match double_tap {
        None => 0,
        Some(Modifier::Ctrl) => 1,
        Some(Modifier::Shift) => 2,
        Some(Modifier::Alt) => 3,
    };
    HISTORY_DOUBLE_TAP.store(code, Ordering::SeqCst);
    HISTORY_DOUBLE_TAP_MS.store(interval_ms, Ordering::SeqCst);
}

fn history_hotkey() -> Option<Hotkey> {
    match HISTORY_HOTKEY.load(Ordering::SeqCst) {
        0 => None,
        packed => Some(Hotkey::unpack(packed)),
    }
}

fn history_double_tap() -> Option<Modifier> {
    match HISTORY_DOUBLE_TAP.load(Ordering::SeqCst) {
        1 => Some(Modifier::Ctrl),
        2 => Some(Modifier::Shift),
        3 => Some(Modifier::Alt),
        _ => None,
    }
}

/// アプリが「離した」を送った（[`mark_released_by_us`]）が、利用者はまだ押したままの
/// 修飾キー（[`MOD_CTRL`] などのビット）。`GetAsyncKeyState` はアプリが送った「離した」も
/// 反映するので、修飾キーを押したまま割り当てたキーを続けて押したときに、2 回目が一致しなく
/// なる。それを防ぐため、利用者が実際に離す（または押し直す）までは押されているとみなす。
static RELEASED_BY_US: AtomicU32 = AtomicU32::new(0);
/// [`RELEASED_BY_US`] に最後に印を付けた時刻（`GetTickCount`）。
static RELEASED_AT: AtomicU32 = AtomicU32::new(0);
/// 印を信じる時間。利用者が離したことを見逃すと印が残り、押していない修飾キーを押していると
/// みなし続けてしまうので、これを過ぎた印は捨てる。
const RELEASED_STALE_MS: u32 = 5000;
const MOD_CTRL: u32 = 1;
const MOD_SHIFT: u32 = 2;
const MOD_ALT: u32 = 4;
const MOD_WIN: u32 = 8;

/// アプリが修飾キーの「離した」を送ることを知らせる（[`crate::sendinput`] が、送る直前に呼ぶ）。
///
/// その時点で実際に押されているものだけに印を付ける。利用者がすでに離していた修飾キーに
/// 印を付けると、次に押すまで「押されている」と誤解し続けてしまうため。
pub fn mark_released_by_us(ctrl: bool, shift: bool, alt: bool, win: bool) {
    let bits = (u32::from(ctrl && is_down(VK_CONTROL)) * MOD_CTRL)
        | (u32::from(shift && is_down(VK_SHIFT)) * MOD_SHIFT)
        | (u32::from(alt && is_down(VK_MENU)) * MOD_ALT)
        | (u32::from(win && (is_down(VK_LWIN) || is_down(VK_RWIN))) * MOD_WIN);
    if bits != 0 {
        RELEASED_AT.store(unsafe { GetTickCount() }, Ordering::SeqCst);
        RELEASED_BY_US.fetch_or(bits, Ordering::SeqCst);
        // 印を付けている間に、利用者がもう離していた（フックが印を消したあとに付けてしまった）
        // 修飾キーは、印を外す。
        let released = (u32::from(!is_down(VK_CONTROL)) * MOD_CTRL)
            | (u32::from(!is_down(VK_SHIFT)) * MOD_SHIFT)
            | (u32::from(!is_down(VK_MENU)) * MOD_ALT)
            | (u32::from(!is_down(VK_LWIN) && !is_down(VK_RWIN)) * MOD_WIN);
        RELEASED_BY_US.fetch_and(!(bits & released), Ordering::SeqCst);
    }
}

/// アプリが離した Ctrl・Shift を、利用者がまだ押しているなら押し直す（動作のあとで呼ぶ）。
///
/// 押し直さないと、Ctrl を押したまま割り当てのキーを押し、続けて `S` を押したときに、
/// Windows は Ctrl が離されていると思っているので、保存ではなく `s` が入力されてしまう。
/// Alt と Win は、押し直すとメニューやスタートが開くことがあるので、押し直さない。
pub fn repress_released_modifiers() {
    let mut pressed = 0;
    for (bit, vk) in [(MOD_CTRL, VK_CONTROL), (MOD_SHIFT, VK_SHIFT)] {
        if RELEASED_BY_US.load(Ordering::SeqCst) & bit == 0 {
            continue;
        }
        sendinput::press_modifier(vk.0);
        pressed += 1;
        // 押し直す間に利用者が離していたら（フックが印を消している）、離した状態に戻す。
        if RELEASED_BY_US.fetch_and(!bit, Ordering::SeqCst) & bit == 0 {
            sendinput::release_modifier(vk.0);
        }
    }
    if pressed == 2 {
        // Ctrl+Shift だけを押して離すと、入力方式を切り替える Windows のホットキーになることがある。
        sendinput::tap_mask();
    }
}

/// 修飾キーの仮想キーコードを、[`MOD_CTRL`] などのビットにする。修飾キーでなければ 0。
fn modifier_bit(vk: u32) -> u32 {
    match vk {
        0x11 | 0xA2 | 0xA3 => MOD_CTRL,
        0x10 | 0xA0 | 0xA1 => MOD_SHIFT,
        0x12 | 0xA4 | 0xA5 => MOD_ALT,
        0x5B | 0x5C => MOD_WIN,
        _ => 0,
    }
}

/// キー割り当て（有効で読めるものだけ）。
static RULES: RwLock<Vec<Rule>> = RwLock::new(Vec::new());

/// キー割り当てを使うか（トレイメニューで切り替える。起動時 ON、メモリのみ）。
static RULES_ENABLED: AtomicBool = AtomicBool::new(true);

/// キー割り当てを差し替える（起動時と、キー割り当て画面で保存したとき）。
pub fn set_rules(rules: Vec<Rule>) {
    match RULES.write() {
        Ok(mut current) => *current = rules,
        Err(poisoned) => *poisoned.into_inner() = rules,
    }
}

/// キー割り当てを使うか。
pub fn rules_enabled() -> bool {
    RULES_ENABLED.load(Ordering::SeqCst)
}

/// キー割り当ての ON/OFF を反転し、反転後の状態を返す。
/// キー割り当ての ON/OFF を決める（起動時と、設定を読み込んだとき）。
pub fn set_rules_enabled(value: bool) {
    RULES_ENABLED.store(value, Ordering::SeqCst);
}

pub fn toggle_rules() -> bool {
    !RULES_ENABLED.fetch_xor(true, Ordering::SeqCst)
}

/// 押されたキーに当てはまるキー割り当てを探す（動作と、その割り当てのキーの組み合わせ）。
fn find_rule(vk: u32) -> Option<(Action, Hotkey)> {
    if !rules_enabled() {
        return None;
    }
    let rules = match RULES.read() {
        Ok(rules) => rules,
        Err(poisoned) => poisoned.into_inner(),
    };
    // まずキー本体だけで絞る（ほとんどのキーはここで外れ、余計な問い合わせをしない）。
    let mut candidates = rules
        .iter()
        .filter(|r| r.hotkey.vk == vk && modifiers_match(&r.hotkey))
        .peekable();
    candidates.peek()?;
    // アプリを限った割り当てがあるときだけ、前面のプロセス名を調べる（1 回だけ）。
    let mut process: Option<Option<String>> = None;
    for rule in candidates {
        if !rule.apps.is_empty() {
            let name = process.get_or_insert_with(excel_check::foreground_process_name);
            if !rule.applies_to(name.as_deref()) {
                continue;
            }
        }
        return Some((rule.action.clone(), rule.hotkey));
    }
    None
}

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
        // マウスのボタンを押したことを知るためのフック（Ctrl+クリックを 2 回したのを、Ctrl の
        // 2 回押しと数えないように）。設置できなくても、キーボードの動作には影響しない。
        match SetWindowsHookExW(WH_MOUSE_LL, Some(low_level_mouse_proc), HINSTANCE(hmod.0), 0) {
            Ok(mouse) => MOUSE_HOOK_HANDLE.store(mouse.0 as isize, Ordering::SeqCst),
            Err(e) => log::warn!("マウスのフックを設置できませんでした: {e}"),
        }
    }
    Ok(())
}

/// 設置済みのマウスのフック。0 は未設置。
static MOUSE_HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);

/// 低レベルマウスフックのコールバック。ボタンを押したら 2 回押しの数えを戻すだけで、
/// マウスの動きには何もしない。
unsafe extern "system" fn low_level_mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0
        && matches!(
            wparam.0 as u32,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        )
    {
        DOUBLE_TAP.with(|d| d.borrow_mut().reset());
        crate::ime_indicator::note_click();
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// 文字を打つキーか（修飾キーそのものと、Ctrl・Alt・Win を押しながらのキーは除く）。
unsafe fn is_typing_key(vk: u32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    const MODIFIERS: [u32; 11] = [0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5];
    if MODIFIERS.contains(&vk) {
        return false;
    }
    // 0x11 = Ctrl、0x12 = Alt、0x5B / 0x5C = Win
    ![0x11, 0x12, 0x5B, 0x5C].iter().any(|key| GetAsyncKeyState(*key) < 0)
}

/// キーボードフックを解除する（リソースリーク防止のため終了時に必ず呼ぶ）。
pub fn uninstall() {
    let mouse = MOUSE_HOOK_HANDLE.swap(0, Ordering::SeqCst);
    if mouse != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(mouse as *mut core::ffi::c_void));
        }
    }
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

/// 押されている修飾キーが、設定した組み合わせとちょうど一致するか。
///
/// アプリが「離した」を送ったが利用者はまだ押している修飾キー（[`RELEASED_BY_US`]）も、
/// 押されているものとして数える。
fn modifiers_match(hotkey: &Hotkey) -> bool {
    let mut ours = RELEASED_BY_US.load(Ordering::SeqCst);
    if ours != 0
        && unsafe { GetTickCount() }.wrapping_sub(RELEASED_AT.load(Ordering::SeqCst))
            > RELEASED_STALE_MS
    {
        RELEASED_BY_US.store(0, Ordering::SeqCst);
        ours = 0;
    }
    let held = |vk_down: bool, bit: u32| vk_down || ours & bit != 0;
    let matched = held(is_down(VK_CONTROL), MOD_CTRL) == hotkey.ctrl
        && held(is_down(VK_SHIFT), MOD_SHIFT) == hotkey.shift
        && held(is_down(VK_MENU), MOD_ALT) == hotkey.alt
        && held(is_down(VK_LWIN) || is_down(VK_RWIN), MOD_WIN) == hotkey.win;
    if matched && ours != 0 {
        // 印のおかげで一致した（Alt を押したまま続けて押している）。使われている間は、印を
        // 古くなったとみなさない。
        RELEASED_AT.store(unsafe { GetTickCount() }, Ordering::SeqCst);
    }
    matched
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

        // 利用者が修飾キーを実際に離した・押し直したら、「アプリが離した」の印を消す
        // （ここからは GetAsyncKeyState が正しい状態を表す）。PowerToys や AutoHotkey で
        // キーを入れ替えている人は、修飾キーが注入された入力として届くので、注入でも消す
        // （このアプリ自身が送ったものだけは除く）。
        if !is_self {
            let bit = modifier_bit(kb.vkCode);
            if bit != 0 {
                RELEASED_BY_US.fetch_and(!bit, Ordering::SeqCst);
            }
        }

        // 自分が送った入力・注入入力は対象外。
        if !injected && !is_self {
            // 文字を打ち始めたら、入力モードの小さな印を出す（Ctrl+C のようなショートカットや、
            // Shift などの修飾キーだけでは出さない）。
            if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN) && is_typing_key(kb.vkCode) {
                crate::ime_indicator::note_key();
            }
            // Ctrl などの 2 回押しで、クリップボードの履歴の一覧を出す（キーはそのまま通す）。
            if let Some(key) = history_double_tap() {
                let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
                let interval = HISTORY_DOUBLE_TAP_MS.load(Ordering::SeqCst);
                let fired = DOUBLE_TAP
                    .with(|d| d.borrow_mut().on_key(key, kb.vkCode, down, kb.time, interval));
                if fired {
                    log::debug!("クリップボードの履歴: 2 回押しを捕まえました");
                    actions::dispatch(Action::ClipboardHistory, Hotkey::unpack(0));
                }
            }
            let hotkey = hotkey();
            match message {
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    // リマップ中のキーの押しっぱなし（オートリピート）は、修飾キーの
                    // 状態にかかわらず離すまで握りつぶす（再送もしない）。
                    if kb.vkCode != 0 && kb.vkCode == ACTIVE_VK.load(Ordering::SeqCst) {
                        let last = ACTIVE_TIME.swap(kb.time, Ordering::SeqCst);
                        if kb.time.wrapping_sub(last) < ACTIVE_STALE_MS {
                            return LRESULT(1);
                        }
                        // 「離した」を見逃していた。新しく押されたものとして扱う。
                        ACTIVE_VK.store(0, Ordering::SeqCst);
                    }
                    // 対象アプリかつ ON かつ修飾キーが一致するときだけリマップする。
                    if kb.vkCode == hotkey.vk && modifiers_match(&hotkey) {
                        if is_enabled() && is_target_foreground() {
                            set_active(kb.vkCode, kb.time);
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
                    // クリップボードの履歴の一覧を出すキー（キー割り当ての ON/OFF にかかわらず効く）。
                    if let Some(history) = history_hotkey() {
                        if kb.vkCode == history.vk && modifiers_match(&history) {
                            set_active(kb.vkCode, kb.time);
                            log::debug!("クリップボードの履歴: {} を捕まえました", history.format());
                            actions::dispatch(Action::ClipboardHistory, history);
                            return LRESULT(1);
                        }
                    }
                    // キー割り当て。実行は別のスレッドに頼み、ここではすぐに戻る。
                    if let Some((action, rule_hotkey)) = find_rule(kb.vkCode) {
                        set_active(kb.vkCode, kb.time);
                        log::debug!("キー割り当て: {} を捕まえました", rule_hotkey.format());
                        actions::dispatch(action, rule_hotkey);
                        return LRESULT(1);
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
