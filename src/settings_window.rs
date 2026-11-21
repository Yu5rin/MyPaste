//! 設定画面。
//!
//! トレイメニューの「設定...」から開く。5 つのタブ（ページ）に分けてあり、「OK」か「適用」を
//! 押すと `settings.json` に書き込み、メインスレッドへ [`TrayMessage::SettingsSaved`] を送って
//! その場で反映させる（再起動は要らない）。「適用」は閉じずに保存する。保存していない変更が
//! あるまま閉じようとしたら、確かめる。
//!
//! - 全般: 自動起動、更新の確認（起動時・起動している間の数時間おき）と、新しいバージョンが
//!   見つかったときの動き、ログ（`log.txt`）、設定の書き出し・読み込み、既定値に戻す
//! - 値貼り付け: 押すキー（Ctrl / Shift / Alt + キー）と、対象アプリ（プロセス名）
//! - 入力モード: 表示の ON/OFF、位置、配色、表示時間とフェードアウトの時間（秒）、大きさ、
//!   不透明度、全画面のアプリでは出さない（プレビュー付き）
//! - クリップボードの履歴: ON/OFF、一覧を出す操作（キーの組み合わせ、Ctrl・Shift・Alt の
//!   2 回押し）、一覧の見た目、残す件数、終了後も残すか、履歴に残さないアプリ、履歴の削除
//! - アプリごとの入力: アプリに切り替えたときに、自動で入れる入力モード（日本語・英字）の一覧
//!
//! タブは見出しの並びだけを使い、すべてのコントロールを同じ画面の上に作って、選ばれた
//! ページのものだけを見せる（[`show_page`]）。
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
use windows::Win32::UI::Controls::NMHDR;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    GetDlgItem, IsWindow,
    DefWindowProcW, DestroyWindow, PostQuitMessage, SetForegroundWindow, ShowWindow, WM_CLOSE,
    WM_NOTIFY,
    MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, SW_SHOW, SW_SHOWNORMAL, WM_COMMAND,
    WM_DESTROY, WM_DPICHANGED,
};

use crate::clip_history::{self, MenuPosition, Theme as HistoryTheme, Trigger};
use crate::config::{
    self, AppImeModeSetting, ClipboardHistorySettings, ImeIndicatorSettings, Settings,
};
use crate::hotkey_rules;
use crate::ime_indicator::{Params, Timing};
use crate::ime_logic::{
    self, AppImeMode, AppModeRule, Badge, BadgeWhen, Position, Theme, FADE_MS_MAX, FADE_MS_MIN, HOLD_MS_MAX, HOLD_MS_MIN, OPACITY_MAX,
    OPACITY_MIN, SIZE_MAX, SIZE_MIN,
};
use crate::remap_logic::{self, Hotkey, KEYS};
use crate::update_logic::UpdateMode;
use crate::ui::{
    self, combo_index, get_text, is_checked, item, parse_in_range, report_invalid, set_checked,
    set_combo_index, set_text, show_message, wide, Item, Kind, SingleWindow, BN_CLICKED,
    CBN_CLOSEUP, CBN_SELCHANGE,
};
use crate::{actions, ime_indicator, logging, process_picker, startup, TrayMessage};

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
const ID_UPDATE_PERIODIC: i32 = 124;
const ID_UPDATE_MODE: i32 = 125;
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
const ID_CH_MOVE_TOP: i32 = 156;
const ID_CH_EXCLUDE: i32 = 157;
const ID_EXPORT: i32 = 160;
const ID_IMPORT: i32 = 161;
const ID_PICK_APP: i32 = 162;
const ID_CH_PICK_EXCLUDE: i32 = 163;
const ID_CH_KEY_LABEL: i32 = 164;
const ID_CH_PLUS: i32 = 165;
const ID_CH_INTERVAL_LABEL: i32 = 166;
const ID_CH_INTERVAL_UNIT: i32 = 167;


const ID_APPLY: i32 = 170;
const ID_TABS: i32 = 171;
const ID_BADGE: i32 = 180;
const ID_BADGE_SIZE: i32 = 181;
const ID_FLASH: i32 = 182;
const ID_CH_IMAGES: i32 = 183;
const ID_CH_KEEP_OPEN: i32 = 184;
const ID_BADGE_WHEN: i32 = 185;
const ID_BADGE_HOLD: i32 = 186;
const ID_BADGE_OPACITY: i32 = 187;
const ID_BADGE_IBEAM: i32 = 188;
const ID_BADGE_ENABLED: i32 = 189;
const ID_BADGE_KEEP: i32 = 190;
const ID_NO_CAPTURE: i32 = 191;
const ID_AM_ENABLED: i32 = 192;
const ID_AM_LIST: i32 = 193;
const ID_AM_APP: i32 = 194;
const ID_AM_PICK: i32 = 195;
const ID_AM_MODE: i32 = 196;
const ID_AM_ADD: i32 = 197;
const ID_AM_REMOVE: i32 = 198;

/// 画面の中身の大きさ（96 DPI 基準）。ノート PC（1366x768、表示 125%）でも収まる大きさにする。
const CLIENT_W: i32 = 500;
const CLIENT_H: i32 = 472;

/// タブの見出し（[`PAGES`] と同じ順）。
const TAB_NAMES: [&str; 5] = ["全般", "値貼り付け", "入力モード", "クリップボードの履歴", "アプリごとの入力"];

/// どのページでも出すもの（タブ。作る順が Tab キーで移る順になるので、最初に作る）。
const HEAD: &[Item] = &[item(ID_TABS, Kind::Tabs, "", 8, 8, 484, 28)];

/// どのページでも出すもの（下のボタン。最後に作る）。
const FOOT: &[Item] = &[
    item(ID_SAVE, Kind::DefaultButton, "OK", 230, 432, 84, 28),
    item(ID_CANCEL, Kind::Button, "キャンセル", 320, 432, 84, 28),
    item(ID_APPLY, Kind::Button, "適用(&A)", 410, 432, 78, 28),
];

// 見出しや枠など、ID で触らないものは 200 番台の通し番号にしている。

/// 「全般」のページ。
const PAGE_GENERAL: &[Item] = &[
    item(200, Kind::Group, "起動と更新", 12, 44, 476, 136),
    item(ID_STARTUP, Kind::Check, "Windows の起動時に自動で実行する", 24, 64, 452, 22),
    item(ID_UPDATE, Kind::Check, "起動時に新しいバージョンを確認する", 24, 90, 452, 22),
    item(ID_UPDATE_PERIODIC, Kind::Check, "起動している間も 6 時間おきに確認する", 24, 116, 452, 22),
    item(201, Kind::Label, "新しいバージョンが見つかったら", 24, 144, 210, 24),
    item(ID_UPDATE_MODE, Kind::Combo, "", 238, 144, 200, 200),
    item(202, Kind::Group, "ログ（トラブル調査用）", 12, 188, 476, 58),
    item(ID_LOG, Kind::Check, "動作のログを保存する（log.txt）", 24, 211, 330, 22),
    item(ID_OPEN_LOG, Kind::Button, "ログを開く(&L)", 376, 207, 100, 28),
    item(203, Kind::Group, "設定の引っ越しとバックアップ", 12, 254, 476, 116),
    item(204, Kind::Note, "設定・キー割り当て・定型文を 1 つのファイルにまとめます。新しい PC では、このアプリを置いてから「読み込む」を押します（履歴の中身は含みません）。", 24, 274, 452, 44),
    item(ID_EXPORT, Kind::Button, "書き出す(&E)...", 24, 328, 120, 28),
    item(ID_IMPORT, Kind::Button, "読み込む(&I)...", 150, 328, 120, 28),
    item(ID_OPEN_FOLDER, Kind::Button, "設定フォルダーを開く(&F)", 306, 328, 170, 28),
    item(ID_DEFAULTS, Kind::Button, "すべて既定値に戻す(&R)...", 12, 382, 190, 28),
];

/// 「値貼り付け」のページ。
const PAGE_REMAP: &[Item] = &[
    item(210, Kind::Note, "対象アプリで下のキーを押すと、値貼り付け（Ctrl+Shift+V）を送ります。Excel で、書式を付けずに値だけを貼り付けるのに使います。", 16, 48, 468, 40),
    item(211, Kind::Group, "押すキー", 12, 92, 476, 62),
    item(ID_CTRL, Kind::Check, "Ctrl", 24, 116, 58, 22),
    item(ID_SHIFT, Kind::Check, "Shift", 84, 116, 64, 22),
    item(ID_ALT, Kind::Check, "Alt", 150, 116, 52, 22),
    item(212, Kind::Label, "+", 206, 115, 16, 24),
    item(ID_KEY, Kind::Combo, "", 226, 115, 110, 300),
    item(213, Kind::Group, "対象アプリ", 12, 162, 476, 208),
    item(214, Kind::Label, "プロセス名を 1 行に 1 つ（例: EXCEL.EXE）", 24, 184, 320, 24),
    item(ID_PICK_APP, Kind::Button, "アプリを選ぶ(&B)...", 346, 182, 130, 28),
    item(ID_APPS, Kind::MultiEdit, "", 24, 216, 452, 142),
];

/// 小さな印を出す場所の選択肢（出すかどうかは、チェックボックスで決める）。
const BADGE_PLACES: [(Badge, &str); 2] = [
    (Badge::Caret, "入力位置の横（分からないときはマウスの横）"),
    (Badge::Mouse, "マウスカーソルの横"),
];

/// 「入力モード」のページ。
const PAGE_IME: &[Item] = &[
    item(ID_IME_ENABLED, Kind::Check, "入力モード（あ / A）を表示する（メニューの「入力モードの表示」と同じ）", 16, 48, 468, 22),
    item(220, Kind::Group, "切り替えたとき", 12, 72, 476, 206),
    item(ID_FLASH, Kind::Check, "切り替えたときに、画面に大きく表示する", 24, 92, 440, 22),
    item(221, Kind::Label, "表示する位置", 24, 118, 112, 24),
    item(ID_POSITION, Kind::Combo, "", 140, 118, 170, 200),
    item(222, Kind::Label, "配色", 24, 144, 112, 24),
    item(ID_THEME, Kind::Combo, "", 140, 144, 170, 200),
    item(223, Kind::Label, "表示時間", 24, 170, 112, 24),
    item(ID_HOLD, Kind::Edit, "", 140, 170, 70, 24),
    item(224, Kind::Label, "秒（0.1〜5）", 216, 170, 200, 24),
    item(225, Kind::Label, "フェードアウト", 24, 196, 112, 24),
    item(ID_FADE, Kind::Edit, "", 140, 196, 70, 24),
    item(226, Kind::Label, "秒（0〜2。0 ですぐに消す）", 216, 196, 220, 24),
    item(227, Kind::Label, "大きさ", 24, 222, 112, 24),
    item(ID_SIZE, Kind::NumberEdit, "", 140, 222, 70, 24),
    item(228, Kind::Label, "px（40〜600）", 216, 222, 150, 24),
    item(229, Kind::Label, "不透明度", 24, 248, 112, 24),
    item(ID_OPACITY, Kind::NumberEdit, "", 140, 248, 70, 24),
    item(230, Kind::Label, "%（30〜100）", 216, 248, 150, 24),
    item(ID_PREVIEW, Kind::Button, "プレビュー(&P)", 366, 246, 110, 28),
    item(231, Kind::Group, "小さな印", 12, 280, 476, 122),
    item(ID_BADGE_ENABLED, Kind::Check, "いまの入力モードを、小さな印で出す", 24, 298, 440, 22),
    item(232, Kind::Label, "出す場所", 24, 322, 66, 24),
    item(ID_BADGE, Kind::Combo, "", 92, 322, 262, 200),
    item(233, Kind::Label, "大きさ", 364, 322, 44, 24),
    item(ID_BADGE_SIZE, Kind::NumberEdit, "", 410, 322, 40, 24),
    item(234, Kind::Label, "px", 454, 322, 24, 24),
    item(235, Kind::Label, "出すとき", 24, 348, 66, 24),
    item(ID_BADGE_WHEN, Kind::Combo, "", 92, 348, 210, 200),
    item(ID_BADGE_HOLD, Kind::Edit, "", 308, 348, 32, 24),
    item(236, Kind::Label, "秒", 344, 348, 16, 24),
    item(237, Kind::Label, "不透明度", 366, 348, 56, 24),
    item(ID_BADGE_OPACITY, Kind::NumberEdit, "", 424, 348, 34, 24),
    item(238, Kind::Label, "%", 462, 348, 16, 24),
    item(ID_BADGE_KEEP, Kind::Check, "入力位置では出し続ける", 24, 374, 170, 22),
    item(ID_BADGE_IBEAM, Kind::Check, "マウスが I の形のときも出す", 200, 374, 284, 22),
    item(ID_FULLSCREEN, Kind::Check, "全画面のアプリでは表示しない", 16, 404, 212, 22),
    item(ID_NO_CAPTURE, Kind::Check, "スクリーンショット・画面共有に写さない", 232, 404, 252, 22),
];

/// 「クリップボードの履歴」のページ。
const PAGE_HISTORY: &[Item] = &[
    item(ID_CH_ENABLED, Kind::Check, "コピーした文字や画像を履歴に残し、一覧から選んで貼り付ける", 16, 48, 468, 22),
    item(240, Kind::Group, "一覧を出す操作", 12, 72, 476, 86),
    item(241, Kind::Label, "操作", 24, 92, 100, 24),
    item(ID_CH_TRIGGER, Kind::Combo, "", 128, 92, 348, 200),
    // キーの組み合わせのときはキーの欄を、2 回押しのときは間隔の欄を、同じ行に出し分ける。
    item(ID_CH_KEY_LABEL, Kind::Label, "キー", 24, 122, 100, 24),
    item(ID_CH_CTRL, Kind::Check, "Ctrl", 128, 123, 54, 22),
    item(ID_CH_SHIFT, Kind::Check, "Shift", 184, 123, 60, 22),
    item(ID_CH_ALT, Kind::Check, "Alt", 246, 123, 48, 22),
    item(ID_CH_WIN, Kind::Check, "Win", 296, 123, 52, 22),
    item(ID_CH_PLUS, Kind::Label, "+", 350, 122, 14, 24),
    item(ID_CH_KEY, Kind::Combo, "", 366, 122, 110, 300),
    item(ID_CH_INTERVAL_LABEL, Kind::Label, "2 回押しの間隔", 24, 122, 100, 24),
    item(ID_CH_INTERVAL, Kind::Edit, "", 128, 122, 70, 24),
    item(ID_CH_INTERVAL_UNIT, Kind::Label, "秒以内（0.2〜1）", 204, 122, 200, 24),
    item(242, Kind::Group, "一覧の見た目", 12, 162, 476, 110),
    item(243, Kind::Label, "出す位置", 24, 182, 100, 24),
    item(ID_CH_POSITION, Kind::Combo, "", 128, 182, 176, 200),
    item(244, Kind::Label, "配色", 314, 182, 36, 24),
    item(ID_CH_THEME, Kind::Combo, "", 352, 182, 124, 200),
    item(245, Kind::Label, "幅（px）", 24, 212, 100, 24),
    item(ID_CH_WIDTH, Kind::NumberEdit, "", 128, 212, 50, 24),
    item(246, Kind::Label, "不透明度（%）", 196, 212, 90, 24),
    item(ID_CH_OPACITY, Kind::NumberEdit, "", 288, 212, 44, 24),
    item(247, Kind::Label, "1 ページの件数", 346, 212, 86, 24),
    item(ID_CH_PAGE, Kind::NumberEdit, "", 436, 212, 40, 24),
    item(ID_CH_KEEP_OPEN, Kind::Check, "貼り付けても、一覧を出したままにする（Esc で閉じる）", 24, 242, 452, 22),
    item(248, Kind::Group, "履歴の保存", 12, 276, 476, 148),
    item(249, Kind::Label, "残す件数", 24, 296, 100, 24),
    item(ID_CH_MAX, Kind::NumberEdit, "", 128, 296, 70, 24),
    item(250, Kind::Label, "件（10〜10000）", 204, 296, 110, 24),
    item(ID_CH_CLEAR, Kind::Button, "履歴をすべて削除(&D)...", 316, 294, 160, 28),
    item(ID_CH_KEEP, Kind::Check, "アプリを終了しても履歴を残す（暗号化して保存）", 24, 324, 452, 22),
    item(ID_CH_IMAGES, Kind::Check, "画像（スクリーンショットなど）も履歴に残す（合計 500 MB まで）", 24, 346, 452, 22),
    item(ID_CH_MOVE_TOP, Kind::Check, "一覧から貼り付けたものを、いちばん上に移す", 24, 368, 452, 22),
    item(251, Kind::Label, "履歴に残さないアプリ", 24, 394, 130, 24),
    item(ID_CH_EXCLUDE, Kind::Edit, "", 158, 394, 208, 24),
    item(ID_CH_PICK_EXCLUDE, Kind::Button, "アプリを選ぶ...", 372, 392, 104, 28),
];

/// 「アプリごとの入力」のページ。
const PAGE_APP_MODES: &[Item] = &[
    item(ID_AM_ENABLED, Kind::Check, "アプリに切り替えたときに、入力モードを自動で切り替える", 16, 48, 468, 22),
    item(260, Kind::Note, "下の一覧のアプリに切り替えると、選んだ入力モードにします。切り替えたあとは自由に変えられ、そのアプリから離れて戻ってきたときに、また切り替えます。入力モードの表示を OFF にしていても使えます。", 16, 74, 468, 58),
    item(261, Kind::Group, "アプリと入力モード", 12, 136, 476, 288),
    item(ID_AM_LIST, Kind::ListBox, "", 24, 156, 452, 146),
    item(262, Kind::Label, "アプリ", 24, 312, 70, 24),
    item(ID_AM_APP, Kind::Edit, "", 98, 312, 226, 24),
    item(ID_AM_PICK, Kind::Button, "アプリを選ぶ...", 330, 310, 146, 28),
    item(263, Kind::Label, "入力モード", 24, 344, 70, 24),
    item(ID_AM_MODE, Kind::Combo, "", 98, 344, 226, 200),
    item(ID_AM_ADD, Kind::Button, "追加", 330, 342, 70, 28),
    item(ID_AM_REMOVE, Kind::Button, "削除", 406, 342, 70, 28),
    item(264, Kind::Note, "同じアプリを追加すると、入力モードを置き換えます。管理者として実行しているアプリなど、切り替えられないアプリもあります。", 24, 378, 452, 40),
];

/// ページ（[`TAB_NAMES`] と同じ順）。
const PAGES: [&[Item]; 5] = [PAGE_GENERAL, PAGE_REMAP, PAGE_IME, PAGE_HISTORY, PAGE_APP_MODES];

/// すべてのコントロール（作る順 = Tab キーで移る順）。
fn all_items() -> Vec<Item> {
    HEAD.iter()
        .chain(PAGES.iter().flat_map(|page| page.iter()))
        .chain(FOOT.iter())
        .copied()
        .collect()
}

/// そのコントロールがあるページ。
fn page_of(id: i32) -> Option<usize> {
    PAGES.iter().position(|page| page.iter().any(|item| item.id == id))
}

/// 選ばれているタブのページだけを見せる。
unsafe fn show_page(hwnd: HWND, index: usize) {
    for (i, page) in PAGES.iter().enumerate() {
        for item in page.iter() {
            ui::set_visible(hwnd, item.id, i == index);
        }
    }
    if index == 3 {
        // 履歴のページは、出す操作に合わせてキーの欄・間隔の欄を出し分ける。
        update_history_fields(hwnd);
    }
}

/// 入力の誤りを知らせる。その欄のあるページへ切り替えてから、欄に移る。
unsafe fn report_invalid_on_page(hwnd: HWND, id: i32, message: &str) {
    if let Some(page) = page_of(id) {
        ui::set_tab_index(hwnd, ID_TABS, page);
        show_page(hwnd, page);
    }
    report_invalid(hwnd, id, message);
}

/// 画面に表示する値。
struct Form {
    hotkey: Hotkey,
    target_apps: Vec<String>,
    ime_enabled: bool,
    ime: Params,
    startup: bool,
    check_update: bool,
    /// 動いている間に確認する間隔（時間）。0 なら確認しない。
    update_every_hours: u64,
    update_mode: UpdateMode,
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
            update_every_hours: settings.update.check_every_hours,
            update_mode: UpdateMode::from_setting(&settings.update.mode),
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
    /// 開いたとき（最後に保存・適用したとき）の、欄の中身の控え。
    baseline: String,
    /// 「アプリごとの入力」の一覧（画面の一覧と同じ順）。
    app_modes: Vec<AppModeRule>,
}

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

/// 開いている設定画面。
static WINDOW: SingleWindow = SingleWindow::new();

/// 画面が開いているか（尋ねずに更新するとき、編集中に再起動しないように確かめる）。
pub fn is_open() -> bool {
    WINDOW.is_open()
}

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
    CONTEXT.with(|c| {
        *c.borrow_mut() = Some(Context {
            tx,
            settings,
            font,
            baseline: String::new(),
            app_modes: Vec::new(),
        })
    });

    let items = all_items();
    ui::create_controls(hwnd, &items, font);
    ui::add_tabs(hwnd, ID_TABS, &TAB_NAMES);
    ui::add_combo_items(hwnd, ID_KEY, KEYS.iter().map(|(name, _)| *name));
    ui::add_combo_items(hwnd, ID_POSITION, Position::ALL.iter().map(|(_, _, name)| *name));
    ui::add_combo_items(hwnd, ID_THEME, Theme::ALL.iter().map(|(_, _, name)| *name));
    ui::add_combo_items(hwnd, ID_BADGE, BADGE_PLACES.iter().map(|(_, name)| *name));
    ui::add_combo_items(hwnd, ID_BADGE_WHEN, BadgeWhen::ALL.iter().map(|(_, _, name)| *name));
    ui::add_combo_items(hwnd, ID_UPDATE_MODE, UpdateMode::ALL.iter().map(|(_, _, name)| *name));
    ui::add_combo_items(hwnd, ID_CH_TRIGGER, Trigger::ALL.iter().map(|(_, _, name)| *name));
    ui::add_combo_items(hwnd, ID_CH_KEY, KEYS.iter().map(|(name, _)| *name));
    ui::add_combo_items(hwnd, ID_CH_POSITION, MenuPosition::ALL.iter().map(|(_, _, name)| *name));
    ui::add_combo_items(hwnd, ID_CH_THEME, HistoryTheme::ALL.iter().map(|(_, _, name)| *name));
    ui::add_combo_items(hwnd, ID_AM_MODE, AppImeMode::ALL.iter().map(|(_, _, name)| *name));
    fill_form(hwnd, &form);
    ui::place_window(hwnd, dpi, CLIENT_W, CLIENT_H);
    ui::layout(hwnd, &items, dpi);
    show_page(hwnd, 0);
    mark_clean(hwnd);
    WINDOW.set_window(hwnd);

    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    ui::focus(hwnd, ID_TABS);

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
    set_checked(hwnd, ID_NO_CAPTURE, ime.hide_from_capture);
    set_checked(hwnd, ID_FLASH, ime.flash);
    // 「出さない」が書かれていたら、出さないことにして、場所は最初のものを出しておく。
    let badge_index = BADGE_PLACES.iter().position(|(b, _)| *b == ime.badge);
    set_checked(hwnd, ID_BADGE_ENABLED, ime.badge_enabled && badge_index.is_some());
    set_combo_index(hwnd, ID_BADGE, badge_index.unwrap_or(0));
    set_text(hwnd, ID_BADGE_SIZE, &ime.badge_size.to_string());
    let when_index = BadgeWhen::ALL.iter().position(|(w, _, _)| *w == ime.badge_when).unwrap_or(0);
    set_combo_index(hwnd, ID_BADGE_WHEN, when_index);
    set_text(hwnd, ID_BADGE_HOLD, &ime_logic::format_seconds(ime.badge_hold_ms));
    set_text(hwnd, ID_BADGE_OPACITY, &ime.badge_opacity.to_string());
    set_checked(hwnd, ID_BADGE_IBEAM, ime.badge_on_ibeam);
    set_checked(hwnd, ID_BADGE_KEEP, ime.badge_keep_at_caret);
    set_checked(hwnd, ID_STARTUP, form.startup);
    set_checked(hwnd, ID_UPDATE, form.check_update);
    set_checked(hwnd, ID_UPDATE_PERIODIC, form.update_every_hours > 0);
    set_text(
        hwnd,
        ID_UPDATE_PERIODIC,
        &format!("起動している間も {} 時間おきに確認する", periodic_hours(form.update_every_hours)),
    );
    let mode_index = UpdateMode::ALL.iter().position(|(m, _, _)| *m == form.update_mode).unwrap_or(0);
    set_combo_index(hwnd, ID_UPDATE_MODE, mode_index);
    set_checked(hwnd, ID_LOG, form.log);
    fill_history(hwnd, &form.history);
    update_ime_fields(hwnd);
    set_checked(hwnd, ID_AM_ENABLED, ime.app_modes_enabled);
    set_app_modes(hwnd, ime.app_modes.clone(), None);
    set_text(hwnd, ID_AM_APP, "");
    set_combo_index(hwnd, ID_AM_MODE, 0);
    update_app_mode_fields(hwnd);
}

/// 「アプリごとの入力」の一覧を置き換え、画面の一覧に出す。`select` は選んでおく行。
unsafe fn set_app_modes(hwnd: HWND, rules: Vec<AppModeRule>, select: Option<usize>) {
    let labels: Vec<String> = rules.iter().map(AppModeRule::label).collect();
    ui::set_list_items(hwnd, ID_AM_LIST, &labels, select);
    CONTEXT.with(|c| {
        if let Some(context) = c.borrow_mut().as_mut() {
            context.app_modes = rules;
        }
    });
}

/// 「アプリごとの入力」の一覧（画面に出しているもの）。
fn app_modes() -> Vec<AppModeRule> {
    CONTEXT.with(|c| c.borrow().as_ref().map(|context| context.app_modes.clone()).unwrap_or_default())
}

/// アプリごとの入力モードを使わないときは、一覧と入力の欄を使えなくする。
unsafe fn update_app_mode_fields(hwnd: HWND) {
    let on = is_checked(hwnd, ID_AM_ENABLED);
    for id in [ID_AM_LIST, ID_AM_APP, ID_AM_PICK, ID_AM_MODE, ID_AM_ADD, ID_AM_REMOVE] {
        ui::set_enabled(hwnd, id, on);
    }
}

/// 「追加」: 入力したアプリを、選んだ入力モードで一覧に足す（すでにあれば入力モードを置き換える）。
unsafe fn on_add_app_mode(hwnd: HWND) {
    let mode = selected_app_mode(hwnd);
    let mut rules = app_modes();
    let Some(index) = ime_logic::add_app_mode_rule(&mut rules, &get_text(hwnd, ID_AM_APP), mode) else {
        report_invalid(hwnd, ID_AM_APP, "アプリのプロセス名を入力してください（例: EXCEL.EXE）。");
        return;
    };
    set_app_modes(hwnd, rules, Some(index));
    set_text(hwnd, ID_AM_APP, "");
    ui::focus(hwnd, ID_AM_APP);
}

/// 「削除」: 一覧で選んでいるアプリを消す。
unsafe fn on_remove_app_mode(hwnd: HWND) {
    let mut rules = app_modes();
    let Some(index) = ui::list_index(hwnd, ID_AM_LIST).filter(|&i| i < rules.len()) else {
        show_message(hwnd, "削除するアプリを、一覧で選んでください。", MB_ICONINFORMATION);
        return;
    };
    rules.remove(index);
    let select = (!rules.is_empty()).then(|| index.min(rules.len() - 1));
    set_app_modes(hwnd, rules, select);
    ui::focus(hwnd, ID_AM_LIST);
}

/// 「アプリごとの入力」で選んでいる入力モード。
unsafe fn selected_app_mode(hwnd: HWND) -> AppImeMode {
    combo_index(hwnd, ID_AM_MODE)
        .and_then(|i| AppImeMode::ALL.get(i))
        .map_or(AppImeMode::Japanese, |(m, _, _)| *m)
}

/// 「アプリごとの入力」の欄に入れたまま、まだ「追加」していないアプリがあれば、その説明を返す
/// （「追加」を押し忘れたまま保存して、効かないと思われないように）。
unsafe fn app_mode_not_added(hwnd: HWND) -> Option<String> {
    if !field_enabled(hwnd, ID_AM_APP) {
        return None;
    }
    let app = remap_logic::normalize_app(&get_text(hwnd, ID_AM_APP))?;
    let mode = selected_app_mode(hwnd);
    let listed = app_modes().iter().any(|r| r.app == app && r.mode == mode);
    (!listed).then(|| {
        format!("「{app}」はまだ一覧に入っていません。「追加」を押すか、欄を空にしてください。")
    })
}

/// 欄の中身の控え（保存していない変更があるかを調べるのに使う）。「アプリごとの入力」の一覧も含め、
/// 一覧に足す前の入力欄は含めない。
unsafe fn form_snapshot(hwnd: HWND) -> String {
    let items: Vec<Item> = all_items()
        .into_iter()
        .filter(|item| item.id != ID_AM_APP && item.id != ID_AM_MODE)
        .collect();
    let rules: Vec<String> = app_modes().iter().map(AppModeRule::label).collect();
    format!("{}\u{1f}{}", ui::snapshot(hwnd, &items), rules.join("\u{1e}"))
}

/// 入力モードの表示を使わないときは、表示のしかたの欄を使えなくする。
unsafe fn update_ime_fields(hwnd: HWND) {
    let on = is_checked(hwnd, ID_IME_ENABLED);
    let flash = on && is_checked(hwnd, ID_FLASH);
    for id in [ID_POSITION, ID_THEME, ID_HOLD, ID_FADE, ID_SIZE, ID_OPACITY, ID_PREVIEW] {
        ui::set_enabled(hwnd, id, flash);
    }
    for id in [ID_FLASH, ID_FULLSCREEN, ID_NO_CAPTURE, ID_BADGE_ENABLED] {
        ui::set_enabled(hwnd, id, on);
    }
    // 小さな印の出し方は、小さな印を出すときだけ使う。
    let on = on && is_checked(hwnd, ID_BADGE_ENABLED);
    for id in [ID_BADGE, ID_BADGE_SIZE, ID_BADGE_WHEN, ID_BADGE_OPACITY] {
        ui::set_enabled(hwnd, id, on);
    }
    // 出しておく時間は「出し続ける」以外で、マウスカーソルの形は「おまかせ」のときに使う。
    let when = combo_index(hwnd, ID_BADGE_WHEN)
        .and_then(|i| BadgeWhen::ALL.get(i))
        .map_or(BadgeWhen::Auto, |(w, _, _)| *w);
    ui::set_enabled(hwnd, ID_BADGE_HOLD, on && when != BadgeWhen::Always);
    ui::set_enabled(hwnd, ID_BADGE_IBEAM, on && when == BadgeWhen::Auto);
    ui::set_enabled(hwnd, ID_BADGE_KEEP, on && when == BadgeWhen::Auto);
}

/// 今の欄の中身を「保存済みの状態」として覚える。
unsafe fn mark_clean(hwnd: HWND) {
    let snapshot = form_snapshot(hwnd);
    CONTEXT.with(|c| {
        if let Some(context) = c.borrow_mut().as_mut() {
            context.baseline = snapshot;
        }
    });
}

/// 「キャンセル」・閉じるボタン・Esc: 保存していない変更があれば確かめてから閉じる。
unsafe fn on_close(hwnd: HWND) {
    let baseline = CONTEXT.with(|c| c.borrow().as_ref().map(|context| context.baseline.clone()));
    let changed = baseline.is_some_and(|baseline| baseline != form_snapshot(hwnd));
    if changed && !ui::ask_yes_no(hwnd, "保存していない変更があります。保存せずに閉じますか？") {
        return;
    }
    let _ = DestroyWindow(hwnd);
}

/// クリップボードの履歴の設定を画面に入れる。
unsafe fn fill_history(hwnd: HWND, settings: &ClipboardHistorySettings) {
    let config = clip_history::Config::from_settings(settings);
    set_checked(hwnd, ID_CH_ENABLED, config.enabled);
    let trigger_index = Trigger::ALL.iter().position(|(t, _, _)| *t == config.trigger).unwrap_or(0);
    set_combo_index(hwnd, ID_CH_TRIGGER, trigger_index);
    // 一覧を出す操作がキーの組み合わせでないときも、保存してあるキーをそのまま出す（出さないと、
    // 次に保存したときに既定のキーで上書きしてしまう）。読めないキーなら、既定のキーを出す。
    let hotkey = clip_history::parse_hotkey(&settings.hotkey)
        .ok()
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
    set_checked(hwnd, ID_CH_MOVE_TOP, config.move_to_top);
    set_checked(hwnd, ID_CH_IMAGES, config.record_images);
    set_checked(hwnd, ID_CH_KEEP_OPEN, config.keep_open);
    // 空白を含む名前は " で囲む（読むときに 1 つの名前として読めるように）。
    set_text(hwnd, ID_CH_EXCLUDE, &remap_logic::apps_to_line(&config.exclude_apps));
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
    let uses_key = trigger == Trigger::Hotkey;
    // キーの欄と間隔の欄は同じ行にあり、出す操作に合わせて片方だけを見せる
    // （このページを出しているときだけ）。
    let visible = ui::tab_index(hwnd, ID_TABS) == 3;
    for id in [ID_CH_KEY_LABEL, ID_CH_CTRL, ID_CH_SHIFT, ID_CH_ALT, ID_CH_WIN, ID_CH_PLUS, ID_CH_KEY] {
        ui::set_visible(hwnd, id, visible && uses_key);
        ui::set_enabled(hwnd, id, on);
    }
    for id in [ID_CH_INTERVAL_LABEL, ID_CH_INTERVAL, ID_CH_INTERVAL_UNIT] {
        ui::set_visible(hwnd, id, visible && !uses_key);
        ui::set_enabled(hwnd, id, on);
    }
    for id in [
        ID_CH_TRIGGER,
        ID_CH_MAX,
        ID_CH_POSITION,
        ID_CH_KEEP,
        ID_CH_WIDTH,
        ID_CH_OPACITY,
        ID_CH_PAGE,
        ID_CH_THEME,
        ID_CH_MOVE_TOP,
        ID_CH_IMAGES,
        ID_CH_KEEP_OPEN,
        ID_CH_EXCLUDE,
        ID_CH_PICK_EXCLUDE,
    ] {
        ui::set_enabled(hwnd, id, on);
    }
}

/// クリップボードの履歴の設定を読み、検証する。
unsafe fn read_history(hwnd: HWND) -> Result<ClipboardHistorySettings, (i32, String)> {
    let stored = CONTEXT.with(|c| {
        c.borrow()
            .as_ref()
            .map(|context| context.settings.clipboard_history.clone())
            .unwrap_or_default()
    });
    if !is_checked(hwnd, ID_CH_ENABLED) {
        // 使わないときは、欄は使えなくなっている。中身を検証せず、保存してある値を残す。
        return Ok(ClipboardHistorySettings {
            enabled: false,
            ..stored
        });
    }
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
    let interval = if trigger == Trigger::Hotkey {
        // 間隔の欄は出していない。保存してある値を残す。
        u64::from(stored.double_tap_ms)
    } else {
        ime_logic::parse_seconds(
            &get_text(hwnd, ID_CH_INTERVAL),
            u64::from(clip_history::DOUBLE_TAP_MS_MIN),
            u64::from(clip_history::DOUBLE_TAP_MS_MAX),
        )
        .ok_or_else(|| {
            (
                ID_CH_INTERVAL,
                "2 回押しの間隔は 0.2〜1 秒の数で入力してください（例: 0.4）。".to_string(),
            )
        })?
    };
    let max_items = parse_in_range(
        &get_text(hwnd, ID_CH_MAX),
        clip_history::MIN_ITEMS as u64,
        clip_history::MAX_ITEMS_LIMIT as u64,
    )
    .ok_or_else(|| {
        (
            ID_CH_MAX,
            format!(
                "残す件数は {}〜{} の数で入力してください。",
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
        move_to_top: is_checked(hwnd, ID_CH_MOVE_TOP),
        record_images: is_checked(hwnd, ID_CH_IMAGES),
        keep_open: is_checked(hwnd, ID_CH_KEEP_OPEN),
        exclude_apps: remap_logic::normalize_apps([get_text(hwnd, ID_CH_EXCLUDE).as_str()]),
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
    if let Some(message) = app_mode_not_added(hwnd) {
        return Err((ID_AM_APP, message));
    }

    Ok(Form {
        hotkey,
        target_apps,
        ime_enabled: is_checked(hwnd, ID_IME_ENABLED),
        ime,
        startup: is_checked(hwnd, ID_STARTUP),
        check_update: is_checked(hwnd, ID_UPDATE),
        update_every_hours: if is_checked(hwnd, ID_UPDATE_PERIODIC) {
            let saved = CONTEXT.with(|c| {
                c.borrow().as_ref().map_or(0, |context| context.settings.update.check_every_hours)
            });
            periodic_hours(saved)
        } else {
            0
        },
        update_mode: combo_index(hwnd, ID_UPDATE_MODE)
            .and_then(|i| UpdateMode::ALL.get(i))
            .map_or(UpdateMode::Ask, |(m, _, _)| *m),
        log: is_checked(hwnd, ID_LOG),
        history,
    })
}

/// 欄が使える（灰色になっていない）か。
unsafe fn field_enabled(hwnd: HWND, id: i32) -> bool {
    GetDlgItem(hwnd, id).is_ok_and(|control| IsWindowEnabled(control).as_bool())
}

/// 欄の値を読む。使えない（灰色の）欄は、誤りがあっても止めず、保存してある値 `saved` を使う
/// （使えない欄へは移れないので、直しようがない）。
unsafe fn read_field<T>(
    hwnd: HWND,
    id: i32,
    saved: T,
    read: impl FnOnce() -> Result<T, (i32, String)>,
) -> Result<T, (i32, String)> {
    if field_enabled(hwnd, id) {
        read()
    } else {
        Ok(read().unwrap_or(saved))
    }
}

/// 入力モード表示の出し方（位置・色・時間・大きさ・不透明度・全画面）を読む。
unsafe fn read_ime_params(hwnd: HWND) -> Result<Params, (i32, String)> {
    let saved = CONTEXT.with(|c| {
        c.borrow()
            .as_ref()
            .map(|context| context.settings.ime_indicator.clone())
            .unwrap_or_default()
    });
    let timing = read_timing(hwnd, &saved)?;
    let size = read_field(hwnd, ID_SIZE, saved.size, || read_size(hwnd))?;
    let opacity = read_field(hwnd, ID_OPACITY, saved.opacity, || {
        parse_in_range(
            &get_text(hwnd, ID_OPACITY),
            u64::from(OPACITY_MIN),
            u64::from(OPACITY_MAX),
        )
        .map(|v| v as u32)
        .ok_or_else(|| {
            (
                ID_OPACITY,
                format!("不透明度は {OPACITY_MIN}〜{OPACITY_MAX} の数で入力してください。"),
            )
        })
    })?;
    let position = combo_index(hwnd, ID_POSITION)
        .and_then(|i| Position::ALL.get(i))
        .map(|(p, _, _)| *p)
        .unwrap_or(Position::Center);
    let theme = combo_index(hwnd, ID_THEME)
        .and_then(|i| Theme::ALL.get(i))
        .map(|(t, _, _)| *t)
        .unwrap_or(Theme::Dark);
    let badge_size = read_field(hwnd, ID_BADGE_SIZE, saved.badge_size, || {
        parse_in_range(
            &get_text(hwnd, ID_BADGE_SIZE),
            u64::from(ime_logic::BADGE_SIZE_MIN),
            u64::from(ime_logic::BADGE_SIZE_MAX),
        )
        .map(|v| v as u32)
        .ok_or_else(|| {
            (
                ID_BADGE_SIZE,
                format!(
                    "小さな印の大きさは {}〜{} の数で入力してください。",
                    ime_logic::BADGE_SIZE_MIN,
                    ime_logic::BADGE_SIZE_MAX
                ),
            )
        })
    })?;
    let badge_hold_ms = read_field(hwnd, ID_BADGE_HOLD, saved.badge_hold_ms, || {
        ime_logic::parse_seconds(
            &get_text(hwnd, ID_BADGE_HOLD),
            ime_logic::BADGE_HOLD_MS_MIN,
            ime_logic::BADGE_HOLD_MS_MAX,
        )
        .ok_or_else(|| {
            (
                ID_BADGE_HOLD,
                format!(
                    "小さな印を出しておく時間は {}〜{} 秒で入力してください。",
                    ime_logic::format_seconds(ime_logic::BADGE_HOLD_MS_MIN),
                    ime_logic::format_seconds(ime_logic::BADGE_HOLD_MS_MAX)
                ),
            )
        })
    })?;
    let badge_opacity = read_field(hwnd, ID_BADGE_OPACITY, saved.badge_opacity, || {
        parse_in_range(
            &get_text(hwnd, ID_BADGE_OPACITY),
            u64::from(ime_logic::BADGE_OPACITY_MIN),
            u64::from(ime_logic::BADGE_OPACITY_MAX),
        )
        .map(|v| v as u32)
        .ok_or_else(|| {
            (
                ID_BADGE_OPACITY,
                format!(
                    "小さな印の不透明度は {}〜{} の数で入力してください。",
                    ime_logic::BADGE_OPACITY_MIN,
                    ime_logic::BADGE_OPACITY_MAX
                ),
            )
        })
    })?;
    Ok(Params {
        timing,
        size,
        position,
        theme,
        opacity,
        hide_in_fullscreen: is_checked(hwnd, ID_FULLSCREEN),
        hide_from_capture: is_checked(hwnd, ID_NO_CAPTURE),
        flash: is_checked(hwnd, ID_FLASH),
        badge_enabled: is_checked(hwnd, ID_BADGE_ENABLED),
        badge: combo_index(hwnd, ID_BADGE)
            .and_then(|i| BADGE_PLACES.get(i))
            .map_or(Badge::Caret, |(b, _)| *b),
        badge_size,
        badge_when: combo_index(hwnd, ID_BADGE_WHEN)
            .and_then(|i| BadgeWhen::ALL.get(i))
            .map_or(BadgeWhen::Auto, |(w, _, _)| *w),
        badge_on_ibeam: is_checked(hwnd, ID_BADGE_IBEAM),
        badge_keep_at_caret: is_checked(hwnd, ID_BADGE_KEEP),
        badge_hold_ms,
        badge_opacity,
        app_modes_enabled: is_checked(hwnd, ID_AM_ENABLED),
        app_modes: app_modes(),
    })
}

/// 表示時間とフェードアウトの時間（秒で入力）を読む。使えない欄は、保存してある値を使う。
unsafe fn read_timing(hwnd: HWND, saved: &ImeIndicatorSettings) -> Result<Timing, (i32, String)> {
    let hold_ms = read_field(hwnd, ID_HOLD, saved.hold_ms, || {
        ime_logic::parse_seconds(&get_text(hwnd, ID_HOLD), HOLD_MS_MIN, HOLD_MS_MAX).ok_or_else(
            || {
                (
                    ID_HOLD,
                    format!(
                        "表示時間は {}〜{} 秒の数で入力してください（例: 0.4、1.5）。",
                        ime_logic::format_seconds(HOLD_MS_MIN),
                        ime_logic::format_seconds(HOLD_MS_MAX)
                    ),
                )
            },
        )
    })?;
    let fade_ms = read_field(hwnd, ID_FADE, saved.fade_ms, || {
        ime_logic::parse_seconds(&get_text(hwnd, ID_FADE), FADE_MS_MIN, FADE_MS_MAX).ok_or_else(
            || {
                (
                    ID_FADE,
                    format!(
                        "フェードアウトは {}〜{} 秒の数で入力してください（例: 0.25）。",
                        ime_logic::format_seconds(FADE_MS_MIN),
                        ime_logic::format_seconds(FADE_MS_MAX)
                    ),
                )
            },
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
            if id == ID_BADGE_WHEN && code == CBN_SELCHANGE {
                update_ime_fields(hwnd);
            }
            if (id == ID_IME_ENABLED || id == ID_FLASH || id == ID_BADGE_ENABLED)
                && code == BN_CLICKED
            {
                update_ime_fields(hwnd);
            }
            if id == ID_AM_ENABLED && code == BN_CLICKED {
                update_app_mode_fields(hwnd);
            }
            if code == BN_CLICKED {
                match id {
                    ID_SAVE => on_save(hwnd, true),
                    ID_APPLY => on_save(hwnd, false),
                    ID_CANCEL => on_close(hwnd),
                    ID_PREVIEW => on_preview(hwnd),
                    ID_DEFAULTS => on_defaults(hwnd),
                    ID_OPEN_FOLDER => on_open_folder(hwnd),
                    ID_OPEN_LOG => on_open_log(hwnd),
                    ID_CH_CLEAR => on_clear_history(hwnd),
                    ID_EXPORT => on_export(hwnd),
                    ID_IMPORT => on_import(hwnd),
                    ID_PICK_APP => on_pick_app(hwnd, ID_APPS),
                    ID_CH_PICK_EXCLUDE => on_pick_app(hwnd, ID_CH_EXCLUDE),
                    ID_AM_PICK => on_pick_app(hwnd, ID_AM_APP),
                    ID_AM_ADD => on_add_app_mode(hwnd),
                    ID_AM_REMOVE => on_remove_app_mode(hwnd),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            let header = &*(lparam.0 as *const NMHDR);
            if header.idFrom == ID_TABS as usize && header.code == ui::TCN_SELCHANGE {
                show_page(hwnd, ui::tab_index(hwnd, ID_TABS));
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            on_close(hwnd);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // 別の DPI のモニターへ移った。勧められた大きさに合わせ、文字と配置を作り直す。
            let old = CONTEXT.with(|c| c.borrow().as_ref().map(|context| context.font));
            let font =
                ui::on_dpi_changed(hwnd, &all_items(), wparam, lparam, old.unwrap_or_default());
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

/// 「選ぶ...」: 開いているアプリの一覧からプロセス名を選び、`id` の欄に足す。
unsafe fn on_pick_app(hwnd: HWND, id: i32) {
    let Some(name) = process_picker::choose(hwnd) else {
        return;
    };
    if !IsWindow(hwnd).as_bool() {
        return;
    }
    if id == ID_AM_APP {
        // アプリごとの入力は 1 つずつ足すので、選んだアプリで置き換える。
        set_text(hwnd, id, &name);
        ui::focus(hwnd, ID_AM_MODE);
        return;
    }
    let apps = remap_logic::add_app(&get_text(hwnd, id), &name);
    let text = if id == ID_APPS {
        remap_logic::apps_to_text(&apps)
    } else {
        remap_logic::apps_to_line(&apps)
    };
    set_text(hwnd, id, &text);
    ui::focus(hwnd, id);
}

/// 「OK」「適用」: 検証して settings.json に書き、メインスレッドへ反映を頼む。`close` なら閉じる。
unsafe fn on_save(hwnd: HWND, close: bool) {
    let form = match read_form(hwnd) {
        Ok(form) => form,
        Err((id, message)) => {
            report_invalid_on_page(hwnd, id, &message);
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
    let saved_now = config::Settings::load();
    let hotkeys = saved_now.hotkeys;
    if let Some(message) =
        hotkey_rules::find_conflict(&hotkeys, (form.hotkey, &form.target_apps))
    {
        report_invalid_on_page(hwnd, ID_KEY, &message);
        return;
    }
    if let Some(message) = history_conflict(&form, &hotkeys) {
        report_invalid_on_page(hwnd, ID_CH_KEY, &message);
        return;
    }
    // 履歴を残さない設定に変えると、保存してある履歴（ピン留めを含む）が消える。確かめる。
    if settings.clipboard_history.keep_after_exit
        && form.history.enabled
        && !form.history.keep_after_exit
        && !ui::ask_yes_no(
            hwnd,
            "「アプリを終了しても履歴を残す」を OFF にすると、保存してある履歴\
             （ピン留めしたものを含む）は消えます。よろしいですか？",
        )
    {
        return;
    }
    if !IsWindow(hwnd).as_bool() {
        return;
    }
    settings.remap.hotkey = form.hotkey.format();
    settings.remap.target_apps = form.target_apps;
    // ON/OFF は、この画面を開いている間にトレイメニューで切り替えられていることがある。
    // この画面で変えていなければ、いま保存されている値を残す。
    settings.ime_indicator.enabled = if form.ime_enabled == settings.ime_indicator.enabled {
        saved_now.ime_indicator.enabled
    } else {
        form.ime_enabled
    };
    let ime = &form.ime;
    settings.ime_indicator.hold_ms = ime.timing.hold_ms;
    settings.ime_indicator.fade_ms = ime.timing.fade_ms;
    settings.ime_indicator.size = ime.size;
    settings.ime_indicator.position = ime.position.as_setting().to_string();
    settings.ime_indicator.theme = ime.theme.as_setting().to_string();
    settings.ime_indicator.opacity = ime.opacity;
    settings.ime_indicator.hide_in_fullscreen = ime.hide_in_fullscreen;
    settings.ime_indicator.hide_from_capture = ime.hide_from_capture;
    settings.ime_indicator.flash = ime.flash;
    settings.ime_indicator.badge_enabled = ime.badge_enabled;
    settings.ime_indicator.badge = ime.badge.as_setting().to_string();
    settings.ime_indicator.badge_size = ime.badge_size;
    settings.ime_indicator.badge_when = ime.badge_when.as_setting().to_string();
    settings.ime_indicator.badge_hold_ms = ime.badge_hold_ms;
    settings.ime_indicator.badge_opacity = ime.badge_opacity;
    settings.ime_indicator.badge_on_ibeam = ime.badge_on_ibeam;
    settings.ime_indicator.badge_keep_at_caret = ime.badge_keep_at_caret;
    settings.ime_indicator.app_modes_enabled = ime.app_modes_enabled;
    settings.ime_indicator.app_modes = ime
        .app_modes
        .iter()
        .map(|rule| AppImeModeSetting {
            app: rule.app.clone(),
            mode: rule.mode.as_setting().to_string(),
        })
        .collect();
    settings.log.enabled = form.log;
    settings.update.check_on_startup = form.check_update;
    settings.update.check_every_hours = form.update_every_hours;
    settings.update.mode = form.update_mode.as_setting().to_string();
    let history_enabled = if form.history.enabled == settings.clipboard_history.enabled {
        saved_now.clipboard_history.enabled
    } else {
        form.history.enabled
    };
    // 一覧の不透明度（Shift+ホイール）と「出したまま」（K キー・見出し）は、この画面を開いている
    // 間に一覧で変えられていることがある。この画面で変えていなければ、いま保存されている値を残す。
    let opened = clip_history::Config::from_settings(&settings.clipboard_history);
    let history_opacity = if form.history.opacity == opened.opacity {
        saved_now.clipboard_history.opacity
    } else {
        form.history.opacity
    };
    let history_keep_open = if form.history.keep_open == opened.keep_open {
        saved_now.clipboard_history.keep_open
    } else {
        form.history.keep_open
    };
    settings.clipboard_history = form.history;
    settings.clipboard_history.enabled = history_enabled;
    settings.clipboard_history.opacity = history_opacity;
    settings.clipboard_history.keep_open = history_keep_open;

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

    let _ = tx.send(TrayMessage::SettingsSaved(Box::new(settings.clone())));
    log::info!("設定を保存しました");
    // 画面と違う値を残した欄（トレイメニューや一覧で変えられていたもの）は、保存した値に合わせて
    // おく。合わせないと、もう一度「適用」したときに、画面で変えたものとして書き戻してしまう。
    set_checked(hwnd, ID_IME_ENABLED, settings.ime_indicator.enabled);
    update_ime_fields(hwnd);
    let history = clip_history::Config::from_settings(&settings.clipboard_history);
    set_checked(hwnd, ID_CH_ENABLED, history.enabled);
    set_text(hwnd, ID_CH_OPACITY, &history.opacity.to_string());
    set_checked(hwnd, ID_CH_KEEP_OPEN, history.keep_open);
    update_history_fields(hwnd);
    // 「適用」のあとも続けて編集できるよう、保存した内容を今の状態として覚える。
    CONTEXT.with(|c| {
        if let Some(context) = c.borrow_mut().as_mut() {
            context.settings = settings;
        }
    });
    mark_clean(hwnd);

    if let Some(e) = startup_error {
        show_message(
            hwnd,
            &format!("設定は保存しましたが、自動起動の設定を変えられませんでした。\n\n詳細: {e}"),
            MB_ICONWARNING,
        );
    }
    if close && IsWindow(hwnd).as_bool() {
        let _ = DestroyWindow(hwnd);
    }
}

/// 履歴の一覧を出すキーが、値貼り付けのキーやキー割り当てと重なっていれば、その説明を返す。
fn history_conflict(form: &Form, hotkeys: &[config::HotkeyRuleSetting]) -> Option<String> {
    history_conflict_with(form.hotkey, &form.history, hotkeys)
}

/// [`history_conflict`] の中身（値貼り付けのキー・履歴の設定・キー割り当てから調べる）。
fn history_conflict_with(
    remap: Hotkey,
    history: &ClipboardHistorySettings,
    hotkeys: &[config::HotkeyRuleSetting],
) -> Option<String> {
    let history = clip_history::Config::from_settings(history);
    if !history.enabled {
        return None;
    }
    let key = history.hotkey?;
    if key == remap {
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
    if ui::ask_yes_no(hwnd, "クリップボードの履歴をすべて削除しますか？\n\nピン留めしたものは残ります。削除した履歴は元に戻せません。") {
        actions::clear_history();
        show_message(hwnd, "クリップボードの履歴を削除しました。", MB_ICONINFORMATION);
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
            if m.len() > config::SETTINGS_FILE_MAX_BYTES {
                Err("ファイルが大きすぎます".to_string())
            } else {
                std::fs::read_to_string(&path).map_err(|e| e.to_string())
            }
        })
        .and_then(|text| config::parse_import(&text));
    // 書かれていないキー割り当て・定型文は今のものを残し、更新の確認先は今のままにする。
    let current = config::Settings::load();
    let read = read.and_then(|imported| config::merge_import(imported, &current));
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
            "今の設定を、読み込んだものに置き換えます。\n\n\
             キー割り当て: 今 {} 件 → {} 件\n\
             定型文: 今 {} 件 → {} 件\n\n\
             よろしいですか？",
            current.hotkeys.len(),
            imported.settings.hotkeys.len(),
            current.snippets.len(),
            imported.settings.snippets.len()
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
    let _ = tx.send(TrayMessage::SettingsImported(Box::new(imported.settings.clone())));
    let mut message = "設定を読み込み、使い始めました。".to_string();
    // 読み込んだキーどうしが重なっていれば知らせる（使えない割り当てがあることになるため）。
    let settings = &imported.settings;
    let (remap, _) = Hotkey::from_setting(&settings.remap.hotkey);
    let apps = remap_logic::effective_target_apps(&settings.remap.target_apps);
    let conflict = hotkey_rules::find_conflict(&settings.hotkeys, (remap, &apps))
        .or_else(|| history_conflict_with(remap, &settings.clipboard_history, &settings.hotkeys));
    if let Some(conflict) = conflict {
        message.push_str(&format!("\n\n注意: {conflict}"));
    }
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

/// 動いている間に確認する間隔（時間）。設定ファイルで決めた間隔があればそれを、
/// 無ければ（0 = 確認しない）既定の間隔を使う。
fn periodic_hours(saved: u64) -> u64 {
    if saved > 0 {
        saved
    } else {
        config::DEFAULT_CHECK_EVERY_HOURS
    }
}

/// 「すべて既定値に戻す」: 確かめてから、画面の値を既定値にする（保存するまでは反映しない）。
/// 自動起動は既定値を持たない（ショートカットの有無で決まる）ので、今の選択のままにする。
unsafe fn on_defaults(hwnd: HWND) {
    if !ui::ask_yes_no(
        hwnd,
        "すべてのページの設定を、既定値に戻しますか？\n\n\
         キー割り当て・定型文・履歴は変わりません。「OK」か「適用」を押すまでは保存しません。",
    ) || !IsWindow(hwnd).as_bool()
    {
        return;
    }
    let startup = is_checked(hwnd, ID_STARTUP);
    // 確認の間隔も既定に戻す（画面に出している「6 時間おき」と、保存する値を合わせる）。
    CONTEXT.with(|c| {
        if let Some(context) = c.borrow_mut().as_mut() {
            context.settings.update.check_every_hours = config::DEFAULT_CHECK_EVERY_HOURS;
        }
    });
    fill_form(hwnd, &Form::from_settings(&Settings::default(), startup));
}

/// 「ログを開く」: log.txt を既定のアプリ（メモ帳など）で開く。
unsafe fn on_open_log(hwnd: HWND) {
    let Some(path) = logging::log_file() else {
        show_message(hwnd, "ログの場所を決められませんでした。", MB_ICONERROR);
        return;
    };
    if !path.exists() {
        show_message(
            hwnd,
            "まだログはありません。\n\n「動作のログを保存する」を ON にして「適用」を押すと、ログを残し始めます。",
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

/// 「設定フォルダーを開く」: エクスプローラーで settings.json を選んだ状態で開く。
unsafe fn on_open_folder(hwnd: HWND) {
    let Some(path) = config::settings_file() else {
        show_message(hwnd, "設定フォルダーの場所を決められませんでした。", MB_ICONERROR);
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

