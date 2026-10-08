//! クリップボードの履歴の一覧（Clibor のような一覧の画面）。
//!
//! 一覧を出す操作をすると、入力位置（またはマウス）の近くに小さな画面を出す。
//!
//! - 上: 絞り込みの欄。文字を打つと、その語を含むものだけに絞る（空白で区切ると、すべてを含むもの）。
//! - 中: 履歴の一覧。1 行に 1 件、番号・最初の行・行数を出し、1 行おきに色を変えて読みやすくする。
//!   マウスを乗せた行・矢印キーで選んだ行が選ばれる。
//! - 下: 選んでいるものの全文。
//!
//! クリックか Enter で、選んだものを貼り付ける（[`crate::actions`] が行う）。Esc か、ほかの場所を
//! クリックすると何もせずに閉じる。Shift+Delete で、選んでいる 1 件を履歴から消す。
//!
//! 実行スレッド（[`crate::actions`]）で、閉じるまで専用のメッセージループを回す。
//! 中身の整え方・絞り込みは [`crate::clip_history`] にあり、Linux でもテストできる。

use std::cell::RefCell;
use std::time::Duration;

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetMonitorInfoW, GetSysColor,
    MonitorFromPoint, SelectObject, SetBkMode, SetTextColor, COLOR_BTNFACE, COLOR_GRAYTEXT,
    COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DT_END_ELLIPSIS,
    DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, HBRUSH, HDC, HFONT, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, EM_SETCUEBANNER, ODS_SELECTED};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_DELETE, VK_DOWN, VK_ESCAPE, VK_NEXT, VK_PRIOR, VK_RETURN, VK_SHIFT,
    VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetDlgItem,
    GetForegroundWindow, GetGUIThreadInfo, GetMessageW, GetWindowThreadProcessId, IsChild,
    PostQuitMessage, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowPos,
    TranslateMessage, GUITHREADINFO, HWND_TOPMOST, LB_GETCURSEL,
    LB_ITEMFROMPOINT, LB_SETCOUNT, LB_SETCURSEL, LB_SETITEMHEIGHT, MSG, SWP_SHOWWINDOW,
    WM_ACTIVATE, WM_CLOSE, WM_COMMAND, WM_DRAWITEM, WM_KEYDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WINDOW_STYLE, WNDCLASSW, WS_CAPTION, WS_EX_TOOLWINDOW, WS_SYSMENU,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::clip_history::{self, MenuPosition};
use crate::ime_indicator;
use crate::ui::{self, item, set_text, wide, Item, Kind};

const ID_SEARCH: i32 = 1;
const ID_LIST: i32 = 2;
const ID_PREVIEW: i32 = 3;
const ID_HINT: i32 = 4;

/// 画面の中身の大きさ（96 DPI 基準）。
const CLIENT_W: i32 = 460;
const CLIENT_H: i32 = 510;
/// 一覧の 1 行の高さ（96 DPI 基準）。
const ROW_H: i32 = 26;
/// 番号の欄と、行数の欄の幅（96 DPI 基準）。
const NUMBER_W: i32 = 34;
const LINES_W: i32 = 48;
/// 絞り込みの欄が書き換わった（EN_CHANGE）、一覧で選んだ行が変わった（LBN_SELCHANGE）。
const EN_CHANGE: u32 = 0x0300;
const LBN_SELCHANGE: u32 = 1;
/// 画面の枠。小さなタイトルバー（ドラッグで動かせる）と閉じるボタンを付ける。タイトルバーの
/// 無い画面は、環境によってはキー入力を受け取れないことがあるため。
const WINDOW_FRAME: WINDOW_STYLE = WINDOW_STYLE(WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0);
/// 前面にするまで待つ時間。
const SHOW_WAIT: Duration = Duration::from_millis(30);

const ITEMS: &[Item] = &[
    item(ID_SEARCH, Kind::Edit, "", 8, 8, 444, 26),
    item(ID_LIST, Kind::OwnerList, "", 8, 40, 444, 330),
    item(ID_PREVIEW, Kind::ReadOnlyText, "", 8, 376, 444, 100),
    item(ID_HINT, Kind::Label, "", 8, 480, 444, 24),
];

/// 一覧の画面の結果。
#[derive(Debug, Default)]
pub struct Outcome {
    /// 選ばれたもの（選ばずに閉じたら `None`）。
    pub chosen: Option<String>,
    /// 一覧で消したもの。
    pub removed: Vec<String>,
}

/// 画面を開いている間の状態。
struct State {
    /// 履歴（新しい順。一覧で消したものは除いていく）。
    items: Vec<String>,
    /// 絞り込んだ結果（`items` の位置）。
    visible: Vec<usize>,
    dpi: u32,
    font: HFONT,
    outcome: Outcome,
    done: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// 一覧の画面を出し、閉じるまで待つ。画面を前面にできなかったときは `None`
/// （前面にできないまま出すと、キーで操作できず、閉じられなくなるため）。
pub unsafe fn choose(items: Vec<String>, position: MenuPosition, target: HWND) -> Option<Outcome> {
    let anchor = anchor_point(target, position);
    let monitor = MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST);
    let mut dpi_x = 96;
    let mut dpi_y = 96;
    if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_err() {
        dpi_x = 96;
    }
    let dpi = dpi_x;

    let hwnd = create_window()?;
    let font = ui::create_ui_font(dpi);
    let count = items.len();
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            visible: (0..count).collect(),
            items,
            dpi,
            font,
            outcome: Outcome::default(),
            done: false,
        })
    });
    ui::create_controls(hwnd, ITEMS, font);
    ui::layout(hwnd, ITEMS, dpi);
    let list = control(hwnd, ID_LIST);
    SendMessageW(list, LB_SETITEMHEIGHT, WPARAM(0), LPARAM(ui::scale(ROW_H, dpi) as isize));
    let cue = wide("絞り込み（文字を入力すると、それを含むものだけを出します）");
    SendMessageW(
        control(hwnd, ID_SEARCH),
        EM_SETCUEBANNER,
        WPARAM(1),
        LPARAM(cue.as_ptr() as isize),
    );
    refresh(hwnd, 0);

    // 置く場所: 基準の位置の下（入らなければ上）。モニターの作業領域に収める。
    let (width, height) = window_size(dpi);
    let work = work_area(monitor);
    let mut x = anchor.x;
    let mut y = anchor.y + ui::scale(4, dpi);
    if y + height > work.bottom {
        y = anchor.y - height - ui::scale(24, dpi);
    }
    x = x.clamp(work.left, (work.right - width).max(work.left));
    y = y.clamp(work.top, (work.bottom - height).max(work.top));
    let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_SHOWWINDOW);
    pump_for(SHOW_WAIT);
    if !bring_to_front(hwnd, target) {
        log::warn!("クリップボードの履歴: 一覧を前面にできませんでした");
        close(hwnd);
        return None;
    }
    let _ = SetFocus(control(hwnd, ID_SEARCH));

    run_loop(hwnd);
    let outcome = STATE.with(|s| s.borrow_mut().as_mut().map(|st| std::mem::take(&mut st.outcome)));
    close(hwnd);
    outcome
}

/// 画面を閉じ、状態を片付ける。
unsafe fn close(hwnd: HWND) {
    let _ = DestroyWindow(hwnd);
    if let Some(state) = STATE.with(|s| s.borrow_mut().take()) {
        let _ = DeleteObject(state.font);
    }
}

/// 閉じるまでメッセージを処理する。キー操作は、絞り込みの欄に入力しながらでも一覧を動かせる
/// よう、ここで先に受け取る。
unsafe fn run_loop(hwnd: HWND) {
    let list = control(hwnd, ID_LIST);
    let mut msg = MSG::default();
    while !is_done() {
        let ret = GetMessageW(&mut msg, None, 0, 0);
        if ret.0 <= 0 {
            // アプリの終了。元のループにも知らせる。
            PostQuitMessage(msg.wParam.0 as i32);
            finish(None);
            break;
        }
        let ours = msg.hwnd == hwnd || IsChild(hwnd, msg.hwnd).as_bool();
        if ours && msg.message == WM_KEYDOWN && on_key(hwnd, msg.wParam.0 as u16) {
            continue;
        }
        if ours && msg.message == WM_MOUSEWHEEL && msg.hwnd != list {
            // ホイールは、どこで回しても一覧を動かす。
            SendMessageW(list, WM_MOUSEWHEEL, msg.wParam, msg.lParam);
            continue;
        }
        if msg.hwnd == list && msg.message == WM_MOUSEMOVE {
            // マウスを乗せた行を選ぶ。
            if let Some(row) = row_at(list, msg.lParam) {
                if current_row(list) != Some(row) {
                    select(hwnd, row);
                }
            }
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
        if msg.hwnd == list && msg.message == WM_LBUTTONUP && row_at(list, msg.lParam).is_some() {
            choose_current(hwnd);
        }
    }
}

fn is_done() -> bool {
    STATE.with(|s| s.borrow().as_ref().is_none_or(|st| st.done))
}

/// 選んで（または選ばずに）閉じる。
fn finish(chosen: Option<String>) {
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.outcome.chosen = chosen;
            state.done = true;
        }
    });
}

/// キー操作。処理したら `true`（絞り込みの欄には渡さない）。
unsafe fn on_key(hwnd: HWND, vk: u16) -> bool {
    let list = control(hwnd, ID_LIST);
    let count = STATE.with(|s| s.borrow().as_ref().map_or(0, |st| st.visible.len()));
    let current = current_row(list);
    let page = page_rows();
    let move_to = |row: isize| {
        if count > 0 {
            select(hwnd, row.clamp(0, count as isize - 1) as usize);
        }
    };
    let current = current.map_or(-1, |c| c as isize);
    match vk {
        v if v == VK_ESCAPE.0 => finish(None),
        v if v == VK_RETURN.0 => choose_current(hwnd),
        v if v == VK_UP.0 => move_to(current - 1),
        v if v == VK_DOWN.0 => move_to(current + 1),
        v if v == VK_PRIOR.0 => move_to(current - page),
        v if v == VK_NEXT.0 => move_to(current + page),
        v if v == VK_DELETE.0 && GetKeyState(VK_SHIFT.0 as i32) < 0 => remove_current(hwnd),
        _ => return false,
    }
    true
}

/// 一覧に見えている行の数（PageUp / PageDown で動く数）。
fn page_rows() -> isize {
    STATE.with(|s| {
        s.borrow().as_ref().map_or(10, |st| {
            let list_h = ui::scale(ITEMS[1].h, st.dpi);
            (list_h / ui::scale(ROW_H, st.dpi)).max(1) as isize
        })
    })
}

/// 選んでいるものを貼り付けるために閉じる。
unsafe fn choose_current(hwnd: HWND) {
    let Some(row) = current_row(control(hwnd, ID_LIST)) else {
        return;
    };
    let chosen = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.visible.get(row).and_then(|&i| st.items.get(i).cloned()))
    });
    if chosen.is_some() {
        finish(chosen);
    }
}

/// 選んでいる 1 件を履歴から消す。
unsafe fn remove_current(hwnd: HWND) {
    let Some(row) = current_row(control(hwnd, ID_LIST)) else {
        return;
    };
    let removed = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.as_mut()?;
        let index = *st.visible.get(row)?;
        let text = st.items.remove(index);
        st.outcome.removed.push(text);
        Some(())
    });
    if removed.is_some() {
        refresh(hwnd, row);
    }
}

/// 絞り込みをやり直し、一覧と下の欄を作り直す。`select_row` の行を選ぶ（無ければ最後の行）。
unsafe fn refresh(hwnd: HWND, select_row: usize) {
    let query = ui::get_text(hwnd, ID_SEARCH);
    let (count, total) = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(st) = s.as_mut() else {
            return (0, 0);
        };
        st.visible = clip_history::filter(&st.items, &query);
        (st.visible.len(), st.items.len())
    });
    let list = control(hwnd, ID_LIST);
    SendMessageW(list, LB_SETCOUNT, WPARAM(count), LPARAM(0));
    let hint = if count == total {
        format!("{total} 件　クリック / Enter で貼り付け　Esc で閉じる　Shift+Delete で消す")
    } else {
        format!("{count} / {total} 件　クリック / Enter で貼り付け　Esc で閉じる　Shift+Delete で消す")
    };
    set_text(hwnd, ID_HINT, &hint);
    if count == 0 {
        set_text(hwnd, ID_PREVIEW, "");
        return;
    }
    select(hwnd, select_row.min(count - 1));
}

/// 行を選び、下の欄に全文を出す。
unsafe fn select(hwnd: HWND, row: usize) {
    let list = control(hwnd, ID_LIST);
    SendMessageW(list, LB_SETCURSEL, WPARAM(row), LPARAM(0));
    update_preview(hwnd);
}

unsafe fn update_preview(hwnd: HWND) {
    let row = current_row(control(hwnd, ID_LIST));
    let text = STATE.with(|s| {
        s.borrow().as_ref().and_then(|st| {
            row.and_then(|r| st.visible.get(r))
                .and_then(|&i| st.items.get(i))
                .map(|t| clip_history::preview_text(t))
        })
    });
    set_text(hwnd, ID_PREVIEW, &text.unwrap_or_default());
}

/// 一覧で選んでいる行。
unsafe fn current_row(list: HWND) -> Option<usize> {
    usize::try_from(SendMessageW(list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0).ok()
}

/// 一覧の中の位置（`lparam` はマウスの位置）にある行。行の外なら `None`。
unsafe fn row_at(list: HWND, lparam: LPARAM) -> Option<usize> {
    let result = SendMessageW(list, LB_ITEMFROMPOINT, WPARAM(0), lparam).0 as usize;
    // 上位の 16 ビットが 1 なら、行の外。
    if (result >> 16) & 0xFFFF != 0 {
        return None;
    }
    let row = result & 0xFFFF;
    let count = STATE.with(|s| s.borrow().as_ref().map_or(0, |st| st.visible.len()));
    (row < count).then_some(row)
}

unsafe fn control(hwnd: HWND, id: i32) -> HWND {
    GetDlgItem(hwnd, id).unwrap_or_default()
}

/// 1 行を描く: 番号・最初の行・行数。1 行おきに背景の色を少し変える。
unsafe fn draw_row(item: &DRAWITEMSTRUCT) {
    let Ok(row) = usize::try_from(item.itemID) else {
        return;
    };
    let Some((number, text, lines, dpi, font)) = STATE.with(|s| {
        s.borrow().as_ref().and_then(|st| {
            let index = *st.visible.get(row)?;
            let (text, lines) = clip_history::row_text(st.items.get(index)?);
            // 番号は、履歴の中での位置（新しいものから 1, 2, …）。絞り込んでも変わらない。
            Some((index + 1, text, lines, st.dpi, st.font))
        })
    }) else {
        return;
    };
    let hdc = item.hDC;
    let rect = item.rcItem;
    let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
    let background = if selected {
        GetSysColor(COLOR_HIGHLIGHT)
    } else if row % 2 == 1 {
        blend(GetSysColor(COLOR_WINDOW), GetSysColor(COLOR_HIGHLIGHT), 8)
    } else {
        GetSysColor(COLOR_WINDOW)
    };
    fill(hdc, &rect, background);
    let old_font = SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT);
    let (main, sub) = if selected {
        let c = GetSysColor(COLOR_HIGHLIGHTTEXT);
        (c, c)
    } else {
        (GetSysColor(COLOR_WINDOWTEXT), GetSysColor(COLOR_GRAYTEXT))
    };
    let pad = ui::scale(6, dpi);
    // 番号（右寄せ）
    let mut number_rect = RECT {
        left: rect.left,
        right: rect.left + ui::scale(NUMBER_W, dpi),
        ..rect
    };
    SetTextColor(hdc, COLORREF(sub));
    draw_text(hdc, &number.to_string(), &mut number_rect, DT_RIGHT);
    // 行数（2 行以上のとき、右端に）
    let mut text_right = rect.right - pad;
    if lines > 1 {
        let mut lines_rect = RECT {
            left: rect.right - ui::scale(LINES_W, dpi),
            right: rect.right - pad,
            ..rect
        };
        draw_text(hdc, &format!("{lines} 行"), &mut lines_rect, DT_RIGHT);
        text_right = lines_rect.left - pad;
    }
    // 最初の行
    let mut text_rect = RECT {
        left: number_rect.right + ui::scale(10, dpi),
        right: text_right,
        ..rect
    };
    SetTextColor(hdc, COLORREF(main));
    draw_text(hdc, &text, &mut text_rect, DT_LEFT | DT_END_ELLIPSIS);
    SelectObject(hdc, old_font);
}

unsafe fn draw_text(
    hdc: HDC,
    text: &str,
    rect: &mut RECT,
    align: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    DrawTextW(hdc, &mut units, rect, align | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
}

unsafe fn fill(hdc: HDC, rect: &RECT, color: u32) {
    let brush = CreateSolidBrush(COLORREF(color));
    FillRect(hdc, rect, brush);
    let _ = DeleteObject(brush);
}

/// 2 つの色を混ぜる（`percent` は `b` の割合）。
fn blend(a: u32, b: u32, percent: u32) -> u32 {
    let mix = |shift: u32| {
        let x = (a >> shift) & 0xFF;
        let y = (b >> shift) & 0xFF;
        ((x * (100 - percent) + y * percent) / 100) << shift
    };
    mix(0) | mix(8) | mix(16)
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_DRAWITEM => {
            draw_row(&*(lparam.0 as *const DRAWITEMSTRUCT));
            LRESULT(1)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            match (id, code) {
                (ID_SEARCH, EN_CHANGE) => refresh(hwnd, 0),
                (ID_LIST, LBN_SELCHANGE) => update_preview(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        // ほかのウィンドウに移ったら（ほかの場所をクリックしたら）、何もせずに閉じる。
        WM_ACTIVATE if wparam.0 & 0xFFFF == 0 => {
            if !is_done() {
                finish(None);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            finish(None);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 画面を作る（まだ表示しない）。
unsafe fn create_window() -> Option<HWND> {
    let instance = GetModuleHandleW(None).ok()?;
    let class_name = w!("AtaiPasteHistory");
    let class = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance.into(),
        lpszClassName: class_name,
        hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as isize as *mut core::ffi::c_void),
        ..Default::default()
    };
    // 2 回目以降は登録済みで失敗するが、そのまま使える。
    RegisterClassW(&class);
    CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
        class_name,
        &HSTRING::from("クリップボードの履歴"),
        WINDOW_FRAME,
        0,
        0,
        0,
        0,
        None,
        None,
        instance,
        None,
    )
    .ok()
}

/// 枠を含めた画面の大きさ。
unsafe fn window_size(dpi: u32) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: ui::scale(CLIENT_W, dpi),
        bottom: ui::scale(CLIENT_H, dpi),
    };
    let _ = AdjustWindowRectExForDpi(
        &mut rect,
        WINDOW_FRAME,
        false,
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
        dpi,
    );
    (rect.right - rect.left, rect.bottom - rect.top)
}

unsafe fn work_area(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> RECT {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(monitor, &mut info).as_bool() {
        info.rcWork
    } else {
        ui::cursor_work_area()
    }
}

/// 画面を出す基準の位置。入力位置の近くにするときは、入力位置（キャレット）が分かればその下、
/// 分からなければマウスの位置。
unsafe fn anchor_point(target: HWND, position: MenuPosition) -> POINT {
    if position == MenuPosition::Caret && !target.is_invalid() {
        let thread_id = GetWindowThreadProcessId(target, None);
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if thread_id != 0 && GetGUIThreadInfo(thread_id, &mut info).is_ok() {
            if let Some(caret) = ime_indicator::caret_rect(&info) {
                return POINT {
                    x: caret.left,
                    y: caret.bottom,
                };
            }
        }
    }
    let mut point = POINT::default();
    let _ = GetCursorPos(&mut point);
    point
}

/// 画面を前面にする（キーで操作できるように）。前面にできたら `true`。
/// Windows は、ほかのアプリが前面のときに前面を奪うことを制限しているので、通らなければ
/// 前面のアプリの入力の流れに一時的に加わってから前面にする。
unsafe fn bring_to_front(window: HWND, current: HWND) -> bool {
    let _ = SetForegroundWindow(window);
    if GetForegroundWindow() == window {
        return true;
    }
    let ours = GetCurrentThreadId();
    let theirs = if current.is_invalid() {
        0
    } else {
        GetWindowThreadProcessId(current, None)
    };
    if theirs != 0 && theirs != ours && AttachThreadInput(ours, theirs, true).as_bool() {
        let _ = SetForegroundWindow(window);
        let _ = AttachThreadInput(ours, theirs, false);
    }
    GetForegroundWindow() == window
}

/// 少しの間、メッセージを処理する（表示が追いつくのを待つ）。
unsafe fn pump_for(duration: Duration) {
    use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, PM_REMOVE};
    let start = std::time::Instant::now();
    let mut msg = MSG::default();
    while start.elapsed() < duration {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                PostQuitMessage(msg.wParam.0 as i32);
                return;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
