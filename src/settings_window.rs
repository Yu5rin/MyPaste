//! 設定画面。
//!
//! トレイメニューの「設定...」から開く。編集できるのは次の項目で、「保存」を押すと
//! `settings.json` に書き込み、メインスレッドへ [`TrayMessage::SettingsSaved`] を送って
//! その場で反映させる（再起動は要らない）。
//!
//! - キーリマップ: 値貼り付けを起動するキーの組み合わせ（Ctrl / Shift / Alt + キー）と、
//!   対象アプリ（プロセス名）
//! - 入力モードの表示: ON/OFF、表示位置（画面中央・マウスの近く・入力位置の近く）、色、
//!   表示時間とフェードアウトの時間（秒）、大きさ、不透明度、全画面のアプリでは出さない
//!   （プレビュー付き）
//! - トラブル調査用の動作の記録（`log.txt`）の ON/OFF と「記録を開く」
//! - 自動起動、起動時の更新確認
//! - クリップボードの履歴: ON/OFF、一覧を出す操作（キーの組み合わせ、Ctrl・Shift・Alt の
//!   2 回押し）、2 回押しの間隔、覚えておく件数、一覧を出す位置、終了後も残すか、履歴を消す
//! - 設定の書き出し・読み込み（PC の引っ越し用。キー割り当てと自動起動の状態も含む）
//!
//! 外部のクレートを足さず、Win32 の標準コントロール（ボタン・エディット・コンボボックス）
//! だけで組み立てている。画面は専用スレッドで動かし、メインスレッドのメニュー処理を
//! 止めない。高 DPI（Per-Monitor V2）に対応し、モニター間を移動したときは
//! `WM_DPICHANGED` で配置と文字の大きさを作り直す。
//!
//! 入力の解析と検証は [`crate::remap_logic`] / [`crate::ime_logic`] にあり、Linux でも
//! テストできる。

use std::cell::RefCell;
use std::sync::mpsc::Sender;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{DeleteObject, HFONT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, PostQuitMessage, SetForegroundWindow, ShowWindow,
    MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, SW_SHOW, SW_SHOWNORMAL, WM_COMMAND,
    WM_DESTROY, WM_DPICHANGED,
};

use crate::clip_history::{self, MenuPosition, Theme as HistoryTheme, Trigger};
use crate::config::{self, ClipboardHistorySettings, Settings};
use crate::hotkey_rules;
use crate::ime_indicator::{Params, Timing};
use crate::ime_logic::{
    self, Position, Theme, FADE_MS_MAX, FADE_MS_MIN, HOLD_MS_MAX, HOLD_MS_MIN, OPACITY_MAX,
    OPACITY_MIN, SIZE_MAX, SIZE_MIN,
};
use crate::remap_logic::{self, Hotkey, KEYS};
use crate::ui::{
    self, combo_index, get_text, is_checked, item, parse_in_range, report_invalid, set_checked,
    set_combo_index, set_text, show_message, wide, Item, Kind, SingleWindow, BN_CLICKED,
    CBN_CLOSEUP, CBN_SELCHANGE,
};
use crate::{actions, ime_indicator, logging, startup, TrayMessage};

// --- コントロールの ID ---
// 「保存」「キャンセル」は IDOK / IDCANCEL と同じ値にして、Enter / Esc で押せるようにする
// （IsDialogMessageW がそのように送ってくる）。
const ID_SAVE: i32 = 1;
const ID_CANCEL: i32 = 2;
const ID_CTRL: i32 = 101;
const ID_SHIFT: i32 = 102;
const ID_ALT: i32 = 103;
const ID_KEY: i32 = 104;
const ID_APPS: i32 = 105;
const ID_IME_ENABLED: i32 = 110;
const ID_HOLD: i32 = 111;
const ID_SIZE: i32 = 112;
const ID_FADE: i32 = 114;
const ID_POSITION: i32 = 115;
const ID_THEME: i32 = 116;
const ID_OPACITY: i32 = 117;
const ID_FULLSCREEN: i32 = 118;
const ID_LOG: i32 = 122;
const ID_OPEN_LOG: i32 = 123;
const ID_PREVIEW: i32 = 113;
const ID_STARTUP: i32 = 120;
const ID_UPDATE: i32 = 121;
const ID_OPEN_FOLDER: i32 = 130;
const ID_DEFAULTS: i32 = 131;
const ID_CH_ENABLED: i32 = 140;
const ID_CH_TRIGGER: i32 = 141;
const ID_CH_CTRL: i32 = 142;
const ID_CH_SHIFT: i32 = 143;
const ID_CH_ALT: i32 = 144;
const ID_CH_WIN: i32 = 145;
const ID_CH_KEY: i32 = 146;
const ID_CH_INTERVAL: i32 = 147;
const ID_CH_MAX: i32 = 148;
const ID_CH_POSITION: i32 = 149;
const ID_CH_KEEP: i32 = 150;
const ID_CH_CLEAR: i32 = 151;
const ID_CH_WIDTH: i32 = 152;
const ID_CH_OPACITY: i32 = 153;
const ID_CH_PAGE: i32 = 154;
const ID_CH_THEME: i32 = 155;
const ID_EXPORT: i32 = 160;
const ID_IMPORT: i32 = 161;


/// 画面の中身の大きさ（96 DPI 基準）。
const CLIENT_W: i32 = 948;
const CLIENT_H: i32 = 578;

/// 画面の構成。並び順がそのまま Tab キーで移る順になる。
/// 見出しや枠など、ID で触らないものは 200 番台の通し番号にしている。
const ITEMS: &[Item] = &[
    // キーリマップ
    item(200, Kind::Group, "値貼り付け（Ctrl+Shift+V）を送るキー", 12, 8, 456, 147),
    item(201, Kind::Label, "キーの組み合わせ", 24, 30, 112, 24),
    item(ID_CTRL, Kind::Check, "Ctrl", 140, 31, 58, 22),
    item(ID_SHIFT, Kind::Check, "Shift", 200, 31, 64, 22),
    item(ID_ALT, Kind::Check, "Alt", 266, 31, 52, 22),
    item(202, Kind::Label, "+", 322, 30, 16, 24),
    item(ID_KEY, Kind::Combo, "", 342, 30, 110, 300),
    item(203, Kind::Label, "対象アプリ（プロセス名を 1 行に 1 つ。例: EXCEL.EXE）", 24, 60, 428, 22),
    item(ID_APPS, Kind::MultiEdit, "", 24, 84, 428, 58),
    // 入力モードの表示
    item(210, Kind::Group, "入力モードの表示", 12, 163, 456, 238),
    item(ID_IME_ENABLED, Kind::Check, "IME の入力モードを切り替えたら大きく表示する", 24, 185, 428, 22),
    item(217, Kind::Label, "表示位置", 24, 214, 112, 24),
    item(ID_POSITION, Kind::Combo, "", 140, 214, 150, 200),
    item(218, Kind::Label, "色", 306, 214, 40, 24),
    item(ID_THEME, Kind::Combo, "", 350, 214, 102, 200),
    item(211, Kind::Label, "表示時間", 24, 246, 112, 24),
    item(ID_HOLD, Kind::Edit, "", 140, 246, 70, 24),
    item(212, Kind::Label, "秒（0.1〜5）", 216, 246, 160, 24),
    item(215, Kind::Label, "フェードアウト", 24, 278, 112, 24),
    item(ID_FADE, Kind::Edit, "", 140, 278, 70, 24),
    item(216, Kind::Label, "秒（0〜2。0 ですぐ消す）", 216, 278, 200, 24),
    item(213, Kind::Label, "大きさ", 24, 310, 112, 24),
    item(ID_SIZE, Kind::NumberEdit, "", 140, 310, 70, 24),
    item(214, Kind::Label, "px（40〜600）", 216, 310, 130, 24),
    item(ID_PREVIEW, Kind::Button, "プレビュー", 362, 308, 90, 28),
    item(219, Kind::Label, "不透明度", 24, 342, 112, 24),
    item(ID_OPACITY, Kind::NumberEdit, "", 140, 342, 70, 24),
    item(221, Kind::Label, "%（30〜100）", 216, 342, 130, 24),
    item(ID_FULLSCREEN, Kind::Check, "全画面のアプリ（ゲーム・動画・発表など）の間は表示しない", 24, 371, 428, 22),
    // 起動・更新・記録
    item(220, Kind::Group, "起動・更新・記録", 12, 409, 456, 106),
    item(ID_STARTUP, Kind::Check, "Windows にサインインしたら自動で起動する", 24, 431, 428, 22),
    item(ID_UPDATE, Kind::Check, "起動時に新しいバージョンを確認する", 24, 458, 428, 22),
    item(ID_LOG, Kind::Check, "トラブル調査用に動作を記録する（log.txt）", 24, 485, 330, 22),
    item(ID_OPEN_LOG, Kind::Button, "記録を開く", 362, 482, 90, 28),
    // クリップボードの履歴（右の列）
    item(230, Kind::Group, "クリップボードの履歴", 480, 8, 456, 394),
    item(ID_CH_ENABLED, Kind::Check, "コピーした文字を記録して、一覧から選んで貼り付ける", 492, 30, 432, 22),
    item(231, Kind::Label, "一覧を出す操作", 492, 60, 120, 24),
    item(ID_CH_TRIGGER, Kind::Combo, "", 616, 60, 308, 200),
    item(232, Kind::Label, "キー", 492, 92, 120, 24),
    item(ID_CH_CTRL, Kind::Check, "Ctrl", 616, 93, 54, 22),
    item(ID_CH_SHIFT, Kind::Check, "Shift", 672, 93, 60, 22),
    item(ID_CH_ALT, Kind::Check, "Alt", 734, 93, 48, 22),
    item(ID_CH_WIN, Kind::Check, "Win", 784, 93, 52, 22),
    item(233, Kind::Label, "+", 616, 122, 16, 24),
    item(ID_CH_KEY, Kind::Combo, "", 634, 122, 150, 300),
    item(234, Kind::Label, "2 回押す間隔", 492, 154, 120, 24),
    item(ID_CH_INTERVAL, Kind::Edit, "", 616, 154, 70, 24),
    item(235, Kind::Label, "秒以内（0.2〜1）", 692, 154, 200, 24),
    item(236, Kind::Label, "覚えておく件数", 492, 186, 120, 24),
    item(ID_CH_MAX, Kind::NumberEdit, "", 616, 186, 70, 24),
    item(237, Kind::Label, "件（10〜10000）", 692, 186, 200, 24),
    item(238, Kind::Label, "一覧を出す位置", 492, 218, 120, 24),
    item(ID_CH_POSITION, Kind::Combo, "", 616, 218, 200, 200),
    item(242, Kind::Label, "一覧の幅", 492, 250, 120, 24),
    item(ID_CH_WIDTH, Kind::NumberEdit, "", 616, 250, 54, 24),
    item(243, Kind::Label, "px", 674, 250, 30, 24),
    item(244, Kind::Label, "不透明度", 712, 250, 70, 24),
    item(ID_CH_OPACITY, Kind::NumberEdit, "", 784, 250, 46, 24),
    item(245, Kind::Label, "%", 834, 250, 30, 24),
    item(246, Kind::Label, "1 ページの件数", 492, 282, 120, 24),
    item(ID_CH_PAGE, Kind::NumberEdit, "", 616, 282, 54, 24),
    item(247, Kind::Label, "件", 674, 282, 30, 24),
    item(248, Kind::Label, "配色", 712, 282, 64, 24),
    item(ID_CH_THEME, Kind::Combo, "", 784, 282, 140, 200),
    item(ID_CH_KEEP, Kind::Check, "アプリを終了しても履歴を残す（暗号化して保存）", 492, 312, 432, 22),
    item(239, Kind::Note, "一覧はクリックか矢印キーと Enter で選びます。一覧の上で Shift+ホイールを回すと不透明度を変えられます。パスワード管理ソフトなどの内容は記録しません。", 492, 338, 334, 58),
    item(ID_CH_CLEAR, Kind::Button, "履歴を消す", 834, 344, 90, 28),
    // 設定の書き出し・読み込み（右の列）
    item(240, Kind::Group, "設定の書き出し・読み込み（PC の引っ越しに）", 480, 410, 456, 116),
    item(241, Kind::Note, "保存済みの設定・キー割り当て・定型文・自動起動の状態を 1 つのファイルにまとめます。新しい PC では、このアプリを置いてから「読み込む」を押します（履歴の中身は含めません）。", 492, 432, 432, 52),
    item(ID_EXPORT, Kind::Button, "設定を書き出す...", 492, 488, 150, 28),
    item(ID_IMPORT, Kind::Button, "設定を読み込む...", 650, 488, 150, 28),
    // 操作ボタン
    item(ID_OPEN_FOLDER, Kind::Button, "設定ファイルの場所を開く", 12, 538, 178, 28),
    item(ID_DEFAULTS, Kind::Button, "既定に戻す", 198, 538, 86, 28),
    item(ID_SAVE, Kind::DefaultButton, "保存", 758, 538, 86, 28),
    item(ID_CANCEL, Kind::Button, "キャンセル", 850, 538, 86, 28),
];

/// 画面に表示する値。
struct Form {
    hotkey: Hotkey,
    target_apps: Vec<String>,
    ime_enabled: bool,
    ime: Params,
    startup: bool,
    check_update: bool,
    log: bool,
    history: ClipboardHistorySettings,
}

impl Form {
    fn from_settings(settings: &Settings, startup: bool) -> Form {
        let (hotkey, _) = Hotkey::from_setting(&settings.remap.hotkey);
        Form {
            hotkey,
            target_apps: remap_logic::effective_target_apps(&settings.remap.target_apps),
            ime_enabled: settings.ime_indicator.enabled,
            ime: Params::from_settings(&settings.ime_indicator),
            startup,
            check_update: settings.update.check_on_startup,
            log: settings.log.enabled,
            history: settings.clipboard_history.clone(),
        }
    }
}

/// 画面のスレッドが持つ状態。
struct Context {
    tx: Sender<TrayMessage>,
    /// 開いたときの設定。保存時はこれに編集した項目を重ねてメインスレッドへ渡す。
    settings: Settings,
    font: HFONT,
}

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

/// 開いている設定画面。
static WINDOW: SingleWindow = SingleWindow::new();

/// 設定画面を開く。すでに開いていれば手前に出す。
///
/// `settings` は現在の設定（トレイで切り替えた入力モード表示の ON/OFF も反映済みのもの）、
/// `startup` は現在の自動起動の状態。
pub fn open(tx: Sender<TrayMessage>, settings: Settings, startup: bool) {
    WINDOW.open(move || unsafe { thread_main(tx, settings, startup) });
}

/// 設定画面が開いていれば閉じ（保存はしない）、スレッドが終わるのを待つ。アプリの終了時に呼ぶ。
pub fn close() {
    WINDOW.close();
}

unsafe fn thread_main(tx: Sender<TrayMessage>, settings: Settings, startup: bool) {
    ui::init_common_controls();
    let title = format!("設定 - アタイの貼り付け v{}", env!("CARGO_PKG_VERSION"));
    let Some(hwnd) = ui::create_top_window(w!("AtaiPasteSettings"), Some(wnd_proc), &title) else {
        log::error!("設定画面を作成できませんでした");
        show_message(HWND::default(), "設定画面を開けませんでした。", MB_ICONERROR);
        return;
    };
    let dpi = GetDpiForWindow(hwnd);
    let font = ui::create_ui_font(dpi);
    let form = Form::from_settings(&settings, startup);
    CONTEXT.with(|c| *c.borrow_mut() = Some(Context { tx, settings, font }));

    ui::create_controls(hwnd, ITEMS, font);
    ui::add_combo_items(hwnd, ID_KEY, KEYS.iter().map(|(name, _)| *name));
    ui::add_combo_items(hwnd, ID_POSITION, Position::ALL.iter().map(|(_, _, name)| *name));
    ui::add_combo_items(hwnd, ID_THEME, Theme::ALL.iter().map(|(_, _, name)| *name));
    ui::add_combo_items(hwnd, ID_CH_TRIGGER, Trigger::ALL.iter().map(|(_, _, name)| *name));
    ui::add_combo_items(hwnd, ID_CH_KEY, KEYS.iter().map(|(name, _)| *name));
    ui::add_combo_items(hwnd, ID_CH_POSITION, MenuPosition::ALL.iter().map(|(_, _, name)| *name));
    ui::add_combo_items(hwnd, ID_CH_THEME, HistoryTheme::ALL.iter().map(|(_, _, name)| *name));
    fill_form(hwnd, &form);
    ui::place_window(hwnd, dpi, CLIENT_W, CLIENT_H);
    ui::layout(hwnd, ITEMS, dpi);
    WINDOW.set_window(hwnd);

    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    ui::focus(hwnd, ID_KEY);

    ui::run_message_loop(hwnd);

    CONTEXT.with(|c| {
        if let Some(context) = c.borrow_mut().take() {
            let _ = DeleteObject(context.font);
        }
    });
}

/// 値を画面に入れる。
unsafe fn fill_form(hwnd: HWND, form: &Form) {
    set_checked(hwnd, ID_CTRL, form.hotkey.ctrl);
    set_checked(hwnd, ID_SHIFT, form.hotkey.shift);
    set_checked(hwnd, ID_ALT, form.hotkey.alt);
    let key_index = KEYS.iter().position(|(_, vk)| *vk == form.hotkey.vk).unwrap_or(1);
    set_combo_index(hwnd, ID_KEY, key_index);
    set_text(hwnd, ID_APPS, &remap_logic::apps_to_text(&form.target_apps));
    set_checked(hwnd, ID_IME_ENABLED, form.ime_enabled);
    let ime = &form.ime;
    let position_index = Position::ALL.iter().position(|(p, _, _)| *p == ime.position).unwrap_or(0);
    set_combo_index(hwnd, ID_POSITION, position_index);
    let theme_index = Theme::ALL.iter().position(|(t, _, _)| *t == ime.theme).unwrap_or(0);
    set_combo_index(hwnd, ID_THEME, theme_index);
    set_text(hwnd, ID_HOLD, &ime_logic::format_seconds(ime.timing.hold_ms));
    set_text(hwnd, ID_FADE, &ime_logic::format_seconds(ime.timing.fade_ms));
    set_text(hwnd, ID_SIZE, &ime.size.to_string());
    set_text(hwnd, ID_OPACITY, &ime.opacity.to_string());
    set_checked(hwnd, ID_FULLSCREEN, ime.hide_in_fullscreen);
    set_checked(hwnd, ID_STARTUP, form.startup);
    set_checked(hwnd, ID_UPDATE, form.check_update);
    set_checked(hwnd, ID_LOG, form.log);
    fill_history(hwnd, &form.history);
}

/// クリップボードの履歴の設定を画面に入れる。
unsafe fn fill_history(hwnd: HWND, settings: &ClipboardHistorySettings) {
    let config = clip_history::Config::from_settings(settings);
    set_checked(hwnd, ID_CH_ENABLED, config.enabled);
    let trigger_index = Trigger::ALL.iter().position(|(t, _, _)| *t == config.trigger).unwrap_or(0);
    set_combo_index(hwnd, ID_CH_TRIGGER, trigger_index);
    // 読めないキーが書かれていた場合は、既定のキーを出す。
    let hotkey = config
        .hotkey
        .or_else(|| Hotkey::parse(&ClipboardHistorySettings::default().hotkey).ok())
        .unwrap_or_default();
    set_checked(hwnd, ID_CH_CTRL, hotkey.ctrl);
    set_checked(hwnd, ID_CH_SHIFT, hotkey.shift);
    set_checked(hwnd, ID_CH_ALT, hotkey.alt);
    set_checked(hwnd, ID_CH_WIN, hotkey.win);
    let key_index = KEYS.iter().position(|(_, vk)| *vk == hotkey.vk).unwrap_or(0);
    set_combo_index(hwnd, ID_CH_KEY, key_index);
    set_text(hwnd, ID_CH_INTERVAL, &ime_logic::format_seconds(u64::from(config.double_tap_ms)));
    set_text(hwnd, ID_CH_MAX, &config.max_items.to_string());
    let position_index = MenuPosition::ALL
        .iter()
        .position(|(p, _, _)| *p == config.position)
        .unwrap_or(0);
    set_combo_index(hwnd, ID_CH_POSITION, position_index);
    set_checked(hwnd, ID_CH_KEEP, config.keep_after_exit);
    set_text(hwnd, ID_CH_WIDTH, &config.width.to_string());
    set_text(hwnd, ID_CH_OPACITY, &config.opacity.to_string());
    set_text(hwnd, ID_CH_PAGE, &config.page_size.to_string());
    let theme_index = HistoryTheme::ALL.iter().position(|(t, _, _)| *t == config.theme).unwrap_or(0);
    set_combo_index(hwnd, ID_CH_THEME, theme_index);
    update_history_fields(hwnd);
}

/// 一覧を出す操作に合わせて、キーの欄と間隔の欄を使える・使えないにする。
unsafe fn update_history_fields(hwnd: HWND) {
    let on = is_checked(hwnd, ID_CH_ENABLED);
    let trigger = combo_index(hwnd, ID_CH_TRIGGER)
        .and_then(|i| Trigger::ALL.get(i))
        .map_or(Trigger::Hotkey, |(t, _, _)| *t);
    let uses_key = on && trigger == Trigger::Hotkey;
    for id in [ID_CH_CTRL, ID_CH_SHIFT, ID_CH_ALT, ID_CH_WIN, ID_CH_KEY] {
        ui::set_enabled(hwnd, id, uses_key);
    }
    ui::set_enabled(hwnd, ID_CH_INTERVAL, on && trigger != Trigger::Hotkey);
    for id in [
        ID_CH_TRIGGER,
        ID_CH_MAX,
        ID_CH_POSITION,
        ID_CH_KEEP,
        ID_CH_WIDTH,
        ID_CH_OPACITY,
        ID_CH_PAGE,
        ID_CH_THEME,
    ] {
        ui::set_enabled(hwnd, id, on);
    }
}

/// クリップボードの履歴の設定を読み、検証する。
unsafe fn read_history(hwnd: HWND) -> Result<ClipboardHistorySettings, (i32, String)> {
    let trigger = combo_index(hwnd, ID_CH_TRIGGER)
        .and_then(|i| Trigger::ALL.get(i))
        .map_or(Trigger::Hotkey, |(t, _, _)| *t);
    let vk = combo_index(hwnd, ID_CH_KEY)
        .and_then(|i| KEYS.get(i))
        .map(|(_, vk)| *vk)
        .unwrap_or(0);
    let hotkey = Hotkey {
        ctrl: is_checked(hwnd, ID_CH_CTRL),
        shift: is_checked(hwnd, ID_CH_SHIFT),
        alt: is_checked(hwnd, ID_CH_ALT),
        win: is_checked(hwnd, ID_CH_WIN),
        vk,
    };
    let enabled = is_checked(hwnd, ID_CH_ENABLED);
    if enabled && trigger == Trigger::Hotkey {
        hotkey
            .validate_trigger()
            .map_err(|e| (ID_CH_KEY, format!("一覧を出すキー: {e}")))?;
    }
    let interval = ime_logic::parse_seconds(
        &get_text(hwnd, ID_CH_INTERVAL),
        u64::from(clip_history::DOUBLE_TAP_MS_MIN),
        u64::from(clip_history::DOUBLE_TAP_MS_MAX),
    )
    .ok_or_else(|| {
        (
            ID_CH_INTERVAL,
            "2 回押す間隔は 0.2〜1 秒の数で入力してください（例: 0.4）。".to_string(),
        )
    })?;
    let max_items = parse_in_range(
        &get_text(hwnd, ID_CH_MAX),
        clip_history::MIN_ITEMS as u64,
        clip_history::MAX_ITEMS_LIMIT as u64,
    )
    .ok_or_else(|| {
        (
            ID_CH_MAX,
            format!(
                "覚えておく件数は {}〜{} の数で入力してください。",
                clip_history::MIN_ITEMS,
                clip_history::MAX_ITEMS_LIMIT
            ),
        )
    })? as usize;
    let number = |id: i32, min: u64, max: u64, name: &str, unit: &str| {
        parse_in_range(&get_text(hwnd, id), min, max)
            .ok_or_else(|| (id, format!("{name}は {min}〜{max} の数で入力してください（{unit}）。")))
    };
    let width = number(
        ID_CH_WIDTH,
        u64::from(clip_history::MIN_WIDTH),
        u64::from(clip_history::MAX_WIDTH),
        "一覧の幅",
        "ピクセル",
    )? as u32;
    let opacity = number(
        ID_CH_OPACITY,
        u64::from(clip_history::MIN_OPACITY),
        u64::from(clip_history::MAX_OPACITY),
        "不透明度",
        "%",
    )? as u32;
    let page_size = number(
        ID_CH_PAGE,
        clip_history::MIN_PAGE_SIZE as u64,
        clip_history::MAX_PAGE_SIZE as u64,
        "1 ページの件数",
        "件",
    )? as usize;
    let position = combo_index(hwnd, ID_CH_POSITION)
        .and_then(|i| MenuPosition::ALL.get(i))
        .map_or(MenuPosition::Caret, |(p, _, _)| *p);
    Ok(ClipboardHistorySettings {
        enabled,
        trigger: trigger.as_setting().to_string(),
        hotkey: hotkey.format(),
        double_tap_ms: interval as u32,
        max_items,
        keep_after_exit: is_checked(hwnd, ID_CH_KEEP),
        position: position.as_setting().to_string(),
        width,
        opacity,
        page_size,
        theme: combo_index(hwnd, ID_CH_THEME)
            .and_then(|i| HistoryTheme::ALL.get(i))
            .map_or("system", |(t, _, _)| t.as_setting())
            .to_string(),
    })
}

/// 画面の値を読み、検証する。誤りがあれば、直すべきコントロールの ID と理由を返す。
unsafe fn read_form(hwnd: HWND) -> Result<Form, (i32, String)> {
    let vk = combo_index(hwnd, ID_KEY)
        .and_then(|i| KEYS.get(i))
        .map(|(_, vk)| *vk)
        .unwrap_or(0);
    let hotkey = Hotkey {
        ctrl: is_checked(hwnd, ID_CTRL),
        shift: is_checked(hwnd, ID_SHIFT),
        alt: is_checked(hwnd, ID_ALT),
        // 値貼り付けの画面には Win の欄が無い。settings.json で指定されていれば残す。
        win: CONTEXT.with(|c| {
            c.borrow()
                .as_ref()
                .is_some_and(|context| Hotkey::from_setting(&context.settings.remap.hotkey).0.win)
        }),
        vk,
    };
    hotkey.validate().map_err(|e| (ID_KEY, e))?;

    let target_apps = remap_logic::normalize_apps([get_text(hwnd, ID_APPS).as_str()]);
    if target_apps.is_empty() {
        return Err((ID_APPS, "対象アプリを 1 つ以上入力してください（例: EXCEL.EXE）。".into()));
    }

    let ime = read_ime_params(hwnd)?;
    let history = read_history(hwnd)?;

    Ok(Form {
        hotkey,
        target_apps,
        ime_enabled: is_checked(hwnd, ID_IME_ENABLED),
        ime,
        startup: is_checked(hwnd, ID_STARTUP),
        check_update: is_checked(hwnd, ID_UPDATE),
        log: is_checked(hwnd, ID_LOG),
        history,
    })
}

/// 入力モード表示の出し方（位置・色・時間・大きさ・不透明度・全画面）を読む。
unsafe fn read_ime_params(hwnd: HWND) -> Result<Params, (i32, String)> {
    let timing = read_timing(hwnd)?;
    let size = read_size(hwnd)?;
    let opacity = parse_in_range(
        &get_text(hwnd, ID_OPACITY),
        u64::from(OPACITY_MIN),
        u64::from(OPACITY_MAX),
    )
    .ok_or_else(|| {
        (
            ID_OPACITY,
            format!("不透明度は {OPACITY_MIN}〜{OPACITY_MAX} の数で入力してください。"),
        )
    })? as u32;
    let position = combo_index(hwnd, ID_POSITION)
        .and_then(|i| Position::ALL.get(i))
        .map(|(p, _, _)| *p)
        .unwrap_or(Position::Center);
    let theme = combo_index(hwnd, ID_THEME)
        .and_then(|i| Theme::ALL.get(i))
        .map(|(t, _, _)| *t)
        .unwrap_or(Theme::Dark);
    Ok(Params {
        timing,
        size,
        position,
        theme,
        opacity,
        hide_in_fullscreen: is_checked(hwnd, ID_FULLSCREEN),
    })
}

/// 表示時間とフェードアウトの時間（秒で入力）を読む。
unsafe fn read_timing(hwnd: HWND) -> Result<Timing, (i32, String)> {
    let hold_ms = ime_logic::parse_seconds(&get_text(hwnd, ID_HOLD), HOLD_MS_MIN, HOLD_MS_MAX)
        .ok_or_else(|| {
            (
                ID_HOLD,
                format!(
                    "表示時間は {}〜{} 秒の数で入力してください（例: 0.4、1.5）。",
                    ime_logic::format_seconds(HOLD_MS_MIN),
                    ime_logic::format_seconds(HOLD_MS_MAX)
                ),
            )
        })?;
    let fade_ms = ime_logic::parse_seconds(&get_text(hwnd, ID_FADE), FADE_MS_MIN, FADE_MS_MAX)
        .ok_or_else(|| {
            (
                ID_FADE,
                format!(
                    "フェードアウトは {}〜{} 秒の数で入力してください（例: 0.25）。",
                    ime_logic::format_seconds(FADE_MS_MIN),
                    ime_logic::format_seconds(FADE_MS_MAX)
                ),
            )
        })?;
    Ok(Timing { hold_ms, fade_ms })
}

/// 表示の大きさを読む。
unsafe fn read_size(hwnd: HWND) -> Result<u32, (i32, String)> {
    parse_in_range(&get_text(hwnd, ID_SIZE), u64::from(SIZE_MIN), u64::from(SIZE_MAX))
        .map(|v| v as u32)
        .ok_or_else(|| {
            (
                ID_SIZE,
                format!("大きさは {SIZE_MIN}〜{SIZE_MAX} の数で入力してください。"),
            )
        })
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            if (id == ID_CH_TRIGGER && (code == CBN_SELCHANGE || code == CBN_CLOSEUP))
                || (id == ID_CH_ENABLED && code == BN_CLICKED)
            {
                update_history_fields(hwnd);
            }
            if code == BN_CLICKED {
                match id {
                    ID_SAVE => on_save(hwnd),
                    ID_CANCEL => {
                        let _ = DestroyWindow(hwnd);
                    }
                    ID_PREVIEW => on_preview(hwnd),
                    ID_DEFAULTS => on_defaults(hwnd),
                    ID_OPEN_FOLDER => on_open_folder(hwnd),
                    ID_OPEN_LOG => on_open_log(hwnd),
                    ID_CH_CLEAR => on_clear_history(hwnd),
                    ID_EXPORT => on_export(hwnd),
                    ID_IMPORT => on_import(hwnd),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // 別の DPI のモニターへ移った。勧められた大きさに合わせ、文字と配置を作り直す。
            let old = CONTEXT.with(|c| c.borrow().as_ref().map(|context| context.font));
            let font = ui::on_dpi_changed(hwnd, ITEMS, wparam, lparam, old.unwrap_or_default());
            CONTEXT.with(|c| {
                if let Some(context) = c.borrow_mut().as_mut() {
                    context.font = font;
                }
            });
            LRESULT(0)
        }
        ui::WM_APP_FORCE_CLOSE => {
            let _ = DestroyWindow(hwnd);
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

/// 「保存」: 検証して settings.json に書き、メインスレッドへ反映を頼んで閉じる。
unsafe fn on_save(hwnd: HWND) {
    let form = match read_form(hwnd) {
        Ok(form) => form,
        Err((id, message)) => {
            report_invalid(hwnd, id, &message);
            return;
        }
    };

    let Some((tx, mut settings)) = CONTEXT.with(|c| {
        c.borrow()
            .as_ref()
            .map(|context| (context.tx.clone(), context.settings.clone()))
    }) else {
        return;
    };
    // 値貼り付けのキーが、キー割り当てと重なっていないか確かめる（値貼り付けが先に効くので、
    // 重なった割り当ては使われなくなる）。割り当ては、この画面を開いたあとにキー割り当て画面で
    // 保存されていることもあるので、settings.json から読み直す。
    let hotkeys = config::Settings::load().hotkeys;
    if let Some(message) =
        hotkey_rules::find_conflict(&hotkeys, (form.hotkey, &form.target_apps))
    {
        show_message(hwnd, &message, MB_ICONWARNING);
        ui::focus(hwnd, ID_KEY);
        return;
    }
    if let Some(message) = history_conflict(&form, &hotkeys) {
        show_message(hwnd, &message, MB_ICONWARNING);
        ui::focus(hwnd, ID_CH_KEY);
        return;
    }
    settings.remap.hotkey = form.hotkey.format();
    settings.remap.target_apps = form.target_apps;
    settings.ime_indicator.enabled = form.ime_enabled;
    let ime = &form.ime;
    settings.ime_indicator.hold_ms = ime.timing.hold_ms;
    settings.ime_indicator.fade_ms = ime.timing.fade_ms;
    settings.ime_indicator.size = ime.size;
    settings.ime_indicator.position = ime.position.as_setting().to_string();
    settings.ime_indicator.theme = ime.theme.as_setting().to_string();
    settings.ime_indicator.opacity = ime.opacity;
    settings.ime_indicator.hide_in_fullscreen = ime.hide_in_fullscreen;
    settings.log.enabled = form.log;
    settings.update.check_on_startup = form.check_update;
    settings.clipboard_history = form.history;

    if let Err(e) = config::save_from_settings_window(&settings) {
        log::error!("設定の保存に失敗: {e}");
        show_message(hwnd, &format!("設定を保存できませんでした。\n\n{e}"), MB_ICONERROR);
        return;
    }

    // 自動起動はスタートアップフォルダのショートカットで表しているので、変わったときだけ作り直す。
    let mut startup_error = None;
    if form.startup != startup::is_enabled() {
        let result = if form.startup {
            startup::enable()
        } else {
            startup::disable()
        };
        if let Err(e) = result {
            log::error!("自動起動設定失敗: {e}");
            startup_error = Some(e.to_string());
        }
    }

    let _ = tx.send(TrayMessage::SettingsSaved(Box::new(settings)));
    log::info!("設定を保存しました");

    if let Some(e) = startup_error {
        show_message(
            hwnd,
            &format!("設定は保存しましたが、自動起動の設定を変えられませんでした。\n\n詳細: {e}"),
            MB_ICONWARNING,
        );
    }
    let _ = DestroyWindow(hwnd);
}

/// 履歴の一覧を出すキーが、値貼り付けのキーやキー割り当てと重なっていれば、その説明を返す。
fn history_conflict(form: &Form, hotkeys: &[config::HotkeyRuleSetting]) -> Option<String> {
    let history = clip_history::Config::from_settings(&form.history);
    if !history.enabled {
        return None;
    }
    let key = history.hotkey?;
    if key == form.hotkey {
        return Some(format!(
            "クリップボードの履歴を出すキー（{}）が、値貼り付けのキーと同じです。別のキーにしてください。",
            key.format()
        ));
    }
    hotkey_rules::find_key_in_rules(hotkeys, key).map(|n| {
        format!(
            "クリップボードの履歴を出すキー（{}）は、キー割り当ての {n} 番目でも使っています。\
             別のキーにするか、キー割り当てを変えてください。",
            key.format()
        )
    })
}

/// 「履歴を消す」: 覚えている履歴と、保存した履歴を消す。
unsafe fn on_clear_history(hwnd: HWND) {
    if ui::ask_yes_no(hwnd, "クリップボードの履歴をすべて消しますか？（元には戻せません）") {
        actions::clear_history();
        show_message(hwnd, "クリップボードの履歴を消しました。", MB_ICONINFORMATION);
    }
}

/// 書き出し・読み込みで選べるファイルの種類。
const FILE_FILTER: &str = "アタイの貼り付けの設定 (*.json)\0*.json\0すべてのファイル (*.*)\0*.*\0";

/// 「設定を書き出す...」: 保存済みの設定・キー割り当て・自動起動の状態を 1 つのファイルにする。
unsafe fn on_export(hwnd: HWND) {
    let Some(path) = ui::choose_file(hwnd, true, FILE_FILTER, "json", "アタイの貼り付けの設定.json")
    else {
        return;
    };
    let result = config::export_all(startup::is_enabled())
        .and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()));
    match result {
        Ok(()) => {
            log::info!("設定を書き出しました（{}）", path.display());
            show_message(
                hwnd,
                &format!(
                    "設定を書き出しました。\n\n{}\n\n\
                     この画面でまだ保存していない変更は含まれません（保存してから書き出してください）。",
                    path.display()
                ),
                MB_ICONINFORMATION,
            );
        }
        Err(e) => show_message(
            hwnd,
            &format!("書き出せませんでした。\n\n{}\n{e}", path.display()),
            MB_ICONERROR,
        ),
    }
}

/// 「設定を読み込む...」: 書き出したファイルで、設定とキー割り当てを置き換えてすぐに使う。
unsafe fn on_import(hwnd: HWND) {
    let Some(path) = ui::choose_file(hwnd, false, FILE_FILTER, "json", "") else {
        return;
    };
    let read = std::fs::metadata(&path)
        .map_err(|e| e.to_string())
        .and_then(|m| {
            if m.len() > config::HOTKEYS_FILE_MAX_BYTES {
                Err("ファイルが大きすぎます".to_string())
            } else {
                std::fs::read_to_string(&path).map_err(|e| e.to_string())
            }
        })
        .and_then(|text| config::parse_import(&text));
    let imported = match read {
        Ok(imported) => imported,
        Err(e) => {
            show_message(
                hwnd,
                &format!("読み込めませんでした。\n\n{}\n{e}", path.display()),
                MB_ICONERROR,
            );
            return;
        }
    };
    if !ui::ask_yes_no(
        hwnd,
        &format!(
            "今の設定とキー割り当て（{} 件）を、読み込んだもの（キー割り当て {} 件）に置き換えます。\n\
             よろしいですか？",
            config::Settings::load().hotkeys.len(),
            imported.settings.hotkeys.len()
        ),
    ) {
        return;
    }
    if let Err(e) = config::replace_settings(&imported.root) {
        log::error!("設定の読み込みに失敗: {e}");
        show_message(hwnd, &format!("設定を書き込めませんでした。\n\n{e}"), MB_ICONERROR);
        return;
    }
    let mut startup_error = None;
    if let Some(autostart) = imported.autostart {
        if autostart != startup::is_enabled() {
            let result = if autostart {
                startup::enable()
            } else {
                startup::disable()
            };
            if let Err(e) = result {
                startup_error = Some(e.to_string());
            }
        }
    }
    log::info!("設定を読み込みました（{}）", path.display());
    let Some(tx) = CONTEXT.with(|c| c.borrow().as_ref().map(|context| context.tx.clone())) else {
        return;
    };
    let _ = tx.send(TrayMessage::SettingsImported(Box::new(imported.settings)));
    let mut message = "設定を読み込み、使い始めました。".to_string();
    if let Some(e) = startup_error {
        message.push_str(&format!("\n\nただし、自動起動の設定を変えられませんでした: {e}"));
    }
    show_message(hwnd, &message, MB_ICONINFORMATION);
    // 画面の値は読み込む前のものなので、閉じる（開き直すと読み込んだ値が出る）。
    let _ = DestroyWindow(hwnd);
}

/// 「プレビュー」: 入力中の表示時間・フェードアウト・大きさで、入力モードを試しに表示する。
unsafe fn on_preview(hwnd: HWND) {
    let params = match read_ime_params(hwnd) {
        Ok(params) => params,
        Err((id, message)) => {
            report_invalid(hwnd, id, &message);
            return;
        }
    };
    if !ime_indicator::preview(params) {
        show_message(
            hwnd,
            "入力モード表示が動いていないため、プレビューできません。アプリを起動し直してください。",
            MB_ICONINFORMATION,
        );
    }
}

/// 「既定に戻す」: 画面の値を既定値にする（保存するまでは反映しない）。
/// 自動起動は既定値を持たない（ショートカットの有無で決まる）ので、今の選択のままにする。
unsafe fn on_defaults(hwnd: HWND) {
    let startup = is_checked(hwnd, ID_STARTUP);
    fill_form(hwnd, &Form::from_settings(&Settings::default(), startup));
}

/// 「記録を開く」: log.txt を既定のアプリ（メモ帳など）で開く。
unsafe fn on_open_log(hwnd: HWND) {
    let Some(path) = logging::log_file() else {
        show_message(hwnd, "記録の場所を決められませんでした。", MB_ICONERROR);
        return;
    };
    if !path.exists() {
        show_message(
            hwnd,
            "まだ記録はありません。\n\n「トラブル調査用に動作を記録する」を ON にして保存すると、記録を始めます。",
            MB_ICONINFORMATION,
        );
        return;
    }
    log::logger().flush();
    let file = wide(&path.display().to_string());
    let result = ShellExecuteW(
        hwnd,
        w!("open"),
        PCWSTR(file.as_ptr()),
        PCWSTR::null(),
        PCWSTR::null(),
        SW_SHOWNORMAL,
    );
    // 32 以下は失敗（ShellExecuteW の仕様）。
    if result.0 as isize <= 32 {
        show_message(
            hwnd,
            &format!("記録を開けませんでした。\n\n{}", path.display()),
            MB_ICONERROR,
        );
    }
}

/// 「設定ファイルの場所を開く」: エクスプローラーで settings.json を選んだ状態で開く。
unsafe fn on_open_folder(hwnd: HWND) {
    let Some(path) = config::settings_file() else {
        show_message(hwnd, "設定ファイルの場所を決められませんでした。", MB_ICONERROR);
        return;
    };
    let (file, params) = if path.exists() {
        ("explorer.exe".to_string(), format!("/select,\"{}\"", path.display()))
    } else {
        let folder = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
        (folder, String::new())
    };
    let file = wide(&file);
    let params = wide(&params);
    let result = ShellExecuteW(
        hwnd,
        w!("open"),
        PCWSTR(file.as_ptr()),
        PCWSTR(params.as_ptr()),
        PCWSTR::null(),
        SW_SHOWNORMAL,
    );
    // 32 以下は失敗（ShellExecuteW の仕様）。
    if result.0 as isize <= 32 {
        show_message(
            hwnd,
            &format!("フォルダーを開けませんでした。\n\n{}", path.display()),
            MB_ICONERROR,
        );
    }
}

