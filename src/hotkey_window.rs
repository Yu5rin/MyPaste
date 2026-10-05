//! キー割り当て画面。
//!
//! トレイメニューの「キー割り当て...」から開く。左に割り当ての一覧、右に選んだ割り当ての
//! 内容を出す。内容を書いて「追加」で一覧に足し、一覧で選んで内容を直したら「更新」で
//! 書き換える。「保存」を押すと `settings.json` の `hotkeys` に書き込み、メインスレッドへ
//! [`TrayMessage::HotkeysSaved`] を送って、その場で使えるようにする（再起動は要らない）。
//!
//! 割り当ての解釈と検証は [`crate::hotkey_rules`] にあり、Linux でもテストできる。
//! 画面の部品は設定画面と共通（[`crate::ui`]）。

use std::cell::RefCell;
use std::sync::mpsc::Sender;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{DeleteObject, HFONT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, PostQuitMessage, SetForegroundWindow, ShowWindow,
    MB_ICONERROR, MB_ICONWARNING, SW_SHOW, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_DPICHANGED,
};

use crate::config::{self, HotkeyRuleSetting};
use crate::hotkey_rules::{self, ActionKind, Field, Rule, ACTIONS};
use crate::remap_logic::{self, Hotkey, KEYS};
use crate::ui::{
    self, combo_index, get_text, is_checked, item, set_checked, set_combo_index, set_text,
    show_message, Item, Kind, SingleWindow, BN_CLICKED, CBN_CLOSEUP, CBN_SELCHANGE, LBN_SELCHANGE,
};
use crate::TrayMessage;

// --- コントロールの ID ---
// 「保存」「キャンセル」は IDOK / IDCANCEL と同じ値（Enter / Esc で押せる）。
const ID_SAVE: i32 = 1;
const ID_CANCEL: i32 = 2;
const ID_LIST: i32 = 301;
const ID_UP: i32 = 302;
const ID_DOWN: i32 = 303;
const ID_DELETE: i32 = 304;
const ID_ENABLED: i32 = 310;
const ID_CTRL: i32 = 311;
const ID_SHIFT: i32 = 312;
const ID_ALT: i32 = 313;
const ID_WIN: i32 = 314;
const ID_KEY: i32 = 315;
const ID_ACTION: i32 = 316;
const ID_VALUE_LABEL: i32 = 317;
const ID_VALUE: i32 = 318;
const ID_HINT: i32 = 319;
const ID_ARGS: i32 = 321;
const ID_SCOPE: i32 = 322;
const ID_APPS: i32 = 323;
const ID_ADD: i32 = 324;
const ID_UPDATE: i32 = 325;

/// 画面の中身の大きさ（96 DPI 基準）。
const CLIENT_W: i32 = 680;
const CLIENT_H: i32 = 496;

/// 効くアプリの選択肢。
const SCOPES: [&str; 2] = ["すべてのアプリ", "下に書いたアプリだけ"];

/// 画面の構成。並び順がそのまま Tab キーで移る順になる。
const ITEMS: &[Item] = &[
    item(400, Kind::Label, "割り当て（上にあるものから順に調べます）", 12, 8, 304, 22),
    item(ID_LIST, Kind::ListBox, "", 12, 32, 304, 340),
    item(ID_UP, Kind::Button, "上へ", 12, 380, 70, 28),
    item(ID_DOWN, Kind::Button, "下へ", 86, 380, 70, 28),
    item(ID_DELETE, Kind::Button, "削除", 234, 380, 82, 28),
    // 内容
    item(401, Kind::Group, "割り当ての内容", 324, 8, 344, 400),
    item(ID_ENABLED, Kind::Check, "この割り当てを使う", 336, 30, 320, 22),
    item(402, Kind::Label, "キー", 336, 58, 48, 24),
    item(ID_CTRL, Kind::Check, "Ctrl", 388, 59, 52, 22),
    item(ID_SHIFT, Kind::Check, "Shift", 442, 59, 58, 22),
    item(ID_ALT, Kind::Check, "Alt", 502, 59, 46, 22),
    item(ID_WIN, Kind::Check, "Win", 550, 59, 52, 22),
    item(403, Kind::Label, "+", 388, 88, 16, 24),
    item(ID_KEY, Kind::Combo, "", 406, 88, 150, 300),
    item(404, Kind::Label, "動作", 336, 120, 48, 24),
    item(ID_ACTION, Kind::Combo, "", 388, 120, 268, 300),
    item(ID_VALUE_LABEL, Kind::Label, "", 336, 152, 320, 22),
    item(ID_VALUE, Kind::MultiEdit, "", 336, 176, 320, 64),
    item(ID_HINT, Kind::Note, "", 336, 246, 320, 62),
    item(405, Kind::Label, "引数", 336, 312, 48, 24),
    item(ID_ARGS, Kind::Edit, "", 388, 312, 268, 24),
    item(406, Kind::Label, "効くアプリ", 336, 344, 80, 24),
    item(ID_SCOPE, Kind::Combo, "", 420, 344, 236, 200),
    item(ID_APPS, Kind::Edit, "", 336, 374, 320, 24),
    item(ID_ADD, Kind::Button, "新しい割り当てとして追加", 324, 416, 172, 28),
    item(ID_UPDATE, Kind::Button, "選んだ割り当てを更新", 500, 416, 168, 28),
    // 操作ボタン
    item(407, Kind::Note, "「保存」を押すと、すぐに使えるようになります。効くアプリはプロセス名を空白で区切って書きます（例: EXCEL.EXE WINWORD.EXE）。", 12, 452, 472, 40),
    item(ID_SAVE, Kind::DefaultButton, "保存", 496, 456, 82, 28),
    item(ID_CANCEL, Kind::Button, "キャンセル", 586, 456, 82, 28),
];

/// 画面のスレッドが持つ状態。
struct Context {
    tx: Sender<TrayMessage>,
    /// 編集中の割り当て（保存するまで settings.json には書かない）。
    rules: Vec<HotkeyRuleSetting>,
    /// 値貼り付けのキーと対象アプリ（同じキーを使っていないか確かめるため）。
    remap: (Hotkey, Vec<String>),
    font: HFONT,
    /// 保存していない変更があるか。
    dirty: bool,
}

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

/// 開いているキー割り当て画面。
static WINDOW: SingleWindow = SingleWindow::new();

/// キー割り当て画面を開く。すでに開いていれば手前に出す。
///
/// `rules` は現在の割り当て、`remap` は値貼り付けのキーと対象アプリ。
pub fn open(tx: Sender<TrayMessage>, rules: Vec<HotkeyRuleSetting>, remap: (Hotkey, Vec<String>)) {
    WINDOW.open(move || unsafe { thread_main(tx, rules, remap) });
}

/// 開いていれば閉じ（保存はしない）、スレッドが終わるのを待つ。アプリの終了時に呼ぶ。
pub fn close() {
    WINDOW.close();
}

unsafe fn thread_main(
    tx: Sender<TrayMessage>,
    rules: Vec<HotkeyRuleSetting>,
    remap: (Hotkey, Vec<String>),
) {
    ui::init_common_controls();
    let title = format!("キー割り当て - アタイの貼り付け v{}", env!("CARGO_PKG_VERSION"));
    let Some(hwnd) = ui::create_top_window(w!("AtaiPasteHotkeys"), Some(wnd_proc), &title) else {
        log::error!("キー割り当て画面を作成できませんでした");
        show_message(HWND::default(), "キー割り当て画面を開けませんでした。", MB_ICONERROR);
        return;
    };
    let dpi = GetDpiForWindow(hwnd);
    let font = ui::create_ui_font(dpi);
    CONTEXT.with(|c| {
        *c.borrow_mut() = Some(Context {
            tx,
            rules,
            remap,
            font,
            dirty: false,
        })
    });

    ui::create_controls(hwnd, ITEMS, font);
    ui::add_combo_items(hwnd, ID_KEY, KEYS.iter().map(|(name, _)| *name));
    ui::add_combo_items(hwnd, ID_ACTION, ACTIONS.iter().map(|a| a.name));
    ui::add_combo_items(hwnd, ID_SCOPE, SCOPES.iter().copied());
    // 最初の割り当てを選んでおく（無ければ新しく作るときの初期値を出す）。
    let first = CONTEXT.with(|c| c.borrow().as_ref().and_then(|ctx| ctx.rules.first().cloned()));
    refresh_list(hwnd, first.as_ref().map(|_| 0));
    load_editor(hwnd, &first.unwrap_or_else(new_rule));
    ui::place_window(hwnd, dpi, CLIENT_W, CLIENT_H);
    ui::layout(hwnd, ITEMS, dpi);
    WINDOW.set_window(hwnd);

    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    ui::focus(hwnd, ID_LIST);

    ui::run_message_loop(hwnd);

    CONTEXT.with(|c| {
        if let Some(context) = c.borrow_mut().take() {
            let _ = DeleteObject(context.font);
        }
    });
}

/// 新しく作るときの初期値。
fn new_rule() -> HotkeyRuleSetting {
    HotkeyRuleSetting {
        hotkey: "Ctrl+Alt+A".into(),
        action: "send_keys".into(),
        ..Default::default()
    }
}

/// 一覧に出す文字。
fn list_texts() -> Vec<String> {
    CONTEXT.with(|c| {
        c.borrow()
            .as_ref()
            .map(|ctx| ctx.rules.iter().map(hotkey_rules::describe).collect())
            .unwrap_or_default()
    })
}

unsafe fn refresh_list(hwnd: HWND, select: Option<usize>) {
    ui::set_list_items(hwnd, ID_LIST, &list_texts(), select);
}

/// 割り当てを右側の欄に入れる。
unsafe fn load_editor(hwnd: HWND, rule: &HotkeyRuleSetting) {
    set_checked(hwnd, ID_ENABLED, rule.enabled);
    let hotkey = Hotkey::parse(&rule.hotkey).ok();
    set_checked(hwnd, ID_CTRL, hotkey.is_some_and(|h| h.ctrl));
    set_checked(hwnd, ID_SHIFT, hotkey.is_some_and(|h| h.shift));
    set_checked(hwnd, ID_ALT, hotkey.is_some_and(|h| h.alt));
    set_checked(hwnd, ID_WIN, hotkey.is_some_and(|h| h.win));
    let key_index = hotkey
        .and_then(|h| KEYS.iter().position(|(_, vk)| *vk == h.vk))
        .unwrap_or(0);
    set_combo_index(hwnd, ID_KEY, key_index);
    let action_index = ActionKind::from_setting(&rule.action)
        .and_then(|kind| ACTIONS.iter().position(|a| a.kind == kind))
        .unwrap_or(0);
    set_combo_index(hwnd, ID_ACTION, action_index);
    // 複数行の欄は改行を \r\n で渡す。
    set_text(hwnd, ID_VALUE, &rule.value.replace("\r\n", "\n").replace('\n', "\r\n"));
    set_text(hwnd, ID_ARGS, &rule.args);
    let apps = remap_logic::normalize_apps(rule.apps.iter().map(String::as_str));
    set_combo_index(hwnd, ID_SCOPE, usize::from(!apps.is_empty()));
    set_text(hwnd, ID_APPS, &apps_to_line(&apps));
    update_fields(hwnd);
}

/// 効くアプリを 1 行にする（空白を含む名前は " で囲む）。
fn apps_to_line(apps: &[String]) -> String {
    apps.iter()
        .map(|a| {
            if a.chars().any(char::is_whitespace) {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 動作に合わせて、内容の欄の見出し・説明と、使える欄を切り替える。
unsafe fn update_fields(hwnd: HWND) {
    let info = &ACTIONS[combo_index(hwnd, ID_ACTION).unwrap_or(0).min(ACTIONS.len() - 1)];
    set_text(
        hwnd,
        ID_VALUE_LABEL,
        info.value_label.unwrap_or("（この動作には内容はありません）"),
    );
    set_text(hwnd, ID_HINT, info.hint);
    ui::set_enabled(hwnd, ID_VALUE, info.value_label.is_some());
    ui::set_enabled(hwnd, ID_ARGS, info.uses_args);
    ui::set_enabled(hwnd, ID_APPS, combo_index(hwnd, ID_SCOPE) == Some(1));
}

/// 右側の欄を読み、検証する。誤りがあれば、直すべき欄の ID と理由を返す。
unsafe fn read_editor(hwnd: HWND) -> Result<HotkeyRuleSetting, (i32, String)> {
    let vk = combo_index(hwnd, ID_KEY)
        .and_then(|i| KEYS.get(i))
        .map(|(_, vk)| *vk)
        .unwrap_or(0);
    let hotkey = Hotkey {
        ctrl: is_checked(hwnd, ID_CTRL),
        shift: is_checked(hwnd, ID_SHIFT),
        alt: is_checked(hwnd, ID_ALT),
        win: is_checked(hwnd, ID_WIN),
        vk,
    };
    let info = &ACTIONS[combo_index(hwnd, ID_ACTION).unwrap_or(0).min(ACTIONS.len() - 1)];
    let apps = if combo_index(hwnd, ID_SCOPE) == Some(1) {
        let apps = remap_logic::normalize_apps([get_text(hwnd, ID_APPS).as_str()]);
        if apps.is_empty() {
            return Err((
                ID_APPS,
                "効くアプリを 1 つ以上入力してください（例: EXCEL.EXE）。すべてのアプリで使うときは「すべてのアプリ」を選びます。".into(),
            ));
        }
        apps
    } else {
        Vec::new()
    };
    let setting = HotkeyRuleSetting {
        enabled: is_checked(hwnd, ID_ENABLED),
        hotkey: hotkey.format(),
        action: info.key.to_string(),
        // 改行は \n にそろえて保存する。内容を使わない動作では空にする。
        value: if info.value_label.is_some() {
            get_text(hwnd, ID_VALUE).replace("\r\n", "\n")
        } else {
            String::new()
        },
        args: if info.uses_args {
            get_text(hwnd, ID_ARGS).trim().to_string()
        } else {
            String::new()
        },
        apps,
    };
    if let Err(e) = Rule::from_setting(&setting) {
        let id = match e.field {
            Field::Hotkey => ID_KEY,
            Field::Action => ID_ACTION,
            Field::Value => ID_VALUE,
        };
        return Err((id, format!("{}。", e.message.trim_end_matches('。'))));
    }
    Ok(setting)
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            match (id, code) {
                (ID_LIST, LBN_SELCHANGE) => on_select(hwnd),
                (ID_ACTION | ID_SCOPE, CBN_SELCHANGE | CBN_CLOSEUP) => update_fields(hwnd),
                (ID_ADD, BN_CLICKED) => on_add(hwnd),
                (ID_UPDATE, BN_CLICKED) => on_update(hwnd),
                (ID_DELETE, BN_CLICKED) => on_delete(hwnd),
                (ID_UP, BN_CLICKED) => on_move(hwnd, -1),
                (ID_DOWN, BN_CLICKED) => on_move(hwnd, 1),
                (ID_SAVE, BN_CLICKED) => on_save(hwnd),
                (ID_CANCEL, BN_CLICKED) => on_close(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            on_close(hwnd);
            LRESULT(0)
        }
        ui::WM_APP_FORCE_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let old = CONTEXT.with(|c| c.borrow().as_ref().map(|context| context.font));
            let font = ui::on_dpi_changed(hwnd, ITEMS, wparam, lparam, old.unwrap_or_default());
            CONTEXT.with(|c| {
                if let Some(context) = c.borrow_mut().as_mut() {
                    context.font = font;
                }
            });
            LRESULT(0)
        }
        WM_DESTROY => {
            WINDOW.set_window(HWND::default());
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 一覧で選んだ割り当てを右側に出す。
unsafe fn on_select(hwnd: HWND) {
    let Some(index) = ui::list_index(hwnd, ID_LIST) else {
        return;
    };
    let rule = CONTEXT.with(|c| c.borrow().as_ref().and_then(|ctx| ctx.rules.get(index).cloned()));
    if let Some(rule) = rule {
        load_editor(hwnd, &rule);
    }
}

/// 右側の内容を、新しい割り当てとして一覧の最後に足す。
unsafe fn on_add(hwnd: HWND) {
    let setting = match read_editor(hwnd) {
        Ok(setting) => setting,
        Err((id, message)) => return ui::report_invalid(hwnd, id, &message),
    };
    let index = CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let ctx = c.as_mut()?;
        ctx.rules.push(setting);
        ctx.dirty = true;
        Some(ctx.rules.len() - 1)
    });
    refresh_list(hwnd, index);
}

/// 一覧で選んでいる割り当てを、右側の内容で書き換える。
unsafe fn on_update(hwnd: HWND) {
    let Some(index) = ui::list_index(hwnd, ID_LIST) else {
        show_message(
            hwnd,
            "書き換える割り当てを、左の一覧で選んでください。新しく足すときは「新しい割り当てとして追加」を押します。",
            MB_ICONWARNING,
        );
        return;
    };
    let setting = match read_editor(hwnd) {
        Ok(setting) => setting,
        Err((id, message)) => return ui::report_invalid(hwnd, id, &message),
    };
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            if let Some(rule) = ctx.rules.get_mut(index) {
                *rule = setting;
                ctx.dirty = true;
            }
        }
    });
    refresh_list(hwnd, Some(index));
}

/// 一覧で選んでいる割り当てを消す。
unsafe fn on_delete(hwnd: HWND) {
    let Some(index) = ui::list_index(hwnd, ID_LIST) else {
        return;
    };
    let next = CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let ctx = c.as_mut()?;
        if index >= ctx.rules.len() {
            return None;
        }
        ctx.rules.remove(index);
        ctx.dirty = true;
        // 消したあとは、同じ位置（最後なら 1 つ上）を選ぶ。
        (!ctx.rules.is_empty()).then(|| index.min(ctx.rules.len() - 1))
    });
    refresh_list(hwnd, next);
    if next.is_some() {
        on_select(hwnd);
    }
}

/// 一覧で選んでいる割り当てを上下に動かす（上にあるものが先に調べられる）。
unsafe fn on_move(hwnd: HWND, delta: isize) {
    let Some(index) = ui::list_index(hwnd, ID_LIST) else {
        return;
    };
    let moved = CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let ctx = c.as_mut()?;
        let to = index.checked_add_signed(delta).filter(|to| *to < ctx.rules.len())?;
        ctx.rules.swap(index, to);
        ctx.dirty = true;
        Some(to)
    });
    if let Some(to) = moved {
        refresh_list(hwnd, Some(to));
    }
}

/// 「保存」: 同じキーの重なりを確かめて settings.json に書き、メインスレッドへ反映を頼んで閉じる。
unsafe fn on_save(hwnd: HWND) {
    let Some((tx, rules, remap)) = CONTEXT.with(|c| {
        c.borrow()
            .as_ref()
            .map(|ctx| (ctx.tx.clone(), ctx.rules.clone(), ctx.remap.clone()))
    }) else {
        return;
    };
    if let Some(message) = hotkey_rules::find_conflict(&rules, (remap.0, &remap.1)) {
        show_message(hwnd, &message, MB_ICONWARNING);
        return;
    }
    if let Err(e) = config::save_hotkeys(&rules) {
        log::error!("キー割り当ての保存に失敗: {e}");
        show_message(hwnd, &format!("キー割り当てを保存できませんでした。\n\n{e}"), MB_ICONERROR);
        return;
    }
    log::info!("キー割り当てを保存しました（{} 件）", rules.len());
    let _ = tx.send(TrayMessage::HotkeysSaved(rules));
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.dirty = false;
        }
    });
    let _ = DestroyWindow(hwnd);
}

/// 「キャンセル」・閉じるボタン: 保存していない変更があれば確かめてから閉じる。
unsafe fn on_close(hwnd: HWND) {
    let dirty = CONTEXT.with(|c| c.borrow().as_ref().is_some_and(|ctx| ctx.dirty));
    if dirty
        && !ui::ask_yes_no(
            hwnd,
            "保存していない変更があります。保存せずに閉じますか？",
        )
    {
        return;
    }
    let _ = DestroyWindow(hwnd);
}
