//! 設定画面。
//!
//! トレイメニューの「設定...」から開く。編集できるのは次の項目で、「保存」を押すと
//! `settings.json` に書き込み、メインスレッドへ [`TrayMessage::SettingsSaved`] を送って
//! その場で反映させる（再起動は要らない）。
//!
//! - キーリマップ: 値貼り付けを起動するキーの組み合わせ（Ctrl / Shift / Alt + キー）と、
//!   対象アプリ（プロセス名）
//! - 入力モードの画面中央表示: ON/OFF、表示時間、大きさ（プレビュー付き）
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
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::thread::JoinHandle;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, GetMonitorInfoW, MonitorFromPoint, COLOR_BTNFACE, HBRUSH,
    HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    CheckDlgButton, InitCommonControlsEx, IsDlgButtonChecked, BST_CHECKED, BST_UNCHECKED,
    ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetDlgItem,
    GetMessageW, GetWindowTextLengthW, GetWindowTextW, IsDialogMessageW, LoadIconW, MessageBoxW,
    PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW, SetForegroundWindow,
    SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage, BS_AUTOCHECKBOX,
    BS_DEFPUSHBUTTON, BS_GROUPBOX, BS_PUSHBUTTON, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL,
    CB_SETCURSEL, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_NUMBER, ES_WANTRETURN, HMENU,
    MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MESSAGEBOX_STYLE, MSG,
    NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SWP_NOACTIVATE, SWP_NOZORDER, SW_RESTORE, SW_SHOW,
    SW_SHOWNORMAL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_DPICHANGED,
    WM_SETFONT, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_MINIMIZEBOX, WS_OVERLAPPED,
    WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::config::{self, Settings};
use crate::ime_logic::{self, HOLD_MS_MAX, HOLD_MS_MIN, SIZE_MAX, SIZE_MIN};
use crate::remap_logic::{self, Hotkey, KEYS};
use crate::{ime_indicator, startup, TrayMessage};

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
const ID_PREVIEW: i32 = 113;
const ID_STARTUP: i32 = 120;
const ID_UPDATE: i32 = 121;
const ID_OPEN_FOLDER: i32 = 130;
const ID_DEFAULTS: i32 = 131;

/// `SS_CENTERIMAGE`。1 行の文字を縦方向の中央にそろえる（隣の入力欄と高さを合わせる）。
const SS_CENTERIMAGE: u32 = 0x0200;
/// `BN_CLICKED`（ボタンが押された通知コード）。
const BN_CLICKED: u32 = 0;

/// 画面の中身の大きさ（96 DPI 基準）。
const CLIENT_W: i32 = 480;
const CLIENT_H: i32 = 425;

/// コントロールの種類。
#[derive(Clone, Copy)]
enum Kind {
    Group,
    Label,
    Check,
    Combo,
    NumberEdit,
    MultiEdit,
    Button,
    DefaultButton,
}

/// 1 つのコントロールの定義。位置と大きさは 96 DPI 基準で、表示時に DPI に合わせて拡大する。
struct Item {
    id: i32,
    kind: Kind,
    text: &'static str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

const fn item(id: i32, kind: Kind, text: &'static str, x: i32, y: i32, w: i32, h: i32) -> Item {
    Item { id, kind, text, x, y, w, h }
}

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
    // 入力モードの画面中央表示
    item(210, Kind::Group, "入力モードの画面中央表示", 12, 163, 456, 120),
    item(ID_IME_ENABLED, Kind::Check, "IME の入力モードを切り替えたら画面中央に表示する", 24, 185, 428, 22),
    item(211, Kind::Label, "表示時間", 24, 214, 112, 24),
    item(ID_HOLD, Kind::NumberEdit, "", 140, 214, 70, 24),
    item(212, Kind::Label, "ミリ秒（100〜5000）", 216, 214, 160, 24),
    item(213, Kind::Label, "大きさ", 24, 246, 112, 24),
    item(ID_SIZE, Kind::NumberEdit, "", 140, 246, 70, 24),
    item(214, Kind::Label, "px（40〜600）", 216, 246, 130, 24),
    item(ID_PREVIEW, Kind::Button, "プレビュー", 362, 244, 90, 28),
    // 起動と更新
    item(220, Kind::Group, "起動と更新", 12, 291, 456, 80),
    item(ID_STARTUP, Kind::Check, "Windows にサインインしたら自動で起動する", 24, 313, 428, 22),
    item(ID_UPDATE, Kind::Check, "起動時に新しいバージョンを確認する", 24, 340, 428, 22),
    // 操作ボタン
    item(ID_OPEN_FOLDER, Kind::Button, "設定ファイルの場所を開く", 12, 385, 178, 28),
    item(ID_DEFAULTS, Kind::Button, "既定に戻す", 198, 385, 86, 28),
    item(ID_SAVE, Kind::DefaultButton, "保存", 290, 385, 86, 28),
    item(ID_CANCEL, Kind::Button, "キャンセル", 382, 385, 86, 28),
];

/// 画面に表示する値。
struct Form {
    hotkey: Hotkey,
    target_apps: Vec<String>,
    ime_enabled: bool,
    hold_ms: u64,
    size: u32,
    startup: bool,
    check_update: bool,
}

impl Form {
    fn from_settings(settings: &Settings, startup: bool) -> Form {
        let (hotkey, _) = Hotkey::from_setting(&settings.remap.hotkey);
        Form {
            hotkey,
            target_apps: remap_logic::effective_target_apps(&settings.remap.target_apps),
            ime_enabled: settings.ime_indicator.enabled,
            hold_ms: settings.ime_indicator.hold_ms,
            size: settings.ime_indicator.size,
            startup,
            check_update: settings.update.check_on_startup,
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

/// 開いている画面（0 は無し）。
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// 画面のスレッドが動いているか（作成中を含む。二重に開かないため）。
static RUNNING: AtomicBool = AtomicBool::new(false);
/// 画面のスレッド。終了時に待つ。
static THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

/// 設定画面を開く。すでに開いていれば手前に出す。
///
/// `settings` は現在の設定（トレイで切り替えた入力モード表示の ON/OFF も反映済みのもの）、
/// `startup` は現在の自動起動の状態。
pub fn open(tx: Sender<TrayMessage>, settings: Settings, startup: bool) {
    if RUNNING.load(Ordering::SeqCst) {
        let window = WINDOW.load(Ordering::SeqCst);
        if window != 0 {
            unsafe {
                let hwnd = HWND(window as *mut core::ffi::c_void);
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
        }
        return;
    }

    let mut slot = THREAD.lock().unwrap_or_else(|p| p.into_inner());
    // 前回開いたときのスレッドは終わっているので、片付けてから新しく作る。
    if let Some(previous) = slot.take() {
        let _ = previous.join();
    }
    RUNNING.store(true, Ordering::SeqCst);
    *slot = Some(std::thread::spawn(move || {
        unsafe { thread_main(tx, settings, startup) };
        WINDOW.store(0, Ordering::SeqCst);
        RUNNING.store(false, Ordering::SeqCst);
    }));
}

/// 設定画面が開いていれば閉じ（保存はしない）、スレッドが終わるのを待つ。アプリの終了時に呼ぶ。
pub fn close() {
    let window = WINDOW.load(Ordering::SeqCst);
    if window != 0 {
        unsafe {
            let _ = PostMessageW(
                HWND(window as *mut core::ffi::c_void),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
    let handle = THREAD.lock().unwrap_or_else(|p| p.into_inner()).take();
    if let Some(handle) = handle {
        let _ = handle.join();
    }
}

unsafe fn thread_main(tx: Sender<TrayMessage>, settings: Settings, startup: bool) {
    // 標準コントロールを Windows のテーマ（Common Controls v6）で描くために読み込む。
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_STANDARD_CLASSES,
    };
    let _ = InitCommonControlsEx(&icc);

    let Some(hwnd) = create_window() else {
        log::error!("設定画面を作成できませんでした");
        show_message(HWND::default(), "設定画面を開けませんでした。", MB_ICONERROR);
        return;
    };
    let dpi = GetDpiForWindow(hwnd);
    let font = create_ui_font(dpi);
    let form = Form::from_settings(&settings, startup);
    CONTEXT.with(|c| *c.borrow_mut() = Some(Context { tx, settings, font }));

    create_controls(hwnd, font);
    fill_form(hwnd, &form);
    place_window(hwnd, dpi);
    layout(hwnd, dpi);
    WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);

    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    if let Ok(combo) = GetDlgItem(hwnd, ID_KEY) {
        let _ = SetFocus(combo);
    }

    let mut msg = MSG::default();
    loop {
        let ret = GetMessageW(&mut msg, None, 0, 0);
        if ret.0 <= 0 {
            break;
        }
        // Tab での移動、Enter で保存、Esc でキャンセルを、ダイアログと同じように扱う。
        if IsDialogMessageW(hwnd, &msg).as_bool() {
            continue;
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    CONTEXT.with(|c| {
        if let Some(context) = c.borrow_mut().take() {
            let _ = DeleteObject(context.font);
        }
    });
}

/// 画面のウィンドウを作る（まだ表示しない。大きさと位置は DPI が分かってから決める）。
unsafe fn create_window() -> Option<HWND> {
    let instance = GetModuleHandleW(None).ok()?;
    let class_name = w!("AtaiPasteSettings");
    let class = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance.into(),
        lpszClassName: class_name,
        // 実行ファイルに埋め込んだアプリのアイコン（app.rc の "app"）。
        hIcon: LoadIconW(instance, w!("app")).unwrap_or_default(),
        hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as isize as *mut core::ffi::c_void),
        ..Default::default()
    };
    // 2 回目以降に開いたときは登録済みで失敗するが、そのまま使えるので結果は見ない。
    RegisterClassW(&class);

    let title = wide(&format!("設定 - アタイの貼り付け v{}", env!("CARGO_PKG_VERSION")));
    // 置き場所は表示したいモニターの左上にしておく（そのモニターの DPI を得るため）。
    let work = cursor_work_area();
    CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class_name,
        PCWSTR(title.as_ptr()),
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
        work.left,
        work.top,
        100,
        100,
        None,
        None,
        instance,
        None,
    )
    .ok()
}

/// マウスカーソルのあるモニター（トレイメニューを開いたモニター）の作業領域。
unsafe fn cursor_work_area() -> RECT {
    let mut point = POINT::default();
    let _ = GetCursorPos(&mut point);
    let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(monitor, &mut info).as_bool() {
        info.rcWork
    } else {
        RECT {
            left: 0,
            top: 0,
            right: 1024,
            bottom: 768,
        }
    }
}

/// 画面の大きさを DPI に合わせて決め、作業領域の中央に置く。
unsafe fn place_window(hwnd: HWND, dpi: u32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scale(CLIENT_W, dpi),
        bottom: scale(CLIENT_H, dpi),
    };
    let _ = AdjustWindowRectExForDpi(
        &mut rect,
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
        false,
        WINDOW_EX_STYLE::default(),
        dpi,
    );
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let work = cursor_work_area();
    let x = work.left + ((work.right - work.left - width) / 2).max(0);
    let y = work.top + ((work.bottom - work.top - height) / 2).max(0);
    let _ = SetWindowPos(hwnd, None, x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE);
}

/// 96 DPI 基準の長さを実際の DPI に合わせる。
fn scale(value: i32, dpi: u32) -> i32 {
    ime_logic::scale_for_dpi(value, dpi)
}

/// 画面の文字に使うフォント（Windows の「メッセージ」のフォントを DPI に合わせて作る）。
unsafe fn create_ui_font(dpi: u32) -> HFONT {
    let mut metrics = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    let ok = SystemParametersInfoForDpi(
        SPI_GETNONCLIENTMETRICS.0,
        metrics.cbSize,
        Some(&mut metrics as *mut NONCLIENTMETRICSW as *mut core::ffi::c_void),
        0,
        dpi,
    )
    .is_ok();
    if ok {
        CreateFontIndirectW(&metrics.lfMessageFont)
    } else {
        HFONT::default()
    }
}

/// コントロールを作る（位置は [`layout`] で決める）。
unsafe fn create_controls(hwnd: HWND, font: HFONT) {
    let instance = GetModuleHandleW(None).unwrap_or_default();
    for item in ITEMS {
        let (class, style, ex_style) = match item.kind {
            Kind::Group => (w!("BUTTON"), BS_GROUPBOX as u32, 0),
            Kind::Label => (w!("STATIC"), SS_CENTERIMAGE, 0),
            Kind::Check => (
                w!("BUTTON"),
                BS_AUTOCHECKBOX as u32 | WS_TABSTOP.0,
                0,
            ),
            Kind::Combo => (
                w!("COMBOBOX"),
                CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0 | WS_TABSTOP.0,
                0,
            ),
            Kind::NumberEdit => (
                w!("EDIT"),
                ES_AUTOHSCROLL as u32 | ES_NUMBER as u32 | WS_TABSTOP.0,
                WS_EX_CLIENTEDGE.0,
            ),
            Kind::MultiEdit => (
                w!("EDIT"),
                ES_MULTILINE as u32
                    | ES_AUTOVSCROLL as u32
                    | ES_WANTRETURN as u32
                    | WS_VSCROLL.0
                    | WS_TABSTOP.0,
                WS_EX_CLIENTEDGE.0,
            ),
            Kind::Button => (w!("BUTTON"), BS_PUSHBUTTON as u32 | WS_TABSTOP.0, 0),
            Kind::DefaultButton => (w!("BUTTON"), BS_DEFPUSHBUTTON as u32 | WS_TABSTOP.0, 0),
        };
        let text = wide(item.text);
        let control = CreateWindowExW(
            WINDOW_EX_STYLE(ex_style),
            class,
            PCWSTR(text.as_ptr()),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style),
            0,
            0,
            0,
            0,
            hwnd,
            HMENU(item.id as isize as *mut core::ffi::c_void),
            instance,
            None,
        );
        if let Ok(control) = control {
            if !font.is_invalid() {
                SendMessageW(control, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            }
        }
    }

    // キーの一覧を入れる。
    if let Ok(combo) = GetDlgItem(hwnd, ID_KEY) {
        for (name, _) in KEYS {
            let text = wide(name);
            SendMessageW(combo, CB_ADDSTRING, WPARAM(0), LPARAM(text.as_ptr() as isize));
        }
    }
}

/// コントロールを DPI に合わせた位置と大きさに置く。
unsafe fn layout(hwnd: HWND, dpi: u32) {
    for item in ITEMS {
        if let Ok(control) = GetDlgItem(hwnd, item.id) {
            let _ = SetWindowPos(
                control,
                None,
                scale(item.x, dpi),
                scale(item.y, dpi),
                scale(item.w, dpi),
                scale(item.h, dpi),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

/// フォントを差し替える（DPI が変わったとき）。
unsafe fn apply_font(hwnd: HWND, font: HFONT) {
    for item in ITEMS {
        if let Ok(control) = GetDlgItem(hwnd, item.id) {
            SendMessageW(control, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
    }
}

/// 値を画面に入れる。
unsafe fn fill_form(hwnd: HWND, form: &Form) {
    set_checked(hwnd, ID_CTRL, form.hotkey.ctrl);
    set_checked(hwnd, ID_SHIFT, form.hotkey.shift);
    set_checked(hwnd, ID_ALT, form.hotkey.alt);
    if let Ok(combo) = GetDlgItem(hwnd, ID_KEY) {
        let index = KEYS.iter().position(|(_, vk)| *vk == form.hotkey.vk).unwrap_or(1);
        SendMessageW(combo, CB_SETCURSEL, WPARAM(index), LPARAM(0));
    }
    set_text(hwnd, ID_APPS, &remap_logic::apps_to_text(&form.target_apps));
    set_checked(hwnd, ID_IME_ENABLED, form.ime_enabled);
    set_text(hwnd, ID_HOLD, &form.hold_ms.to_string());
    set_text(hwnd, ID_SIZE, &form.size.to_string());
    set_checked(hwnd, ID_STARTUP, form.startup);
    set_checked(hwnd, ID_UPDATE, form.check_update);
}

/// 画面の値を読み、検証する。誤りがあれば、直すべきコントロールの ID と理由を返す。
unsafe fn read_form(hwnd: HWND) -> Result<Form, (i32, String)> {
    let index = GetDlgItem(hwnd, ID_KEY)
        .map(|combo| SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0)
        .unwrap_or(-1);
    let vk = usize::try_from(index)
        .ok()
        .and_then(|i| KEYS.get(i))
        .map(|(_, vk)| *vk)
        .unwrap_or(0);
    let hotkey = Hotkey {
        ctrl: is_checked(hwnd, ID_CTRL),
        shift: is_checked(hwnd, ID_SHIFT),
        alt: is_checked(hwnd, ID_ALT),
        vk,
    };
    hotkey.validate().map_err(|e| (ID_KEY, e))?;

    let target_apps = remap_logic::normalize_apps([get_text(hwnd, ID_APPS).as_str()]);
    if target_apps.is_empty() {
        return Err((ID_APPS, "対象アプリを 1 つ以上入力してください（例: EXCEL.EXE）。".into()));
    }

    let hold_ms = parse_in_range(&get_text(hwnd, ID_HOLD), HOLD_MS_MIN, HOLD_MS_MAX)
        .ok_or_else(|| {
            (
                ID_HOLD,
                format!("表示時間は {HOLD_MS_MIN}〜{HOLD_MS_MAX} の数で入力してください。"),
            )
        })?;
    let size = parse_in_range(&get_text(hwnd, ID_SIZE), u64::from(SIZE_MIN), u64::from(SIZE_MAX))
        .ok_or_else(|| {
            (
                ID_SIZE,
                format!("大きさは {SIZE_MIN}〜{SIZE_MAX} の数で入力してください。"),
            )
        })? as u32;

    Ok(Form {
        hotkey,
        target_apps,
        ime_enabled: is_checked(hwnd, ID_IME_ENABLED),
        hold_ms,
        size,
        startup: is_checked(hwnd, ID_STARTUP),
        check_update: is_checked(hwnd, ID_UPDATE),
    })
}

/// 数を読み、範囲内なら返す。
fn parse_in_range(text: &str, min: u64, max: u64) -> Option<u64> {
    text.trim().parse::<u64>().ok().filter(|v| (min..=max).contains(v))
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
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // 別の DPI のモニターへ移った。勧められた大きさに合わせ、文字と配置を作り直す。
            let dpi = (wparam.0 & 0xFFFF) as u32;
            let suggested = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let font = create_ui_font(dpi);
            let old = CONTEXT.with(|c| {
                c.borrow_mut()
                    .as_mut()
                    .map(|context| std::mem::replace(&mut context.font, font))
            });
            apply_font(hwnd, font);
            layout(hwnd, dpi);
            if let Some(old) = old {
                let _ = DeleteObject(old);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            WINDOW.store(0, Ordering::SeqCst);
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
            show_message(hwnd, &message, MB_ICONWARNING);
            if let Ok(control) = GetDlgItem(hwnd, id) {
                let _ = SetFocus(control);
            }
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
    settings.remap.hotkey = form.hotkey.format();
    settings.remap.target_apps = form.target_apps;
    settings.ime_indicator.enabled = form.ime_enabled;
    settings.ime_indicator.hold_ms = form.hold_ms;
    settings.ime_indicator.size = form.size;
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

/// 「プレビュー」: 入力中の表示時間と大きさで、入力モードを試しに表示する。
unsafe fn on_preview(hwnd: HWND) {
    let hold_ms = parse_in_range(&get_text(hwnd, ID_HOLD), HOLD_MS_MIN, HOLD_MS_MAX);
    let size = parse_in_range(&get_text(hwnd, ID_SIZE), u64::from(SIZE_MIN), u64::from(SIZE_MAX));
    let (Some(hold_ms), Some(size)) = (hold_ms, size) else {
        show_message(
            hwnd,
            &format!(
                "表示時間は {HOLD_MS_MIN}〜{HOLD_MS_MAX}、大きさは {SIZE_MIN}〜{SIZE_MAX} の数で入力してください。"
            ),
            MB_ICONWARNING,
        );
        return;
    };
    if !ime_indicator::preview(hold_ms, size as u32) {
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

unsafe fn set_checked(hwnd: HWND, id: i32, checked: bool) {
    let state = if checked { BST_CHECKED } else { BST_UNCHECKED };
    let _ = CheckDlgButton(hwnd, id, state);
}

unsafe fn is_checked(hwnd: HWND, id: i32) -> bool {
    IsDlgButtonChecked(hwnd, id) == BST_CHECKED.0
}

unsafe fn set_text(hwnd: HWND, id: i32, text: &str) {
    if let Ok(control) = GetDlgItem(hwnd, id) {
        let text = wide(text);
        let _ = SetWindowTextW(control, PCWSTR(text.as_ptr()));
    }
}

unsafe fn get_text(hwnd: HWND, id: i32) -> String {
    let Ok(control) = GetDlgItem(hwnd, id) else {
        return String::new();
    };
    let len = GetWindowTextLengthW(control).max(0) as usize;
    let mut buf = vec![0u16; len + 1];
    let copied = GetWindowTextW(control, &mut buf).max(0) as usize;
    String::from_utf16_lossy(&buf[..copied.min(len)])
}

/// 画面を持ち主にしてメッセージを出す（画面の後ろに隠れないように）。
unsafe fn show_message(owner: HWND, text: &str, icon: MESSAGEBOX_STYLE) {
    let text = wide(text);
    MessageBoxW(owner, PCWSTR(text.as_ptr()), w!("アタイの貼り付け"), MB_OK | icon);
}

/// 文字列を UTF-16 のヌル終端バッファへ変換する。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
