//! 画面（設定画面・キー割り当て画面）で共通に使う部品。
//!
//! 外部のクレートを足さず、Win32 の標準コントロール（ボタン・エディット・コンボボックス・
//! リストボックス）だけで画面を組み立てるための小さな道具をまとめている。
//!
//! - コントロールの定義（[`Item`]）を表にしておき、[`create_controls`] で作って
//!   [`layout`] で DPI に合わせて並べる。モニター間を移動して DPI が変わったときは
//!   [`on_dpi_changed`] で大きさ・文字・配置を作り直す。
//! - 画面は専用スレッドで動かし、同じ画面を二重に開かないよう [`SingleWindow`] で管理する。
//!   アプリの終了時は [`SingleWindow::close`] で閉じて、スレッドの終わりを待つ。

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;
use std::thread::JoinHandle;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, GetMonitorInfoW, MonitorFromPoint, COLOR_BTNFACE, HBRUSH,
    HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    CheckDlgButton, InitCommonControlsEx, IsDlgButtonChecked, BST_CHECKED, BST_UNCHECKED,
    ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, SystemParametersInfoForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DispatchMessageW, GetCursorPos, GetDlgItem, GetMessageW,
    GetWindowTextLengthW, GetWindowTextW, IsDialogMessageW, LoadIconW, MessageBoxW,
    PostMessageW, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowPos,
    SetWindowTextW, ShowWindow, TranslateMessage, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_GROUPBOX,
    BS_PUSHBUTTON, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL,
    ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_NUMBER, ES_WANTRETURN, HMENU, LBS_NOTIFY,
    MB_OK, MESSAGEBOX_STYLE, MSG, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_RESTORE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_SETFONT, WNDCLASSW,
    WNDPROC, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::ime_logic;

/// `SS_CENTERIMAGE`。1 行の文字を縦方向の中央にそろえる（隣の入力欄と高さを合わせる）。
const SS_CENTERIMAGE: u32 = 0x0200;
/// `BN_CLICKED`（ボタンが押された通知コード）。
pub const BN_CLICKED: u32 = 0;
/// `LBN_SELCHANGE`（リストボックスの選択が変わった通知コード）。
pub const LBN_SELCHANGE: u32 = 1;
/// `CBN_SELCHANGE`（コンボボックスの選択が変わった通知コード）。一覧を開いて矢印キーで
/// 動かしている間にも届く。
pub const CBN_SELCHANGE: u32 = 1;
/// `CBN_CLOSEUP`（コンボボックスの一覧が閉じた通知コード）。一覧の外を押して閉じると選択が
/// 元に戻るので、このときにも選択を読み直す。
pub const CBN_CLOSEUP: u32 = 8;

/// リストボックスの中身を入れ直し、`select` の行を選ぶ。
pub unsafe fn set_list_items(hwnd: HWND, id: i32, items: &[String], select: Option<usize>) {
    use windows::Win32::UI::WindowsAndMessaging::{LB_ADDSTRING, LB_RESETCONTENT, LB_SETCURSEL};
    if let Ok(list) = GetDlgItem(hwnd, id) {
        SendMessageW(list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
        for item in items {
            let text = wide(item);
            SendMessageW(list, LB_ADDSTRING, WPARAM(0), LPARAM(text.as_ptr() as isize));
        }
        let index = select.map(|i| i as isize).unwrap_or(-1);
        SendMessageW(list, LB_SETCURSEL, WPARAM(index as usize), LPARAM(0));
    }
}

/// リストボックスで選ばれている行。選ばれていなければ `None`。
pub unsafe fn list_index(hwnd: HWND, id: i32) -> Option<usize> {
    use windows::Win32::UI::WindowsAndMessaging::LB_GETCURSEL;
    let index = GetDlgItem(hwnd, id)
        .map(|list| SendMessageW(list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0)
        .unwrap_or(-1);
    usize::try_from(index).ok()
}

/// はい・いいえで尋ねる。「はい」なら `true`。
pub unsafe fn ask_yes_no(owner: HWND, text: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{IDYES, MB_ICONQUESTION, MB_YESNO};
    let text = wide(text);
    MessageBoxW(
        owner,
        PCWSTR(text.as_ptr()),
        w!("アタイの貼り付け"),
        MB_YESNO | MB_ICONQUESTION,
    ) == IDYES
}

/// アプリの終了時に、画面を「保存せず・確かめずに」閉じさせるメッセージ。
/// 各画面のウィンドウプロシージャで `DestroyWindow` する。
pub const WM_APP_FORCE_CLOSE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 50;

/// 画面の枠の形（タイトルバー・閉じるボタン・最小化ボタン。大きさは変えられない）。
const WINDOW_STYLE_FRAME: WINDOW_STYLE =
    WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0);

/// コントロールの種類。
#[derive(Clone, Copy)]
pub enum Kind {
    Group,
    Label,
    /// 折り返して何行かで表示する説明文。
    Note,
    Check,
    Combo,
    Edit,
    NumberEdit,
    MultiEdit,
    ListBox,
    /// 行をアプリが描く一覧（中身の文字は持たず、行数だけを持つ）。
    OwnerList,
    Button,
    DefaultButton,
}

/// 1 つのコントロールの定義。位置と大きさは 96 DPI 基準で、表示時に DPI に合わせて拡大する。
pub struct Item {
    pub id: i32,
    pub kind: Kind,
    pub text: &'static str,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

pub const fn item(id: i32, kind: Kind, text: &'static str, x: i32, y: i32, w: i32, h: i32) -> Item {
    Item { id, kind, text, x, y, w, h }
}

/// 同時に 1 つしか開かない画面の管理（開いている画面と、そのスレッド）。
pub struct SingleWindow {
    window: AtomicIsize,
    running: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl SingleWindow {
    pub const fn new() -> SingleWindow {
        SingleWindow {
            window: AtomicIsize::new(0),
            running: AtomicBool::new(false),
            thread: Mutex::new(None),
        }
    }

    /// 画面のスレッドを始める。すでに開いていれば手前に出すだけ。
    /// `run` は画面を作ってメッセージループを回し、閉じたら戻る関数。
    pub fn open(&'static self, run: impl FnOnce() + Send + 'static) {
        if self.running.load(Ordering::SeqCst) {
            let window = self.window.load(Ordering::SeqCst);
            if window != 0 {
                unsafe {
                    let hwnd = HWND(window as *mut core::ffi::c_void);
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                    let _ = SetForegroundWindow(hwnd);
                }
            }
            return;
        }
        let mut slot = self.thread.lock().unwrap_or_else(|p| p.into_inner());
        // 前回開いたときのスレッドは終わっているので、片付けてから新しく作る。
        if let Some(previous) = slot.take() {
            let _ = previous.join();
        }
        self.running.store(true, Ordering::SeqCst);
        *slot = Some(std::thread::spawn(move || {
            run();
            self.window.store(0, Ordering::SeqCst);
            self.running.store(false, Ordering::SeqCst);
        }));
    }

    /// 画面が開いている（開いている途中・閉じている途中を含む）か。
    pub fn is_open(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// 作った画面を知らせる（手前に出す・閉じるときの宛先）。閉じたら `HWND::default()`。
    pub fn set_window(&self, hwnd: HWND) {
        self.window.store(hwnd.0 as isize, Ordering::SeqCst);
    }

    /// 開いていれば閉じ（保存はしない。変更の有無も確かめない）、スレッドが終わるのを待つ。
    /// アプリの終了時に呼ぶ。
    ///
    /// 画面ができる前（作っている途中や、作れずにメッセージを出している間）だと閉じる宛先が
    /// 無いので、少し待ちながら送り直す。それでも終わらなければ、待たずに戻る
    /// （アプリの終了でスレッドも終わる）。
    pub fn close(&self) {
        let handle = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take();
        let Some(handle) = handle else {
            return;
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut posted_to = 0isize;
        while !handle.is_finished() && std::time::Instant::now() < deadline {
            let window = self.window.load(Ordering::SeqCst);
            if window != 0 && window != posted_to {
                unsafe {
                    let _ = PostMessageW(
                        HWND(window as *mut core::ffi::c_void),
                        WM_APP_FORCE_CLOSE,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
                posted_to = window;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if handle.is_finished() {
            let _ = handle.join();
        } else {
            log::warn!("画面のスレッドが終わらないため、待たずに終了します");
        }
    }
}

/// 標準コントロールを Windows のテーマ（Common Controls v6）で描くために読み込む。
pub unsafe fn init_common_controls() {
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_STANDARD_CLASSES,
    };
    let _ = InitCommonControlsEx(&icc);
}

/// 画面のウィンドウを作る（まだ表示しない。大きさと位置は DPI が分かってから
/// [`place_window`] で決める）。
pub unsafe fn create_top_window(class_name: PCWSTR, wnd_proc: WNDPROC, title: &str) -> Option<HWND> {
    create_owned_window(class_name, wnd_proc, title, HWND::default())
}

/// [`create_top_window`] の、持ち主（`owner`）のある版。持ち主の手前に表示され、
/// 持ち主を閉じると一緒に閉じる。
pub unsafe fn create_owned_window(
    class_name: PCWSTR,
    wnd_proc: WNDPROC,
    title: &str,
    owner: HWND,
) -> Option<HWND> {
    let instance = GetModuleHandleW(None).ok()?;
    let class = WNDCLASSW {
        lpfnWndProc: wnd_proc,
        hInstance: instance.into(),
        lpszClassName: class_name,
        // 実行ファイルに埋め込んだアプリのアイコン（app.rc の "app"）。
        hIcon: LoadIconW(instance, w!("app")).unwrap_or_default(),
        hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as isize as *mut core::ffi::c_void),
        ..Default::default()
    };
    // 2 回目以降に開いたときは登録済みで失敗するが、そのまま使えるので結果は見ない。
    RegisterClassW(&class);

    let title = wide(title);
    // 置き場所は表示したいモニターの左上にしておく（そのモニターの DPI を得るため）。
    let work = cursor_work_area();
    CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class_name,
        PCWSTR(title.as_ptr()),
        WINDOW_STYLE_FRAME,
        work.left,
        work.top,
        100,
        100,
        owner,
        None,
        instance,
        None,
    )
    .ok()
}

/// メッセージループを回す（画面が閉じるまで戻らない）。Tab での移動、Enter で既定のボタン、
/// Esc でキャンセルを、ダイアログと同じように扱う。
pub unsafe fn run_message_loop(hwnd: HWND) {
    let mut msg = MSG::default();
    loop {
        let ret = GetMessageW(&mut msg, None, 0, 0);
        if ret.0 <= 0 {
            break;
        }
        if IsDialogMessageW(hwnd, &msg).as_bool() {
            continue;
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

/// マウスカーソルのあるモニター（トレイメニューを開いたモニター）の作業領域。
pub unsafe fn cursor_work_area() -> RECT {
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

/// 画面の大きさ（中身が 96 DPI 基準で `client_w` × `client_h`）を DPI に合わせて決め、
/// 作業領域の中央に置く。
pub unsafe fn place_window(hwnd: HWND, dpi: u32, client_w: i32, client_h: i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scale(client_w, dpi),
        bottom: scale(client_h, dpi),
    };
    let _ = AdjustWindowRectExForDpi(
        &mut rect,
        WINDOW_STYLE_FRAME,
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
pub fn scale(value: i32, dpi: u32) -> i32 {
    ime_logic::scale_for_dpi(value, dpi)
}

/// 画面の文字に使うフォント（Windows の「メッセージ」のフォントを DPI に合わせて作る）。
pub unsafe fn create_ui_font(dpi: u32) -> HFONT {
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
pub unsafe fn create_controls(hwnd: HWND, items: &[Item], font: HFONT) {
    let instance = GetModuleHandleW(None).unwrap_or_default();
    for item in items {
        let (class, style, ex_style) = match item.kind {
            Kind::Group => (w!("BUTTON"), BS_GROUPBOX as u32, 0),
            Kind::Label => (w!("STATIC"), SS_CENTERIMAGE, 0),
            Kind::Note => (w!("STATIC"), 0, 0),
            Kind::Check => (w!("BUTTON"), BS_AUTOCHECKBOX as u32 | WS_TABSTOP.0, 0),
            Kind::Combo => (
                w!("COMBOBOX"),
                CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0 | WS_TABSTOP.0,
                0,
            ),
            Kind::Edit => (
                w!("EDIT"),
                ES_AUTOHSCROLL as u32 | WS_TABSTOP.0,
                WS_EX_CLIENTEDGE.0,
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
            Kind::ListBox => (
                w!("LISTBOX"),
                LBS_NOTIFY as u32 | WS_VSCROLL.0 | WS_TABSTOP.0,
                WS_EX_CLIENTEDGE.0,
            ),
            Kind::OwnerList => (
                w!("LISTBOX"),
                // LBS_OWNERDRAWFIXED | LBS_NODATA | LBS_NOINTEGRALHEIGHT | LBS_NOTIFY
                // 枠は付けない（一覧の画面の側で 1 ピクセルの枠を描く）。
                0x0010 | 0x2000 | 0x0100 | LBS_NOTIFY as u32,
                0,
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
}

/// コントロールを DPI に合わせた位置と大きさに置く。
pub unsafe fn layout(hwnd: HWND, items: &[Item], dpi: u32) {
    for item in items {
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

/// 別の DPI のモニターへ移ったとき（`WM_DPICHANGED`）の処理。勧められた大きさに合わせ、
/// 新しいフォントを作ってコントロールの文字と配置を作り直す。古いフォントは削除し、
/// 新しいフォントを返す（呼び出し側で持っておき、閉じるときに削除する）。
pub unsafe fn on_dpi_changed(
    hwnd: HWND,
    items: &[Item],
    wparam: WPARAM,
    lparam: LPARAM,
    old_font: HFONT,
) -> HFONT {
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
    for item in items {
        if let Ok(control) = GetDlgItem(hwnd, item.id) {
            SendMessageW(control, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
    }
    layout(hwnd, items, dpi);
    if !old_font.is_invalid() {
        let _ = DeleteObject(old_font);
    }
    font
}

pub unsafe fn add_combo_items<'a>(hwnd: HWND, id: i32, items: impl Iterator<Item = &'a str>) {
    if let Ok(combo) = GetDlgItem(hwnd, id) {
        SendMessageW(combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
        for name in items {
            let text = wide(name);
            SendMessageW(combo, CB_ADDSTRING, WPARAM(0), LPARAM(text.as_ptr() as isize));
        }
    }
}

pub unsafe fn set_combo_index(hwnd: HWND, id: i32, index: usize) {
    if let Ok(combo) = GetDlgItem(hwnd, id) {
        SendMessageW(combo, CB_SETCURSEL, WPARAM(index), LPARAM(0));
    }
}

/// 選ばれている項目の番号。選ばれていなければ `None`。
pub unsafe fn combo_index(hwnd: HWND, id: i32) -> Option<usize> {
    let index = GetDlgItem(hwnd, id)
        .map(|combo| SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0)
        .unwrap_or(-1);
    usize::try_from(index).ok()
}

pub unsafe fn set_checked(hwnd: HWND, id: i32, checked: bool) {
    let state = if checked { BST_CHECKED } else { BST_UNCHECKED };
    let _ = CheckDlgButton(hwnd, id, state);
}

pub unsafe fn is_checked(hwnd: HWND, id: i32) -> bool {
    IsDlgButtonChecked(hwnd, id) == BST_CHECKED.0
}

pub unsafe fn set_text(hwnd: HWND, id: i32, text: &str) {
    if let Ok(control) = GetDlgItem(hwnd, id) {
        let text = wide(text);
        let _ = SetWindowTextW(control, PCWSTR(text.as_ptr()));
    }
}

pub unsafe fn get_text(hwnd: HWND, id: i32) -> String {
    let Ok(control) = GetDlgItem(hwnd, id) else {
        return String::new();
    };
    let len = GetWindowTextLengthW(control).max(0) as usize;
    let mut buf = vec![0u16; len + 1];
    let copied = GetWindowTextW(control, &mut buf).max(0) as usize;
    String::from_utf16_lossy(&buf[..copied.min(len)])
}

/// コントロールを使える・使えない（灰色）にする。
pub unsafe fn set_enabled(hwnd: HWND, id: i32, enabled: bool) {
    if let Ok(control) = GetDlgItem(hwnd, id) {
        let _ = EnableWindow(control, enabled);
    }
}

/// コントロールを表示する・隠す。
pub unsafe fn set_visible(hwnd: HWND, id: i32, visible: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNA};
    if let Ok(control) = GetDlgItem(hwnd, id) {
        let _ = ShowWindow(control, if visible { SW_SHOWNA } else { SW_HIDE });
    }
}

/// ファイルを選ぶ画面を出す（`save` が真なら保存先を選ぶ）。選ばれなければ `None`。
///
/// `filter` は「表示名\0パターン\0」の並び（例 `"JSON ファイル (*.json)\0*.json\0"`）、
/// `default_ext` は拡張子を書かなかったときに付ける拡張子（`.` なし）。
pub unsafe fn choose_file(
    owner: HWND,
    save: bool,
    filter: &str,
    default_ext: &str,
    default_name: &str,
) -> Option<std::path::PathBuf> {
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
        OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    // 選ばれたパスを受け取る場所（長いパスにも足りる大きさ）。
    let mut buffer = vec![0u16; 32 * 1024];
    let name: Vec<u16> = default_name.encode_utf16().collect();
    buffer[..name.len().min(260)].copy_from_slice(&name[..name.len().min(260)]);
    let filter: Vec<u16> = filter.encode_utf16().chain([0, 0]).collect();
    let default_ext = wide(default_ext);
    let mut flags = OFN_NOCHANGEDIR | OFN_HIDEREADONLY | OFN_PATHMUSTEXIST;
    flags |= if save {
        OFN_OVERWRITEPROMPT
    } else {
        OFN_FILEMUSTEXIST
    };
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: 1,
        lpstrFile: windows::core::PWSTR(buffer.as_mut_ptr()),
        nMaxFile: buffer.len() as u32,
        lpstrDefExt: PCWSTR(default_ext.as_ptr()),
        Flags: flags,
        ..Default::default()
    };
    let chosen = if save {
        GetSaveFileNameW(&mut ofn)
    } else {
        GetOpenFileNameW(&mut ofn)
    };
    if !chosen.as_bool() {
        return None;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(std::path::PathBuf::from(String::from_utf16_lossy(&buffer[..len])))
}

pub unsafe fn focus(hwnd: HWND, id: i32) {
    if let Ok(control) = GetDlgItem(hwnd, id) {
        let _ = SetFocus(control);
    }
}

/// 画面を持ち主にしてメッセージを出す（画面の後ろに隠れないように）。
pub unsafe fn show_message(owner: HWND, text: &str, icon: MESSAGEBOX_STYLE) {
    let text = wide(text);
    MessageBoxW(owner, PCWSTR(text.as_ptr()), w!("アタイの貼り付け"), MB_OK | icon);
}

/// 誤りを知らせ、直すべきコントロールへ移る。
pub unsafe fn report_invalid(hwnd: HWND, id: i32, message: &str) {
    show_message(
        hwnd,
        message,
        windows::Win32::UI::WindowsAndMessaging::MB_ICONWARNING,
    );
    focus(hwnd, id);
}

/// 数を読み、範囲内なら返す。
pub fn parse_in_range(text: &str, min: u64, max: u64) -> Option<u64> {
    text.trim().parse::<u64>().ok().filter(|v| (min..=max).contains(v))
}

/// 文字列を UTF-16 のヌル終端バッファへ変換する。
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
