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

use crate::config::{self, Settings};
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
};
use crate::{ime_indicator, logging, startup, TrayMessage};

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


/// 画面の中身の大きさ（96 DPI 基準）。
const CLIENT_W: i32 = 480;
const CLIENT_H: i32 = 569;

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
    // 操作ボタン
    item(ID_OPEN_FOLDER, Kind::Button, "設定ファイルの場所を開く", 12, 529, 178, 28),
    item(ID_DEFAULTS, Kind::Button, "既定に戻す", 198, 529, 86, 28),
    item(ID_SAVE, Kind::DefaultButton, "保存", 290, 529, 86, 28),
    item(ID_CANCEL, Kind::Button, "キャンセル", 382, 529, 86, 28),
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

    Ok(Form {
        hotkey,
        target_apps,
        ime_enabled: is_checked(hwnd, ID_IME_ENABLED),
        ime,
        startup: is_checked(hwnd, ID_STARTUP),
        check_update: is_checked(hwnd, ID_UPDATE),
        log: is_checked(hwnd, ID_LOG),
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

