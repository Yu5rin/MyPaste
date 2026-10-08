//! クリップボードの履歴の一覧（Clibor のような、小さな一覧の画面）。
//!
//! 一覧を出す操作をすると、入力位置（またはマウス）の近くに細長い画面を出す。
//!
//! - 上: 検索の欄。文字を打つと、その語を含むものだけに絞る（空白で区切ると、すべてを含むもの）。
//! - その下: ページの見出し（`1〜20 件目 / 100 件`）と、前後のページへ移るボタン。
//! - 一覧: 1 ページ 20 件。1 行に 1 件、番号・最初の行・行数を出し、1 行おきに色を変える。
//!   マウスを乗せた行・矢印キーで選んだ行が選ばれる。
//! - 選んだものが 1 行に収まらないときは、横に全文の吹き出しを出す。
//!
//! クリックか Enter で、選んだものを貼り付ける（[`crate::actions`] が行う）。左右の矢印キーと
//! マウスのホイールでページを移る。Esc か、ほかの場所をクリックすると何もせずに閉じる。
//! Shift+Delete で、選んでいる 1 件を履歴から消す。
//!
//! 実行スレッド（[`crate::actions`]）で、閉じるまで専用のメッセージループを回す。
//! ページ分け・絞り込み・表示する文の整え方は [`crate::clip_history`] にあり、Linux でもテストできる。

use std::cell::RefCell;
use std::time::Duration;

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    GetDC, GetMonitorInfoW, GetSysColor, GetTextExtentPoint32W, InvalidateRect, MonitorFromPoint,
    MonitorFromWindow, ReleaseDC, SelectObject, SetBkMode, SetTextColor, COLOR_BTNFACE,
    COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_INFOBK, COLOR_INFOTEXT,
    COLOR_WINDOW, COLOR_WINDOWTEXT, DRAW_TEXT_FORMAT, DT_CALCRECT, DT_END_ELLIPSIS, DT_EXPANDTABS,
    DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, HBRUSH, HDC, HFONT,
    HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, EM_SETCUEBANNER, ODS_SELECTED};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_DELETE, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN,
    VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos,
    GetDlgItem, GetForegroundWindow, GetGUIThreadInfo, GetMessageW, GetWindowRect,
    GetWindowThreadProcessId, IsChild, PostQuitMessage, RegisterClassW, SendMessageW,
    SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, GUITHREADINFO, HWND_TOPMOST,
    LB_GETITEMRECT, LB_ITEMFROMPOINT, LB_SETCOUNT, LB_SETCURSEL, LB_SETITEMHEIGHT, MA_NOACTIVATE,
    MSG, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WINDOW_STYLE, WM_ACTIVATE, WM_CLOSE, WM_COMMAND,
    WM_DRAWITEM, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_MOVE,
    WM_PAINT, WM_QUIT, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_SYSMENU,
};

use crate::clip_history::{self, MenuPosition, PAGE_SIZE};
use crate::ime_indicator;
use crate::ui::{self, item, set_text, wide, Item, Kind};

const ID_SEARCH: i32 = 1;
const ID_PREV: i32 = 2;
const ID_PAGE: i32 = 3;
const ID_NEXT: i32 = 4;
const ID_LIST: i32 = 5;

/// 一覧の 1 行の高さ（96 DPI 基準）。
const ROW_H: i32 = 20;
/// 画面の中身の大きさ（96 DPI 基準）。1 ページ分の行がちょうど入る高さにする。
const CLIENT_W: i32 = 300;
const LIST_Y: i32 = 56;
const LIST_H: i32 = ROW_H * PAGE_SIZE as i32 + 4;
const CLIENT_H: i32 = LIST_Y + LIST_H + 4;
/// 番号の欄と、行数の欄の幅（96 DPI 基準）。
const NUMBER_W: i32 = 26;
const LINES_W: i32 = 34;
const PAD: i32 = 5;
/// 全文の吹き出しの幅の上限と高さの上限（96 DPI 基準）。
const TIP_MAX_W: i32 = 380;
const TIP_MAX_H: i32 = 340;
const TIP_PAD: i32 = 6;
/// 検索の欄が書き換わった（EN_CHANGE）、ボタンが押された（BN_CLICKED）。
const EN_CHANGE: u32 = 0x0300;
const BN_CLICKED: u32 = 0;
/// 画面の枠。小さなタイトルバー（ドラッグで動かせる）と閉じるボタンを付ける。タイトルバーの
/// 無い画面は、環境によってはキー入力を受け取れないことがあるため。
const WINDOW_FRAME: WINDOW_STYLE = WINDOW_STYLE(WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0);
/// 前面にするまで待つ時間。
const SHOW_WAIT: Duration = Duration::from_millis(30);

const ITEMS: &[Item] = &[
    item(ID_SEARCH, Kind::Edit, "", 4, 4, 292, 24),
    item(ID_PREV, Kind::Button, "◀", 4, 32, 28, 22),
    item(ID_PAGE, Kind::Label, "", 38, 32, 224, 22),
    item(ID_NEXT, Kind::Button, "▶", 268, 32, 28, 22),
    item(ID_LIST, Kind::OwnerList, "", 4, LIST_Y, 292, LIST_H),
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
    /// 選んでいる位置（`visible` の中の位置）。
    selected: Option<usize>,
    /// 出しているページ（0 から）。
    page: usize,
    filtered: bool,
    dpi: u32,
    font: HFONT,
    /// 全文の吹き出しと、そこに出している文。
    tip: HWND,
    tip_text: String,
    outcome: Outcome,
    done: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.borrow_mut().as_mut().map(f))
}

/// 一覧の画面を出し、閉じるまで待つ。画面を前面にできなかったときは `None`
/// （前面にできないまま出すと、キーで操作できず、閉じられなくなるため）。
pub unsafe fn choose(items: Vec<String>, position: MenuPosition, target: HWND) -> Option<Outcome> {
    let anchor = anchor_point(target, position);
    let monitor = MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST);
    let mut dpi = 96;
    let mut dpi_y = 96;
    if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y).is_err() {
        dpi = 96;
    }

    let hwnd = create_window()?;
    let tip = create_tip_window(hwnd);
    let font = ui::create_ui_font(dpi);
    let count = items.len();
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            visible: (0..count).collect(),
            items,
            selected: (count > 0).then_some(0),
            page: 0,
            filtered: false,
            dpi,
            font,
            tip,
            tip_text: String::new(),
            outcome: Outcome::default(),
            done: false,
        })
    });
    ui::create_controls(hwnd, ITEMS, font);
    ui::layout(hwnd, ITEMS, dpi);
    let list = control(hwnd, ID_LIST);
    SendMessageW(list, LB_SETITEMHEIGHT, WPARAM(0), LPARAM(ui::scale(ROW_H, dpi) as isize));
    let cue = wide("検索（Enter で貼り付け・Esc で閉じる）");
    SendMessageW(
        control(hwnd, ID_SEARCH),
        EM_SETCUEBANNER,
        WPARAM(1),
        LPARAM(cue.as_ptr() as isize),
    );

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
    show_page(hwnd);

    run_loop(hwnd);
    let outcome = with_state(|st| std::mem::take(&mut st.outcome));
    close(hwnd);
    outcome
}

/// 画面を閉じ、状態を片付ける。
unsafe fn close(hwnd: HWND) {
    if let Some(state) = STATE.with(|s| s.borrow_mut().take()) {
        let _ = DestroyWindow(state.tip);
        let _ = DeleteObject(state.font);
    }
    let _ = DestroyWindow(hwnd);
}

/// 閉じるまでメッセージを処理する。キー操作は、検索の欄に入力しながらでも一覧を動かせる
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
        if ours && msg.message == WM_MOUSEWHEEL {
            // ホイールはページを移る（手前に回すと次のページ）。
            let delta = ((msg.wParam.0 >> 16) & 0xFFFF) as u16 as i16;
            turn_page(hwnd, if delta < 0 { 1 } else { -1 });
            continue;
        }
        if msg.hwnd == list && msg.message == WM_MOUSEMOVE {
            // マウスを乗せた行を選ぶ。
            if let Some(row) = row_at(list, msg.lParam) {
                let page_start = with_state(|st| st.page * PAGE_SIZE).unwrap_or(0);
                select(hwnd, page_start + row);
            }
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
        if msg.hwnd == list && msg.message == WM_LBUTTONUP && row_at(list, msg.lParam).is_some() {
            choose_current();
        }
    }
}

fn is_done() -> bool {
    STATE.with(|s| s.borrow().as_ref().is_none_or(|st| st.done))
}

/// 選んで（または選ばずに）閉じる。
fn finish(chosen: Option<String>) {
    with_state(|st| {
        st.outcome.chosen = chosen;
        st.done = true;
    });
}

/// キー操作。処理したら `true`（検索の欄には渡さない）。
unsafe fn on_key(hwnd: HWND, vk: u16) -> bool {
    let Some((current, count)) = with_state(|st| (st.selected, st.visible.len())) else {
        return false;
    };
    let current = current.map_or(-1, |c| c as isize);
    let move_to = |pos: isize| {
        if count > 0 {
            select(hwnd, pos.clamp(0, count as isize - 1) as usize);
        }
    };
    match vk {
        v if v == VK_ESCAPE.0 => finish(None),
        v if v == VK_RETURN.0 => choose_current(),
        v if v == VK_UP.0 => move_to(current - 1),
        v if v == VK_DOWN.0 => move_to(current + 1),
        v if v == VK_PRIOR.0 || v == VK_LEFT.0 => turn_page(hwnd, -1),
        v if v == VK_NEXT.0 || v == VK_RIGHT.0 => turn_page(hwnd, 1),
        v if v == VK_DELETE.0 && GetKeyState(VK_SHIFT.0 as i32) < 0 => remove_current(hwnd),
        _ => return false,
    }
    true
}

/// 前後のページへ移る（ページの中での行の位置はそのまま。足りなければ最後の行）。
unsafe fn turn_page(hwnd: HWND, delta: isize) {
    let Some((page, row, count)) = with_state(|st| {
        let row = st.selected.map_or(0, |s| s % PAGE_SIZE);
        (st.page, row, st.visible.len())
    }) else {
        return;
    };
    let pages = clip_history::page_count(count) as isize;
    let next = page as isize + delta;
    if count == 0 || next < 0 || next >= pages {
        return;
    }
    let next = next as usize;
    let range = clip_history::page_range(next, count);
    let pos = (range.start + row).min(range.end - 1);
    select(hwnd, pos);
}

/// 選んでいるものを貼り付けるために閉じる。
fn choose_current() {
    let chosen = with_state(|st| {
        st.selected
            .and_then(|pos| st.visible.get(pos))
            .and_then(|&i| st.items.get(i).cloned())
    })
    .flatten();
    if chosen.is_some() {
        finish(chosen);
    }
}

/// 選んでいる 1 件を履歴から消す。
unsafe fn remove_current(hwnd: HWND) {
    let removed = with_state(|st| {
        let pos = st.selected?;
        let index = *st.visible.get(pos)?;
        let text = st.items.remove(index);
        st.outcome.removed.push(text);
        Some(pos)
    })
    .flatten();
    if let Some(pos) = removed {
        refilter(hwnd, pos);
    }
}

/// 検索をやり直す。`keep` の位置を選ぶ（無ければ最後）。
unsafe fn refilter(hwnd: HWND, keep: usize) {
    let query = ui::get_text(hwnd, ID_SEARCH);
    with_state(|st| {
        st.visible = clip_history::filter(&st.items, &query);
        st.filtered = !query.trim().is_empty();
        let count = st.visible.len();
        st.selected = (count > 0).then(|| keep.min(count - 1));
        st.page = st.selected.map_or(0, |s| s / PAGE_SIZE);
    });
    show_page(hwnd);
}

/// 位置 `pos` を選ぶ。別のページなら、そのページを出す。
unsafe fn select(hwnd: HWND, pos: usize) {
    let Some(changed_page) = with_state(|st| {
        if st.selected == Some(pos) && st.page == pos / PAGE_SIZE {
            return None;
        }
        st.selected = Some(pos);
        let page = pos / PAGE_SIZE;
        let changed = page != st.page;
        st.page = page;
        Some(changed)
    })
    .flatten() else {
        return;
    };
    if changed_page {
        show_page(hwnd);
    } else {
        let row = pos % PAGE_SIZE;
        SendMessageW(control(hwnd, ID_LIST), LB_SETCURSEL, WPARAM(row), LPARAM(0));
        update_tip(hwnd);
    }
}

/// 今のページを一覧に出し、見出しとボタンを合わせる。
unsafe fn show_page(hwnd: HWND) {
    let Some((page, count, selected, filtered)) =
        with_state(|st| (st.page, st.visible.len(), st.selected, st.filtered))
    else {
        return;
    };
    let range = clip_history::page_range(page, count);
    let list = control(hwnd, ID_LIST);
    SendMessageW(list, LB_SETCOUNT, WPARAM(range.len()), LPARAM(0));
    let row = selected.filter(|s| range.contains(s)).map(|s| s - range.start);
    SendMessageW(list, LB_SETCURSEL, WPARAM(row.map_or(usize::MAX, |r| r)), LPARAM(0));
    let _ = InvalidateRect(list, None, true);
    set_text(hwnd, ID_PAGE, &clip_history::page_label(page, count, filtered));
    ui::set_enabled(hwnd, ID_PREV, page > 0);
    ui::set_enabled(hwnd, ID_NEXT, page + 1 < clip_history::page_count(count));
    update_tip(hwnd);
}

/// 一覧の中の位置（`lparam` はマウスの位置）にある行（ページの中で 0 から）。行の外なら `None`。
unsafe fn row_at(list: HWND, lparam: LPARAM) -> Option<usize> {
    let result = SendMessageW(list, LB_ITEMFROMPOINT, WPARAM(0), lparam).0 as usize;
    // 上位の 16 ビットが 1 なら、行の外。
    if (result >> 16) & 0xFFFF != 0 {
        return None;
    }
    let row = result & 0xFFFF;
    let on_page = with_state(|st| clip_history::page_range(st.page, st.visible.len()).len())?;
    (row < on_page).then_some(row)
}

unsafe fn control(hwnd: HWND, id: i32) -> HWND {
    GetDlgItem(hwnd, id).unwrap_or_default()
}

/// ページの中の行 `row` に出すもの: (番号, 最初の行, 行数)。
fn row_content(row: usize) -> Option<(usize, String, usize)> {
    with_state(|st| {
        let index = *st.visible.get(st.page * PAGE_SIZE + row)?;
        let (text, lines) = clip_history::row_text(st.items.get(index)?);
        // 番号は、履歴の中での位置（新しいものから 1, 2, …）。絞り込んでも変わらない。
        Some((index + 1, text, lines))
    })
    .flatten()
}

/// 1 行の中の、文字を書く範囲（番号と行数の欄を除いたところ）。
fn text_rect(rect: &RECT, lines: usize, dpi: u32) -> RECT {
    let right = if lines > 1 {
        rect.right - ui::scale(LINES_W + PAD, dpi)
    } else {
        rect.right - ui::scale(PAD, dpi)
    };
    RECT {
        left: rect.left + ui::scale(NUMBER_W + PAD * 2, dpi),
        right,
        ..*rect
    }
}

/// 1 行を描く: 番号・最初の行・行数。1 行おきに背景の色を少し変える。
unsafe fn draw_row(item: &DRAWITEMSTRUCT) {
    let Ok(row) = usize::try_from(item.itemID) else {
        return;
    };
    let Some((number, text, lines)) = row_content(row) else {
        return;
    };
    let Some((dpi, font)) = with_state(|st| (st.dpi, st.font)) else {
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
    SetTextColor(hdc, COLORREF(sub));
    let mut number_rect = RECT {
        right: rect.left + ui::scale(NUMBER_W, dpi),
        ..rect
    };
    draw_line(hdc, &number.to_string(), &mut number_rect, DT_RIGHT);
    if lines > 1 {
        let mut lines_rect = RECT {
            left: rect.right - ui::scale(LINES_W + PAD, dpi),
            right: rect.right - ui::scale(PAD, dpi),
            ..rect
        };
        draw_line(hdc, &format!("{lines}行"), &mut lines_rect, DT_RIGHT);
    }
    SetTextColor(hdc, COLORREF(main));
    let mut body = text_rect(&rect, lines, dpi);
    draw_line(hdc, &text, &mut body, DT_LEFT | DT_END_ELLIPSIS);
    SelectObject(hdc, old_font);
}

unsafe fn draw_line(hdc: HDC, text: &str, rect: &mut RECT, align: DRAW_TEXT_FORMAT) {
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

/// 選んでいるものが 1 行に収まらない（複数行か、途中で切れている）ときは、一覧の横に全文の
/// 吹き出しを出す。収まるときは隠す。
unsafe fn update_tip(hwnd: HWND) {
    let Some((tip, dpi, font, selected, page)) =
        with_state(|st| (st.tip, st.dpi, st.font, st.selected, st.page))
    else {
        return;
    };
    if tip.is_invalid() {
        return;
    }
    let list = control(hwnd, ID_LIST);
    let row = selected.and_then(|s| s.checked_sub(page * PAGE_SIZE)).filter(|r| *r < PAGE_SIZE);
    let full = with_state(|st| {
        selected
            .and_then(|s| st.visible.get(s))
            .and_then(|&i| st.items.get(i).cloned())
    })
    .flatten();
    let (Some(row), Some(full)) = (row, full) else {
        let _ = ShowWindow(tip, SW_HIDE);
        return;
    };
    let (first, lines) = clip_history::row_text(&full);

    // 一覧の行の、文字を書く幅に収まるか。
    let mut item_rect = RECT::default();
    SendMessageW(
        list,
        LB_GETITEMRECT,
        WPARAM(row),
        LPARAM(&mut item_rect as *mut RECT as isize),
    );
    let available = text_rect(&item_rect, lines, dpi);
    let hdc = GetDC(list);
    let old = SelectObject(hdc, font);
    let units: Vec<u16> = first.encode_utf16().collect();
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &units, &mut size);
    SelectObject(hdc, old);
    ReleaseDC(list, hdc);
    if lines <= 1 && size.cx <= available.right - available.left {
        let _ = ShowWindow(tip, SW_HIDE);
        return;
    }

    // 吹き出しの大きさを、中身に合わせて決める（上限あり）。
    let text = clip_history::tip_text(&full);
    let pad = ui::scale(TIP_PAD, dpi);
    let mut calc = RECT {
        left: 0,
        top: 0,
        right: ui::scale(TIP_MAX_W, dpi) - pad * 2,
        bottom: 0,
    };
    let hdc = GetDC(tip);
    let old = SelectObject(hdc, font);
    let mut units: Vec<u16> = text.encode_utf16().collect();
    DrawTextW(
        hdc,
        &mut units,
        &mut calc,
        DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX | DT_EXPANDTABS,
    );
    SelectObject(hdc, old);
    ReleaseDC(tip, hdc);
    let width = (calc.right - calc.left) + pad * 2 + 2;
    let height = ((calc.bottom - calc.top) + pad * 2 + 2).min(ui::scale(TIP_MAX_H, dpi));

    // 置く場所: 画面の右（入らなければ左）、選んでいる行の高さ。
    let mut window = RECT::default();
    let _ = GetWindowRect(hwnd, &mut window);
    let mut top_left = POINT {
        x: item_rect.left,
        y: item_rect.top,
    };
    let _ = ClientToScreen(list, &mut top_left);
    let work = work_area(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST));
    let gap = ui::scale(4, dpi);
    let mut x = window.right + gap;
    if x + width > work.right {
        x = window.left - gap - width;
    }
    let y = top_left.y.clamp(work.top, (work.bottom - height).max(work.top));
    with_state(|st| st.tip_text = text);
    let _ = SetWindowPos(
        tip,
        HWND_TOPMOST,
        x.max(work.left),
        y,
        width,
        height,
        SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
    let _ = InvalidateRect(tip, None, true);
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
                (ID_SEARCH, EN_CHANGE) => refilter(hwnd, 0),
                (ID_PREV | ID_NEXT, BN_CLICKED) => {
                    turn_page(hwnd, if id == ID_PREV { -1 } else { 1 });
                    // 押したあとも、続けて検索の文字を打てるようにする。
                    let _ = SetFocus(control(hwnd, ID_SEARCH));
                }
                _ => {}
            }
            LRESULT(0)
        }
        // 画面を動かしたら、吹き出しも付いていく。
        WM_MOVE => {
            if STATE.with(|s| s.borrow().is_some()) {
                update_tip(hwnd);
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

/// 全文の吹き出しのプロシージャ（描くだけ。押されても前面にならない）。
unsafe extern "system" fn tip_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            fill(hdc, &rect, GetSysColor(COLOR_INFOBK));
            if let Some((text, font, dpi)) = with_state(|st| (st.tip_text.clone(), st.font, st.dpi)) {
                let old = SelectObject(hdc, font);
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(GetSysColor(COLOR_INFOTEXT)));
                let pad = ui::scale(TIP_PAD, dpi);
                let mut inner = RECT {
                    left: rect.left + pad,
                    top: rect.top + pad,
                    right: rect.right - pad,
                    bottom: rect.bottom - pad,
                };
                let mut units: Vec<u16> = text.encode_utf16().collect();
                DrawTextW(
                    hdc,
                    &mut units,
                    &mut inner,
                    DT_WORDBREAK | DT_NOPREFIX | DT_EXPANDTABS | DT_END_ELLIPSIS,
                );
                SelectObject(hdc, old);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
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

/// 全文の吹き出しを作る（まだ表示しない）。作れなければ無効なハンドル（吹き出しなしで動く）。
unsafe fn create_tip_window(owner: HWND) -> HWND {
    let Ok(instance) = GetModuleHandleW(None) else {
        return HWND::default();
    };
    let class_name = w!("AtaiPasteHistoryTip");
    let class = WNDCLASSW {
        lpfnWndProc: Some(tip_proc),
        hInstance: instance.into(),
        lpszClassName: class_name,
        ..Default::default()
    };
    RegisterClassW(&class);
    CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
        class_name,
        w!(""),
        WS_POPUP | WS_BORDER,
        0,
        0,
        0,
        0,
        owner,
        None,
        instance,
        None,
    )
    .unwrap_or_default()
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

unsafe fn work_area(monitor: HMONITOR) -> RECT {
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
