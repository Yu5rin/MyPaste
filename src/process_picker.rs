//! プロセス名を選ぶ画面。
//!
//! 対象アプリ・記録しないアプリ・効くアプリの欄の横にある「選ぶ...」から開く。
//! いま開いているウィンドウを、アプリ（プロセス名）ごとにまとめて一覧にし、選んだアプリの
//! プロセス名を返す。プロセス名が分からないアプリでも、使いたいアプリを開いておけば
//! この一覧で名前を確かめて、そのまま欄に足せる。
//!
//! 一覧に出すのは、見えていて題名のあるウィンドウだけ（ツールウィンドウ・隠れたウィンドウと、
//! このアプリ自身は除く）。管理者として動いているアプリは名前を取れないため出ない
//! （その種類のアプリではキーの置き換えも効かない）。
//!
//! 呼び出し元の画面のスレッドで動き、閉じるまで戻らない（その間、呼び出し元の画面は
//! 操作できない）。まとめ方と並べ方は [`crate::remap_logic::running_apps`] にある。

use std::cell::RefCell;

use windows::core::w;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::DeleteObject;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows, GetMessageW,
    GetWindowLongW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsDialogMessageW, IsWindow, IsWindowVisible, PostQuitMessage, SetForegroundWindow,
    ShowWindow, TranslateMessage, GWL_EXSTYLE, IDCANCEL, IDOK, MSG, SW_SHOW, WM_CLOSE,
    WM_COMMAND, WM_DPICHANGED, WM_QUIT, WS_EX_TOOLWINDOW,
};

use crate::excel_check;
use crate::remap_logic::{self, RunningApp};
use crate::ui::{self, item, Item, Kind};

const ID_LIST: i32 = 601;
const ID_REFRESH: i32 = 602;
/// 「追加」（Enter と同じ）。
const ID_OK: i32 = IDOK.0;
/// 「キャンセル」（Esc と同じ）。
const ID_CANCEL: i32 = IDCANCEL.0;
/// `LBN_DBLCLK`（一覧の行をダブルクリックした通知コード）。
const LBN_DBLCLK: u32 = 2;

/// 画面の中身の大きさ（96 DPI 基準）。
const CLIENT_W: i32 = 480;
const CLIENT_H: i32 = 380;

const ITEMS: &[Item] = &[
    item(
        700,
        Kind::Note,
        "いま開いているアプリの一覧です。追加するアプリを選んで「追加」を押します（ダブルクリックでも追加）。一覧に無いときは、そのアプリを開いてから「一覧を更新」を押します。",
        12,
        10,
        456,
        52,
    ),
    item(ID_LIST, Kind::ListBox, "", 12, 66, 456, 262),
    item(ID_REFRESH, Kind::Button, "一覧を更新", 12, 340, 100, 28),
    item(ID_OK, Kind::DefaultButton, "追加", 290, 340, 86, 28),
    item(ID_CANCEL, Kind::Button, "キャンセル", 382, 340, 86, 28),
];

/// 開いている間の状態。
struct Context {
    apps: Vec<RunningApp>,
    owner: HWND,
    chosen: Option<String>,
    font: windows::Win32::Graphics::Gdi::HFONT,
}

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

/// 画面を開き、選ばれたプロセス名（例 `EXCEL.EXE`）を返す。キャンセルなら `None`。
///
/// 戻ったとき、呼び出し元の画面が閉じられている（アプリの終了など）ことがある。
/// 呼び出し元は `IsWindow` で確かめてから欄を書き換える。
pub unsafe fn choose(owner: HWND) -> Option<String> {
    if CONTEXT.with(|c| c.borrow().is_some()) {
        return None;
    }
    let hwnd = ui::create_owned_window(
        w!("AtaiPasteProcessPicker"),
        Some(wnd_proc),
        "プロセス名を選ぶ",
        owner,
    )?;
    let dpi = GetDpiForWindow(hwnd);
    let font = ui::create_ui_font(dpi);
    CONTEXT.with(|c| {
        *c.borrow_mut() = Some(Context {
            apps: Vec::new(),
            owner,
            chosen: None,
            font,
        })
    });
    ui::create_controls(hwnd, ITEMS, font);
    fill_list(hwnd);
    ui::place_window(hwnd, dpi, CLIENT_W, CLIENT_H);
    ui::layout(hwnd, ITEMS, dpi);

    let _ = EnableWindow(owner, false);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    ui::focus(hwnd, ID_LIST);

    // 閉じるまでここでメッセージを回す。WM_QUIT を取り出したら、置き直して元のループに任せる。
    let mut msg = MSG::default();
    while IsWindow(hwnd).as_bool() {
        let ret = GetMessageW(&mut msg, None, 0, 0);
        if ret.0 <= 0 {
            if msg.message == WM_QUIT {
                PostQuitMessage(msg.wParam.0 as i32);
            }
            break;
        }
        if IsDialogMessageW(hwnd, &msg).as_bool() {
            continue;
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    if IsWindow(hwnd).as_bool() {
        let _ = EnableWindow(owner, true);
        let _ = DestroyWindow(hwnd);
    }
    let context = CONTEXT.with(|c| c.borrow_mut().take())?;
    let _ = DeleteObject(context.font);
    context.chosen
}

/// 開いているウィンドウを集めて一覧に入れる（前に選んでいたアプリがあれば選び直す）。
unsafe fn fill_list(hwnd: HWND) {
    let previous = selected_name(hwnd);
    let apps = running_apps();
    let rows: Vec<String> = apps.iter().map(remap_logic::running_app_row).collect();
    let select = previous
        .and_then(|name| apps.iter().position(|a| a.name == name))
        .or(if apps.is_empty() { None } else { Some(0) });
    ui::set_list_items(hwnd, ID_LIST, &rows, select);
    ui::set_enabled(hwnd, ID_OK, !apps.is_empty());
    CONTEXT.with(|c| {
        if let Some(context) = c.borrow_mut().as_mut() {
            context.apps = apps;
        }
    });
}

/// 一覧で選んでいるアプリのプロセス名。
unsafe fn selected_name(hwnd: HWND) -> Option<String> {
    let index = ui::list_index(hwnd, ID_LIST)?;
    CONTEXT.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|context| context.apps.get(index).map(|a| a.name.clone()))
    })
}

/// いま開いているアプリ（見えていて題名のあるウィンドウを、アプリごとにまとめたもの）。
unsafe fn running_apps() -> Vec<RunningApp> {
    let mut windows: Vec<HWND> = Vec::new();
    let _ = EnumWindows(
        Some(collect_window),
        LPARAM(&mut windows as *mut Vec<HWND> as isize),
    );
    let own = GetCurrentProcessId();
    let mut found: Vec<(String, String)> = Vec::new();
    for hwnd in windows {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == own {
            continue;
        }
        let Some(name) = excel_check::process_name_of_window(hwnd) else {
            continue;
        };
        found.push((name, window_title(hwnd)));
    }
    remap_logic::running_apps(found.iter().map(|(n, t)| (n.as_str(), t.as_str())))
}

/// `EnumWindows` の呼び出し先。一覧に出すウィンドウを集める（手前から順に届く）。
unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let windows = &mut *(lparam.0 as *mut Vec<HWND>);
    if is_listed_window(hwnd) {
        windows.push(hwnd);
    }
    BOOL(1)
}

/// 一覧に出すウィンドウか（見えていて、題名があり、ツールウィンドウでなく、隠されていない）。
unsafe fn is_listed_window(hwnd: HWND) -> bool {
    if !IsWindowVisible(hwnd).as_bool() || GetWindowTextLengthW(hwnd) == 0 {
        return false;
    }
    if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
        return false;
    }
    // ストアアプリなどは、見えていない間も「見えている」ウィンドウを持っていることがある。
    let mut cloaked = 0u32;
    let hidden = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut core::ffi::c_void,
        std::mem::size_of::<u32>() as u32,
    )
    .is_ok()
        && cloaked != 0;
    !hidden
}

/// ウィンドウの題名。
unsafe fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = GetWindowTextW(hwnd, &mut buf);
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

/// 選んだアプリを結果にして閉じる。
unsafe fn accept(hwnd: HWND) {
    let Some(name) = selected_name(hwnd) else {
        return;
    };
    CONTEXT.with(|c| {
        if let Some(context) = c.borrow_mut().as_mut() {
            context.chosen = Some(name);
        }
    });
    close(hwnd);
}

/// 閉じる。持ち主の画面を先に使えるようにしておく（そうしないと、ほかのアプリが手前に出る）。
unsafe fn close(hwnd: HWND) {
    let owner = CONTEXT.with(|c| c.borrow().as_ref().map(|context| context.owner));
    if let Some(owner) = owner {
        let _ = EnableWindow(owner, true);
    }
    let _ = DestroyWindow(hwnd);
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            match (id, code) {
                (ID_LIST, LBN_DBLCLK) => accept(hwnd),
                (ID_OK, ui::BN_CLICKED) => accept(hwnd),
                (ID_CANCEL, ui::BN_CLICKED) => close(hwnd),
                (ID_REFRESH, ui::BN_CLICKED) => fill_list(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            close(hwnd);
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
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
