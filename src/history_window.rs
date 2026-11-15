//! クリップボードの履歴と定型文の一覧（Clibor のような、小さな半透明の一覧の画面）。
//!
//! 一覧を出す操作をすると、入力位置（またはマウス）の近くに、1 ピクセルの枠だけの細長い画面を出す。
//!
//! - 上: タブ（「履歴」「定型文」）と、ページ（`1〜20 / 1000`。絞り込んでいるときは
//!   `1〜5 / 該当 5`）。左右の矢印キーかクリックでタブを切り替える。その下の段に、検索の欄。
//! - 一覧: 1 ページに決めた件数（既定 20 件）。1 行に 1 件、番号・最初の行（定型文は名前）・行数を
//!   出し、1 行おきに色を変える。マウスを乗せた行・矢印キーで選んだ行が選ばれる。
//! - 選んだものが 1 行に収まらないとき（定型文はいつも）、選んだ行の横に全文の吹き出しを出す。
//!
//! 色は配色の設定（[`clip_history::Theme`]）に従い、画面は半透明（一覧の上で Shift+ホイールでも
//! 変えられる）。クリックか Enter で、選んだものを貼り付ける（[`crate::actions`] が行う）。
//! PageUp / PageDown・ホイールでページを移る。Esc か、ほかの場所をクリックすると何もせずに閉じる。
//! 履歴は Delete で 1 件を消せる。右クリック（またはアプリケーションキー）で、定型文への登録・
//! 削除・定型文の編集のメニューを出す。メニューの「整えて貼り付け」では、文字（履歴の文字と
//! 定型文）を整えてから貼り付ける（前後の空白を除く・1 行にまとめる・大文字・小文字・全角英数を
//! 半角に。履歴そのものは変えない）。画像の行では「画像をファイルに保存...」で PNG に書き出せる。
//!
//! 検索: Ctrl+F か `/`（または検索の欄をクリック）で、検索の欄に入力欄を出す。入力した言葉で、
//! 今のタブを絞り込む（大文字と小文字は区別しない。空白で区切ると、すべてを含むもの。定型文は
//! 名前と本文、画像は一覧に出す文で探す）。入力している間も、上下・PageUp / PageDown で選び、
//! Enter で貼り付けられる。Esc の 1 回目で検索をやめ（言葉も消す）、2 回目で閉じる。
//! 履歴のタブでは、検索の欄の左に出す種類（すべて・文字だけ・画像だけ）を出し、`F` キーか
//! クリックで切り替える（検索と組み合わせられる。出したままにして出し直しても保つ）。
//! 絞り込んだ一覧の位置は、元の一覧の位置に置き換えてから消す・ピン留め・貼り付けをする。
//!
//! まとめて貼り付け: Space で選んでいる行に印を付ける・外す（印を付けた順番を、番号の所に色の
//! 付いた札で出す。タブをまたいで付けられる）。印があるときは、Enter・行のクリックで、印を付けた
//! ものを付けた順に貼り付ける（続く文字は改行でつないで 1 回で、画像はその位置で 1 枚ずつ）。
//!
//! 一覧は前面（アクティブ）にしない。前面にすると、元のアプリの入力欄からカーソルが外れて
//! 貼り付けられなくなることがあるため。キー操作はキーボードフック（[`hook_key`]）から、一覧の外の
//! クリックとホイールはマウスフックから受け取る。ただし、右クリックのメニューを出している間と、
//! 検索の欄に入力している間（日本語入力を使えるように）は前面にし、キーはそのまま一覧に届ける。
//! 閉じるとき・検索をやめるときに、元のアプリに前面を戻す。
//!
//! 実行スレッド（[`crate::actions`]）で、閉じるまで専用のメッセージループを回す。
//! ページ分けと表示する文の整え方は [`crate::clip_history`] / [`crate::snippets`] にあり、
//! Linux でもテストできる。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    GetDC, GetMonitorInfoW, GetSysColor, GetTextExtentPoint32W, InvalidateRect, MonitorFromPoint,
    MonitorFromWindow, ReleaseDC, SelectObject, SetBkColor, SetBkMode, SetTextColor, UpdateWindow,
    COLOR_BTNFACE,
    COLOR_BTNSHADOW, COLOR_BTNTEXT, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT,
    COLOR_INFOBK, COLOR_INFOTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DRAW_TEXT_FORMAT, DT_CALCRECT,
    DT_CENTER, DT_END_ELLIPSIS, DT_EXPANDTABS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE,
    DT_EDITCONTROL, DT_VCENTER, DT_WORDBREAK, HBRUSH, HDC, HFONT, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::{GetLocalTime, GetTickCount};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, EM_SETSEL, ODS_SELECTED};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::Ime::ImmAssociateContextEx;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SetFocus, VK_APPS, VK_CONTROL, VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END,
    VK_ESCAPE, VK_F10, VK_HOME, VK_LEFT, VK_NEXT, VK_OEM_2, VK_PRIOR, VK_RETURN, VK_RIGHT,
    VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetClientRect, GetCursorPos, GetDlgItem, GetForegroundWindow,
    GetGUIThreadInfo, GetMessageW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsChild, LoadCursorW, SetWindowTextW, EN_CHANGE, ES_AUTOHSCROLL,
    HWND_TOP, IDC_ARROW, WM_COMMAND, WM_CTLCOLOREDIT, WM_RBUTTONDOWN, WS_CLIPCHILDREN,
    PostQuitMessage, RegisterClassW, SendMessageW, SetForegroundWindow, SetLayeredWindowAttributes,
    SetWindowPos, ShowWindow, TrackPopupMenuEx, TranslateMessage, GUITHREADINFO, HMENU,
    HWND_TOPMOST, LB_GETITEMRECT, LB_ITEMFROMPOINT, LB_SETCOUNT, LB_SETCURSEL, LB_SETITEMHEIGHT,
    LWA_ALPHA, MA_NOACTIVATE, MF_CHECKED, MF_GRAYED, MF_POPUP, WM_SETFONT, MF_SEPARATOR, MF_STRING, MSG, SWP_NOACTIVATE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_TOPALIGN,
    PostMessageW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_LBUTTONDBLCLK, WM_TIMER, IsWindowVisible,
    SetTimer, KillTimer, WM_CLOSE, WM_CTLCOLORLISTBOX,
    WM_DRAWITEM, WM_NULL,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_PAINT, WM_QUIT, WM_RBUTTONUP, WM_SYSKEYDOWN, WNDCLASSW, WS_BORDER, WS_CHILD,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::clip_history::{self, GateState, KeyGate, KeyRoute, Marks, MenuPosition, Palette, Theme};
use crate::config::Snippet;
use crate::clip_images::{self, Item, KindFilter, Thumb};
use crate::ui;
use crate::text_transform::{self, QuickFormat};
use crate::{clip_image_store, ime_indicator, snippets};

const ID_HEADER: i32 = 1;
const ID_LIST: i32 = 2;
/// 検索の入力欄（見出しの子）。
const ID_SEARCH: i32 = 3;

/// 一覧の 1 行の高さと、見出しの高さ（96 DPI 基準）。見出しは 2 段で、上の段にタブとページ、
/// 下の段に検索の欄を置く。
const ROW_H: i32 = 18;
const HEADER_H: i32 = 20;
const FILTER_H: i32 = 20;
/// 番号の欄と、行数の欄の幅（96 DPI 基準）。
const NUMBER_W: i32 = 28;
const LINES_W: i32 = 30;
const PAD: i32 = 4;
/// タブの左右の余白（96 DPI 基準）。
const TAB_PAD: i32 = 10;
/// 外枠の太さ（実際のピクセル。拡大率にかかわらず 1 ピクセル）。
const BORDER: i32 = 1;
/// Shift+ホイール 1 目盛りで変える不透明度（%）。
const OPACITY_STEP: u32 = 5;
/// 全文の吹き出しの幅の上限と高さの上限（96 DPI 基準）。
const TIP_MAX_W: i32 = 380;
const TIP_MAX_H: i32 = 340;
const TIP_PAD: i32 = 6;
/// 画面の形（タイトルバーも枠も無い。1 ピクセルの枠は自分で描く）。
const WINDOW_FRAME: WINDOW_STYLE = WS_POPUP;
/// 一覧は前面（アクティブ）にしない。前面にすると、元のアプリの入力欄からカーソルが外れて
/// 貼り付けられなくなることや、元のアプリが前面を取り返して一覧がすぐ閉じることがあるため。
/// キー操作は、キーボードフックから受け取る（[`hook_key`]）。
const WINDOW_EX: WINDOW_EX_STYLE = WINDOW_EX_STYLE(
    WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0 | WS_EX_LAYERED.0 | WS_EX_NOACTIVATE.0,
);
/// 表示が追いつくまで待つ時間。
const SHOW_WAIT: Duration = Duration::from_millis(30);
/// キーボードフックが知らせるキー（wParam = 仮想キーコード）と、マウスフックが知らせるホイール
/// （wParam = 回した量。奥が正）。
const WM_APP_KEY: u32 = WM_APP + 21;
const WM_APP_WHEEL: u32 = WM_APP + 22;
/// 元のアプリが前面でなくなったかを調べるタイマーと、その間隔。
const TIMER_WATCH: usize = 1;
const WATCH_INTERVAL_MS: u32 = 100;
/// 前面が別のウィンドウに移ったとみなすまでの回数（一瞬だけ別のものが前面になっても閉じない）。
const WATCH_STRIKES: u32 = 3;

/// 出している一覧と吹き出しのウィンドウ（フックから見る。出していなければ 0）。
static POPUP: AtomicIsize = AtomicIsize::new(0);
static TIP: AtomicIsize = AtomicIsize::new(0);
/// 右クリックのメニューを出している（その間は、キーをメニューに渡す）。
static MENU_OPEN: AtomicBool = AtomicBool::new(false);
/// 検索の入力欄に入力している（その間は一覧を前面にしているので、キーは握りつぶさずに
/// 入力欄に届ける。日本語入力も使える）。
static SEARCHING: AtomicBool = AtomicBool::new(false);
/// 利用者が Ctrl を押しているか（押している間は、貼り付けても一覧を閉じない）。
static CTRL_HELD: AtomicBool = AtomicBool::new(false);
/// 貼り付けのあとで一覧を出し直すところ（その間に押されたキーは、出し直した一覧に渡す）。
static REOPENING: AtomicBool = AtomicBool::new(false);
/// 一覧の外を押して閉じた（このときは、元のウィンドウに戻さない）。
static CLICKED_OUTSIDE: AtomicBool = AtomicBool::new(false);
/// 一覧に渡したキーと、握りつぶしたキー（離したときも握りつぶすため）。[`POPUP`] を変えるときも
/// これを握り、キーボードフックが見る一覧と食い違わないようにする。
static KEYS: Mutex<KeyGate> = Mutex::new(KeyGate::new());

fn keys() -> std::sync::MutexGuard<'static, KeyGate> {
    KEYS.lock().unwrap_or_else(|p| p.into_inner())
}

/// キーボードフックから呼ぶ。一覧を出している間のキーを一覧に渡す。渡した（元のアプリには
/// 届けない）なら `true`。Shift・Ctrl などの修飾キーそのものと、Alt・Win を押しながらのキー
/// （Alt+Tab など）は、そのまま通す。
///
/// 一覧が握りつぶしたキーは、一覧が閉じたあとも、離すまで握りつぶす（押したままの Enter が
/// 元のアプリに届かないように）。貼り付けて一覧を出し直すまでの間に押されたキーは、出し直した
/// 一覧に渡す。
pub fn hook_key(vk: u32, down: bool) -> bool {
    // Ctrl を押しているかは、ここで覚える（貼り付けのために Ctrl+V を送ると、Windows から見た
    // Ctrl の状態が変わるので、あとから調べても分からない）。0x11 = Ctrl、0xA2 / 0xA3 = 左右の Ctrl
    if matches!(vk, 0x11 | 0xA2 | 0xA3) {
        CTRL_HELD.store(down, Ordering::SeqCst);
    }
    const MODIFIERS: [u32; 11] = [0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5];
    if MODIFIERS.contains(&vk) {
        return false;
    }
    // 0x12 = Alt、0x5B / 0x5C = Win
    let alt_or_win =
        down && [0x12, 0x5B, 0x5C].iter().any(|key| unsafe { GetAsyncKeyState(*key) } < 0);
    let now = unsafe { GetTickCount() };
    let mut gate = keys();
    let popup = POPUP.load(Ordering::SeqCst);
    let state = GateState {
        shown: popup != 0,
        reopening: REOPENING.load(Ordering::SeqCst) && OPEN.load(Ordering::SeqCst),
        // 検索の入力欄に入力している間も、メニューと同じく、キーは一覧（前面）にそのまま届ける。
        menu_open: MENU_OPEN.load(Ordering::SeqCst) || SEARCHING.load(Ordering::SeqCst),
        alt_or_win,
    };
    match gate.on_key(vk, down, now, state) {
        KeyRoute::Pass => false,
        KeyRoute::Swallow | KeyRoute::Queue => true,
        KeyRoute::Forward => {
            unsafe {
                let _ = PostMessageW(
                    HWND(popup as *mut core::ffi::c_void),
                    WM_APP_KEY,
                    WPARAM(vk as usize),
                    LPARAM(0),
                );
            }
            true
        }
    }
}

/// マウスフックから呼ぶ（ボタンを押したとき）。一覧の外を押したら、一覧を閉じる。
pub fn hook_mouse_down(x: i32, y: i32) {
    let popup = POPUP.load(Ordering::SeqCst);
    if popup == 0 || MENU_OPEN.load(Ordering::SeqCst) {
        return;
    }
    if point_in_window(popup, x, y) || point_in_window(TIP.load(Ordering::SeqCst), x, y) {
        return;
    }
    // 押したウィンドウを使いたいので、閉じたあとで元のウィンドウに戻さない。
    CLICKED_OUTSIDE.store(true, Ordering::SeqCst);
    CLOSE_REQUESTED.store(true, Ordering::SeqCst);
    unsafe {
        let _ = PostMessageW(
            HWND(popup as *mut core::ffi::c_void),
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// マウスフックから呼ぶ（ホイールを回したとき）。一覧の上なら一覧に渡す。渡したら `true`。
/// 前面でないウィンドウには、設定によってはホイールが届かないため、ここで受け取る。
pub fn hook_mouse_wheel(x: i32, y: i32, delta: i32) -> bool {
    let popup = POPUP.load(Ordering::SeqCst);
    if popup == 0 || MENU_OPEN.load(Ordering::SeqCst) || !point_in_window(popup, x, y) {
        return false;
    }
    unsafe {
        let _ = PostMessageW(
            HWND(popup as *mut core::ffi::c_void),
            WM_APP_WHEEL,
            WPARAM(delta as isize as usize),
            LPARAM(0),
        );
    }
    true
}

/// 画面の座標が、そのウィンドウ（見えているとき）の中か。
fn point_in_window(window: isize, x: i32, y: i32) -> bool {
    if window == 0 {
        return false;
    }
    let hwnd = HWND(window as *mut core::ffi::c_void);
    let mut rect = RECT::default();
    unsafe {
        IsWindowVisible(hwnd).as_bool()
            && GetWindowRect(hwnd, &mut rect).is_ok()
            && x >= rect.left
            && x < rect.right
            && y >= rect.top
            && y < rect.bottom
    }
}

/// 右クリックのメニューの項目。
const MENU_PASTE: usize = 1;
const MENU_REGISTER: usize = 2;
const MENU_DELETE: usize = 3;
const MENU_EDIT_SNIPPETS: usize = 4;
const MENU_CLOSE: usize = 5;
const MENU_PIN: usize = 6;
const MENU_KEEP: usize = 7;
const MENU_SEARCH: usize = 8;
const MENU_MARK: usize = 9;
const MENU_PASTE_MARKED: usize = 10;
const MENU_CLEAR_MARKS: usize = 11;
const MENU_SAVE_IMAGE: usize = 12;
/// 「整えて貼り付け」の項目（この番号に、[`text_transform::QUICK_FORMATS`] の位置を足す）。
const MENU_FORMAT_BASE: usize = 20;
/// ピン留めした行の左端に引く線の太さ（96 DPI 基準）。
const PIN_BAR_W: i32 = 3;

/// 画面の部品（位置と大きさは、幅と件数に合わせて [`layout`] で決める）。見出しは自分で描く
/// ウィンドウなので、ここには一覧だけを書く。
/// 帯（画面の下に出すとき）のカードの大きさ（96 DPI 基準）。
const CARD_W: i32 = 200;
const CARD_H: i32 = 132;
/// カードのまわりのすき間と、カードの中の余白。
const CARD_GAP: i32 = 4;
const CARD_PAD: i32 = 6;
/// 縦のパネル（画面の右端に出すとき）の幅の下限。
const SIDE_MIN_W: i32 = 300;
/// 読み込んだ縮小した絵を覚えておく枚数。
const THUMB_CACHE: usize = 80;
/// `LBS_MULTICOLUMN` と `LB_SETCOLUMNWIDTH`（帯では、一覧を横に並べる）。
const LBS_MULTICOLUMN: u32 = 0x0200;
const LB_SETCOLUMNWIDTH: u32 = 0x0195;

/// 画面の形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// 入力位置・マウスの近くの、小さな一覧。
    List,
    /// 画面の右端の、縦のパネル。
    Side,
    /// 画面の下の、横長の帯。
    Band,
}

impl Layout {
    fn of(position: MenuPosition) -> Layout {
        match position {
            MenuPosition::Caret | MenuPosition::Mouse => Layout::List,
            MenuPosition::Side => Layout::Side,
            MenuPosition::Bottom => Layout::Band,
        }
    }
}

/// 画面の出し方（設定の値）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub position: MenuPosition,
    /// 幅（96 DPI 基準）。
    pub width: u32,
    /// 不透明度（%）。
    pub opacity: u32,
    /// 1 ページの件数。
    pub page_size: usize,
    pub theme: Theme,
    /// 貼り付けても一覧を出したままにするか。
    pub keep_open: bool,
}

/// 選ばれたもの。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// 履歴（そのまま貼り付ける）。
    History(String),
    /// 定型文（{date} などを置き換えてから貼り付ける）。
    Snippet(String),
    /// 履歴の画像（番号）。
    Image(u64),
    /// 印を付けたもの（付けた順。中身は `History` / `Snippet` / `Image` だけ）。続く文字は改行で
    /// つないで 1 回で、画像はその位置で 1 枚ずつ貼り付ける。
    Many(Vec<Pick>),
    /// 整えて貼り付ける（中身は `History` か `Snippet`）。履歴そのものは変えない。
    Formatted(Box<Pick>, QuickFormat),
}

/// 一覧を閉じたときの、タブと選んでいた位置と絞り込み（出したままにするとき、同じ所から
/// 出し直す）。位置は、絞り込む前の一覧での位置。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resume {
    tab: usize,
    places: [(Option<usize>, usize); 2],
    /// 検索の言葉（出し直したときも、同じ言葉で絞り込んだまま出す）。
    query: String,
    /// 履歴のタブで出す種類。
    kind: KindFilter,
}

impl Resume {
    /// 出していたタブで選ぶ位置（絞り込む前の一覧での位置）を変える（貼り付けたものが、
    /// いちばん上に移ったとき）。
    pub fn with_selected(mut self, position: usize) -> Resume {
        self.places[self.tab % 2].0 = Some(position);
        self
    }
}

/// 一覧の画面の結果。
#[derive(Debug, Default)]
pub struct Outcome {
    /// 選ばれたもの（選ばずに閉じたら `None`）。
    pub chosen: Option<Pick>,
    /// 履歴から消したもの。
    pub removed: Vec<String>,
    /// 履歴から定型文に登録したもの。
    pub registered: Vec<String>,
    /// ピン留めを変えたもの（文字, ピン留めしたか）。同じものを何度も変えたら、最後のものが効く。
    pub pin_changes: Vec<(String, bool)>,
    /// 履歴から消した画像（番号）。
    pub removed_images: Vec<u64>,
    /// ピン留めを変えた画像（番号, ピン留めしたか）。
    pub image_pin_changes: Vec<(u64, bool)>,
    /// 「出したままにする」を切り替えた（切り替えなければ `None`）。
    pub keep_open: Option<bool>,
    /// Ctrl を押しながら選んだ（今回は、貼り付けても一覧を閉じない）。
    pub ctrl_held: bool,
    /// 閉じたときのタブと位置。
    pub resume: Resume,
    /// 「定型文の編集...」を選んだ。
    pub edit_snippets: bool,
    /// Shift+ホイールで変えた不透明度（変えなければ `None`）。
    pub opacity: Option<u32>,
    /// ほかのウィンドウをクリックして閉じた（このときは元のウィンドウに戻さない）。
    pub left_for_other_window: bool,
}

/// 一覧を出しているか。
static OPEN: AtomicBool = AtomicBool::new(false);
/// 閉じるよう頼まれたか（一覧を出す操作をもう一度したとき）。
static CLOSE_REQUESTED: AtomicBool = AtomicBool::new(false);

/// 一覧を出しているか（キーボードフックから呼ぶ）。
pub fn is_open() -> bool {
    OPEN.load(Ordering::SeqCst)
}

/// 一覧を出し始める（貼り付けて出し直すまでの間も、出しているとみなす）。
///
/// `ctrl_held` は、一覧を出したキーの Ctrl を、利用者がまだ押しているか（一覧を出す前に、
/// アプリが Ctrl の「離した」を送っているので、Windows に聞いても分からない）。
pub fn begin_session(ctrl_held: bool) {
    keys().prune(unsafe { GetTickCount() });
    // Ctrl を押しているかは、キーボードフックがいつも覚えている。ここで Windows に聞き直すと、
    // 2 回押しの 2 回目を離した知らせの途中（まだ「押している」と返る）に当たって、押したままと
    // 取り違えることがある。一覧を出したキーの Ctrl を押し続けているときだけ、ここで足す。
    if ctrl_held {
        CTRL_HELD.store(true, Ordering::SeqCst);
    }
    CLOSE_REQUESTED.store(false, Ordering::SeqCst);
    REOPENING.store(false, Ordering::SeqCst);
    OPEN.store(true, Ordering::SeqCst);
}

/// 一覧を出し終えた（もう出し直さない）。貼り付ける前に呼んでよい（貼り付けている間に押された
/// ほかのキー割り当てを、捨てずに実行するため）。
pub fn end_session() {
    let mut gate = keys();
    gate.session_ended();
    REOPENING.store(false, Ordering::SeqCst);
    OPEN.store(false, Ordering::SeqCst);
    CLOSE_REQUESTED.store(false, Ordering::SeqCst);
}

/// 貼り付けのあとで一覧を出し直す（その間に押されたキーは、元のアプリに届けず、出し直した
/// 一覧に渡す）。
pub fn set_reopening() {
    REOPENING.store(true, Ordering::SeqCst);
}

/// 利用者が Ctrl を押しているか（キーボードフックが見た、実際のキーの状態）。
pub fn ctrl_held() -> bool {
    CTRL_HELD.load(Ordering::SeqCst)
}

/// 閉じるよう頼まれているか（貼り付けている間に、一覧を出す操作をされた）。
pub fn close_requested() -> bool {
    CLOSE_REQUESTED.load(Ordering::SeqCst)
}

/// 出している一覧を閉じるよう頼む（一覧を出す操作をもう一度したとき。Clibor と同じく、
/// 出すキーで閉じられる）。実行スレッドを起こすのは呼び出し側。
pub fn request_close() {
    CLOSE_REQUESTED.store(true, Ordering::SeqCst);
}

/// タブ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    History,
    Snippets,
}

impl Tab {
    /// 見出しに並べる順と、その名前。
    const ALL: [(Tab, &'static str); 2] = [(Tab::History, "履歴"), (Tab::Snippets, "定型文")];

    fn index(self) -> usize {
        match self {
            Tab::History => 0,
            Tab::Snippets => 1,
        }
    }

    fn from_index(index: usize) -> Tab {
        Self::ALL[index % Self::ALL.len()].0
    }

    /// 隣のタブ（端まで行ったら反対の端へ）。
    fn step(self, delta: isize) -> Tab {
        let count = Self::ALL.len() as isize;
        Tab::from_index((self.index() as isize + delta).rem_euclid(count) as usize)
    }
}

/// 画面を開いている間の状態。
struct State {
    /// 履歴（文字と画像。ピン留めしたものが先、続けて新しい順）。一覧で消したものは除いていく。
    history: Vec<Item>,
    snippets: Vec<Snippet>,
    /// 読み込んだ縮小した絵（読めなかったものは `None`）。
    thumbs: HashMap<u64, Option<Rc<Thumb>>>,
    layout: Layout,
    keep_open: bool,
    /// 一覧を出したときに前面だったウィンドウ（貼り付け先）と、それが前面でなかった回数。
    target: HWND,
    away: u32,
    tab: Tab,
    /// タブごとの (選んでいる位置, 出しているページ)。位置は、絞り込んだ一覧（[`State::visible`]）
    /// での位置。
    places: [(Option<usize>, usize); 2],
    /// タブごとの、絞り込んだ一覧に出すものの、元の一覧での位置。
    visible: [Vec<usize>; 2],
    /// 検索の言葉。
    query: String,
    /// 履歴のタブで出す種類（すべて・文字だけ・画像だけ）。
    kind: KindFilter,
    /// 検索の入力欄に入力している（一覧を前面にしている）。
    searching: bool,
    /// 検索で探す文（大文字と小文字の違いをなくしたもの。最初に検索するときに作る）。
    folded: Option<[Vec<String>; 2]>,
    /// まとめて貼り付けるために印を付けたもの（タブの番号, 元の一覧での位置）。付けた順。
    marks: Marks,
    page_size: usize,
    opacity: u32,
    dpi: u32,
    font: HFONT,
    colors: Palette,
    /// 一覧の背景を塗るブラシ。
    back_brush: HBRUSH,
    /// 見出しの中のタブの範囲（クリックで切り替えるため）。
    tab_rects: [RECT; 2],
    /// 見出しの中の「出したまま」の範囲。
    keep_rect: RECT,
    /// 見出しの下の段の、出す種類の範囲（履歴のタブだけ）。
    kind_rect: RECT,
    /// 見出しの下の段の、検索の欄の範囲と、そこに置いた入力欄の範囲（置いていなければ空）。
    search_rect: RECT,
    edit_rect: RECT,
    /// 全文の吹き出しと、そこに出している文（画像のときは絵）。
    tip: HWND,
    tip_text: String,
    tip_image: Option<Rc<Thumb>>,
    /// 最後に見たマウスの位置（動いていないのに届く WM_MOUSEMOVE で選び直さないため）。
    last_mouse: Option<(i32, i32)>,
    /// 一覧の上でマウスのボタンを押したか（押したまま一覧に来て離しても貼り付けないため）。
    pressed_on_list: bool,
    /// ホイールの回した量の端数（細かく届くタッチパッドで、1 回に何ページも移らないため）。
    wheel: i32,
    outcome: Outcome,
    done: bool,
}

impl State {
    /// 今のタブの、絞り込んだ一覧の件数。
    fn len(&self) -> usize {
        self.visible[self.tab.index()].len()
    }

    /// 絞り込んだ一覧での位置 `pos` の、元の一覧（`history` / `snippets`）での位置。
    fn actual(&self, pos: usize) -> Option<usize> {
        self.visible[self.tab.index()].get(pos).copied()
    }

    /// 絞り込んでいるか（見出しに、該当の件数を出す）。
    fn filtered(&self) -> bool {
        !clip_history::query_terms(&self.query).is_empty()
            || (self.tab == Tab::History && self.kind != KindFilter::All)
    }

    /// 検索で探す文（大文字と小文字の違いをなくしたもの）。
    fn fold_all(&self) -> [Vec<String>; 2] {
        [
            self.history
                .iter()
                .map(|item| clip_history::fold_case(&clip_images::search_text(item)))
                .collect(),
            self.snippets.iter().map(snippet_haystack).collect(),
        ]
    }

    /// 絞り込んだ一覧を作り直す。
    fn rebuild(&mut self) {
        let terms = clip_history::query_terms(&self.query);
        if !terms.is_empty() && self.folded.is_none() {
            self.folded = Some(self.fold_all());
        }
        let hit = |list: usize, i: usize| {
            terms.is_empty()
                || self
                    .folded
                    .as_ref()
                    .and_then(|f| f[list].get(i))
                    .is_some_and(|text| clip_history::matches_terms(text, &terms))
        };
        let history: Vec<usize> = (0..self.history.len())
            .filter(|i| self.kind.accepts(&self.history[*i]) && hit(0, *i))
            .collect();
        let snippets: Vec<usize> = (0..self.snippets.len()).filter(|i| hit(1, *i)).collect();
        self.visible = [history, snippets];
    }

    /// 絞り込み直す。選んでいたものは、絞り込んだあとも残っていれば選んだままにする
    /// （残っていなければ、その次のもの）。
    fn refilter(&mut self) {
        let wanted =
            [0, 1].map(|i| self.places[i].0.and_then(|p| self.visible[i].get(p).copied()));
        self.rebuild();
        self.restore_places(wanted);
    }

    /// 元の一覧での位置 `wanted` を選ぶ（無ければ、その次のもの。`None` なら最初のもの）。
    fn restore_places(&mut self, wanted: [Option<usize>; 2]) {
        for (i, wanted) in wanted.into_iter().enumerate() {
            let visible = &self.visible[i];
            let selected = clip_history::nearest_position(visible, wanted.unwrap_or(0));
            self.places[i] = (selected, selected.map_or(0, |s| s / self.page_size));
        }
    }

    /// 閉じるときの位置（元の一覧での位置にする）。
    fn actual_places(&self) -> [(Option<usize>, usize); 2] {
        [0, 1].map(|i| {
            let (selected, page) = self.places[i];
            (selected.and_then(|p| self.visible[i].get(p).copied()), page)
        })
    }

    /// 位置 `index` が履歴の画像なら、その番号。
    fn image_id(&self, index: usize) -> Option<u64> {
        let actual = self.actual(index)?;
        match (self.tab, self.history.get(actual)?) {
            (Tab::History, Item::Image(entry)) => Some(entry.id),
            _ => None,
        }
    }

    /// 位置 `index` の画像の、縮小した絵。
    fn thumb_at(&mut self, index: usize) -> Option<Rc<Thumb>> {
        let id = self.image_id(index)?;
        self.thumb(id)
    }

    /// 縮小した絵（最初に使うときにファイルから読む）。
    fn thumb(&mut self, id: u64) -> Option<Rc<Thumb>> {
        if !self.thumbs.contains_key(&id) && self.thumbs.len() >= THUMB_CACHE {
            self.thumbs.clear();
        }
        self.thumbs
            .entry(id)
            .or_insert_with(|| clip_image_store::load_thumb(id).map(Rc::new))
            .clone()
    }

    fn selected(&self) -> Option<usize> {
        self.places[self.tab.index()].0
    }

    fn page(&self) -> usize {
        self.places[self.tab.index()].1
    }

    fn set_place(&mut self, selected: Option<usize>, page: usize) {
        self.places[self.tab.index()] = (selected, page);
    }

    /// 位置 `index` の行に出すもの。番号は、絞り込む前の一覧での番号。
    fn row(&self, index: usize) -> Option<Row> {
        let actual = self.actual(index)?;
        let mark = self.marks.order(self.tab.index(), actual);
        let (shown, lines, pinned) = match self.tab {
            Tab::History => match self.history.get(actual)? {
                Item::Text { text, pinned } => {
                    let (shown, lines) = clip_history::row_text(text);
                    (shown, lines, *pinned)
                }
                Item::Image(entry) => (clip_images::row_label(entry), 1, entry.pinned),
            },
            Tab::Snippets => {
                let snippet = self.snippets.get(actual)?;
                let lines = clip_history::row_text(&snippet.text).1;
                (snippets::display_name(snippet), lines, false)
            }
        };
        Some(Row {
            number: actual + 1,
            shown,
            lines,
            pinned,
            mark,
        })
    }

    /// 印を付けたものを、付けた順に、貼り付けるものにする。
    fn marked_picks(&self) -> Vec<Pick> {
        self.marks
            .iter()
            .filter_map(|(list, index)| match Tab::from_index(list) {
                Tab::History => match self.history.get(index)? {
                    Item::Text { text, .. } => Some(Pick::History(text.clone())),
                    Item::Image(entry) => Some(Pick::Image(entry.id)),
                },
                Tab::Snippets => self.snippets.get(index).map(|s| Pick::Snippet(s.text.clone())),
            })
            .collect()
    }

    /// 位置 `index` の全文。
    fn full_text(&self, index: usize) -> Option<String> {
        let actual = self.actual(index)?;
        match self.tab {
            Tab::History => match self.history.get(actual)? {
                Item::Text { text, .. } => Some(text.clone()),
                Item::Image(_) => None,
            },
            Tab::Snippets => self.snippets.get(actual).map(|s| s.text.clone()),
        }
    }
}

/// 一覧の 1 行に出すもの。
struct Row {
    /// 番号（絞り込む前の一覧での番号）。
    number: usize,
    /// 出す文（最初の行・定型文の名前・画像の大きさと日時）。
    shown: String,
    /// 行数。
    lines: usize,
    pinned: bool,
    /// まとめて貼り付けるために印を付けた順番（1 から）。
    mark: Option<usize>,
}

/// 定型文の、検索で探す文（名前と本文。大文字と小文字の違いをなくしたもの）。
fn snippet_haystack(snippet: &Snippet) -> String {
    clip_history::fold_case(&format!("{}\n{}", snippet.name, snippet.text))
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.borrow_mut().as_mut().map(f))
}

/// 一覧の画面を出し、閉じるまで待つ。画面を前面にできなかったときは `None`
/// （前面にできないまま出すと、キーで操作できず、閉じられなくなるため）。
pub unsafe fn choose(
    history: Vec<Item>,
    snippets: Vec<Snippet>,
    view: View,
    target: HWND,
    resume: Option<Resume>,
) -> Option<Outcome> {
    let anchor = anchor_point(target, view.position);
    let monitor = MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST);
    let mut dpi = 96;
    let mut dpi_y = 96;
    if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y).is_err() {
        dpi = 96;
    }
    let layout = Layout::of(view.position);
    let work = work_area(monitor);
    let (work_w, work_h) = (work.right - work.left, work.bottom - work.top);
    let header_h = ui::scale(HEADER_H + FILTER_H, dpi);

    // 画面の形ごとに、1 ページの件数・1 件の大きさ・一覧の大きさを決める。
    let (page_size, item_w, item_h, list_w, list_h) = match layout {
        Layout::List => {
            // 画面の高さに収まる件数にする（低い画面で、下の行がはみ出さないように）。
            let row_h = ui::scale(ROW_H, dpi).max(1);
            let fit = ((work_h - header_h - BORDER * 2) / row_h).max(1) as usize;
            let size = view.page_size.clamp(1, fit);
            let width = ui::scale(view.width as i32, dpi);
            (size, width, row_h, width, row_h * size as i32)
        }
        Layout::Side => {
            let row_h = ui::scale(ROW_H, dpi).max(1);
            let list_h = (work_h - header_h - BORDER * 2).max(row_h);
            let width = ui::scale((view.width as i32).max(SIDE_MIN_W), dpi).min(work_w - BORDER * 2);
            ((list_h / row_h).max(1) as usize, width, row_h, width, list_h)
        }
        Layout::Band => {
            let list_w = (work_w - BORDER * 2).max(1);
            let size = (list_w / ui::scale(CARD_W, dpi).max(1)).max(1);
            let card_h = ui::scale(CARD_H, dpi).min((work_h - header_h - BORDER * 2).max(1));
            (size as usize, list_w / size, card_h, list_w, card_h)
        }
    };
    let (width, height) = (list_w + BORDER * 2, header_h + list_h + BORDER * 2);

    let hwnd = create_window()?;
    let tip = create_tip_window(hwnd);
    let font = ui::create_ui_font(dpi);
    let colors = view.theme.palette().unwrap_or_else(|| system_palette());
    let counts = [history.len(), snippets.len()];
    // 前に閉じたときの場所と絞り込みがあれば、そこから（件数が減っていれば収まる所へ）。
    // 無ければ、中身のある最初のタブから始める。
    let wanted = [0, 1].map(|i| resume.as_ref().and_then(|r| r.places[i].0));
    let tab = match &resume {
        Some(resume) => Tab::from_index(resume.tab),
        None => Tab::from_index(counts.iter().position(|c| *c > 0).unwrap_or(0)),
    };
    let kind = resume.as_ref().map(|r| r.kind).unwrap_or_default();
    let query = resume.map(|r| r.query).unwrap_or_default();
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            history,
            snippets,
            thumbs: HashMap::new(),
            layout,
            keep_open: view.keep_open,
            target,
            away: 0,
            tab,
            places: [(None, 0); 2],
            visible: [Vec::new(), Vec::new()],
            query,
            kind,
            searching: false,
            folded: None,
            marks: Marks::default(),
            page_size,
            opacity: view.opacity,
            dpi,
            font,
            colors,
            back_brush: CreateSolidBrush(COLORREF(colors.back)),
            tab_rects: [RECT::default(); 2],
            keep_rect: RECT::default(),
            kind_rect: RECT::default(),
            search_rect: RECT::default(),
            edit_rect: RECT::default(),
            tip,
            tip_text: String::new(),
            tip_image: None,
            last_mouse: None,
            pressed_on_list: false,
            wheel: 0,
            outcome: Outcome::default(),
            done: false,
        })
    });
    with_state(|st| {
        st.rebuild();
        st.restore_places(wanted);
    });
    set_opacity(hwnd, view.opacity);
    create_header(hwnd);
    create_search_edit(control(hwnd, ID_HEADER), font);
    let list = create_list(hwnd, layout, font);
    let _ = SetWindowPos(
        control(hwnd, ID_HEADER),
        None,
        BORDER,
        BORDER,
        list_w,
        header_h,
        SWP_NOZORDER | SWP_NOACTIVATE,
    );
    let _ = SetWindowPos(
        list,
        None,
        BORDER,
        BORDER + header_h,
        list_w,
        list_h,
        SWP_NOZORDER | SWP_NOACTIVATE,
    );
    SendMessageW(list, LB_SETITEMHEIGHT, WPARAM(0), LPARAM(item_h as isize));
    if layout == Layout::Band {
        SendMessageW(list, LB_SETCOLUMNWIDTH, WPARAM(item_w as usize), LPARAM(0));
    }

    // 一覧では日本語入力を使わない（右クリックのメニューを出す間だけ前面になるため）。
    let _ = ImmAssociateContextEx(list, None, 0);

    // 置く場所。モニターの作業領域に収める。
    let (x, y) = match layout {
        Layout::List => {
            // 基準の位置の下（入らなければ上）。
            let mut y = anchor.y + ui::scale(4, dpi);
            if y + height > work.bottom {
                y = anchor.y - height - ui::scale(24, dpi);
            }
            (anchor.x, y)
        }
        Layout::Side => (work.right - width, work.top),
        Layout::Band => (work.left, work.bottom - height),
    };
    let x = x.clamp(work.left, (work.right - width).max(work.left));
    let y = y.clamp(work.top, (work.bottom - height).max(work.top));
    // 前面にはしない（元のアプリの入力欄に、カーソルを残したままにする）。
    let _ = SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        x,
        y,
        width,
        height,
        SWP_SHOWWINDOW | SWP_NOACTIVATE,
    );
    show_page(hwnd);
    pump_for(SHOW_WAIT);

    {
        let mut gate = keys();
        // 出し直すまでの間に押されたキーを、この一覧に渡す。
        let queued = gate.shown();
        REOPENING.store(false, Ordering::SeqCst);
        CLICKED_OUTSIDE.store(false, Ordering::SeqCst);
        TIP.store(tip.0 as isize, Ordering::SeqCst);
        POPUP.store(hwnd.0 as isize, Ordering::SeqCst);
        for vk in queued {
            let _ = PostMessageW(hwnd, WM_APP_KEY, WPARAM(vk as usize), LPARAM(0));
        }
    }
    SetTimer(hwnd, TIMER_WATCH, WATCH_INTERVAL_MS, None);
    run_loop(hwnd);
    let _ = KillTimer(hwnd, TIMER_WATCH);
    {
        let _gate = keys();
        POPUP.store(0, Ordering::SeqCst);
        TIP.store(0, Ordering::SeqCst);
        SEARCHING.store(false, Ordering::SeqCst);
    }
    // 検索やメニューのために一覧を前面にしたまま閉じるときは、貼り付け先に前面を戻してから閉じる
    // （閉じたあとで、ほかのウィンドウが前面にならないように）。ほかのウィンドウを押して閉じた
    // ときと、定型文の編集を開くときは戻さない。
    let give_back = with_state(|st| {
        !st.target.is_invalid() && !st.outcome.left_for_other_window && !st.outcome.edit_snippets
    })
    .unwrap_or(false);
    if give_back && GetForegroundWindow() == hwnd {
        let target = with_state(|st| st.target).unwrap_or_default();
        let _ = SetForegroundWindow(target);
    }
    let outcome = with_state(|st| {
        st.outcome.resume = Resume {
            tab: st.tab.index(),
            places: st.actual_places(),
            query: st.query.clone(),
            kind: st.kind,
        };
        std::mem::take(&mut st.outcome)
    });
    close(hwnd);
    outcome
}

/// 一覧（行をこちらで描くリストボックス）を作る。帯では、横に並べる。
unsafe fn create_list(parent: HWND, layout: Layout, font: HFONT) -> HWND {
    let Ok(instance) = GetModuleHandleW(None) else {
        return HWND::default();
    };
    // LBS_OWNERDRAWFIXED | LBS_NODATA | LBS_NOINTEGRALHEIGHT | LBS_NOTIFY
    // 枠は付けない（画面の側で 1 ピクセルの枠を描く）。
    let mut style = WS_CHILD.0 | WS_VISIBLE.0 | 0x0010 | 0x2000 | 0x0100 | 0x0001;
    if layout == Layout::Band {
        style |= LBS_MULTICOLUMN;
    }
    let list = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        w!("LISTBOX"),
        w!(""),
        WINDOW_STYLE(style),
        0,
        0,
        0,
        0,
        parent,
        HMENU(ID_LIST as isize as *mut core::ffi::c_void),
        instance,
        None,
    )
    .unwrap_or_default();
    SendMessageW(list, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
    list
}

/// 「Windows の色」の配色。
unsafe fn system_palette() -> Palette {
    let window = GetSysColor(COLOR_WINDOW);
    Palette {
        back: window,
        alt: blend(window, GetSysColor(COLOR_HIGHLIGHT), 8),
        text: GetSysColor(COLOR_WINDOWTEXT),
        sub: GetSysColor(COLOR_GRAYTEXT),
        sel_back: GetSysColor(COLOR_HIGHLIGHT),
        sel_text: GetSysColor(COLOR_HIGHLIGHTTEXT),
        header_back: GetSysColor(COLOR_BTNFACE),
        header_text: GetSysColor(COLOR_BTNTEXT),
        border: GetSysColor(COLOR_BTNSHADOW),
        tip_back: GetSysColor(COLOR_INFOBK),
        tip_text: GetSysColor(COLOR_INFOTEXT),
    }
}

/// 画面の不透明度を変える。
unsafe fn set_opacity(hwnd: HWND, opacity: u32) {
    let alpha = (opacity.clamp(clip_history::MIN_OPACITY, 100) * 255 / 100) as u8;
    let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
}

/// 画面を閉じ、状態を片付ける。
unsafe fn close(hwnd: HWND) {
    if let Some(state) = STATE.with(|s| s.borrow_mut().take()) {
        let _ = DestroyWindow(state.tip);
        let _ = DeleteObject(state.font);
        let _ = DeleteObject(state.back_brush);
    }
    let _ = DestroyWindow(hwnd);
}

/// 閉じるまでメッセージを処理する。キー操作とホイールはここで先に受け取る。
unsafe fn run_loop(hwnd: HWND) {
    let list = control(hwnd, ID_LIST);
    let mut msg = MSG::default();
    while !is_done() {
        let ret = GetMessageW(&mut msg, None, 0, 0);
        if CLOSE_REQUESTED.swap(false, Ordering::SeqCst) {
            if CLICKED_OUTSIDE.swap(false, Ordering::SeqCst) {
                // ほかのウィンドウを押して閉じた（そちらを使いたいので、元のウィンドウに戻さない）。
                with_state(|st| st.outcome.left_for_other_window = true);
            }
            finish(None);
        }
        if ret.0 <= 0 {
            // アプリの終了。元のループにも知らせる。
            PostQuitMessage(msg.wParam.0 as i32);
            finish(None);
            break;
        }
        let ours = msg.hwnd == hwnd || IsChild(hwnd, msg.hwnd).as_bool();
        // キーボードフックが知らせたキー。
        if msg.hwnd == hwnd && msg.message == WM_APP_KEY {
            on_key(hwnd, msg.wParam.0 as u16);
            continue;
        }
        if ours
            && (msg.message == WM_KEYDOWN || msg.message == WM_SYSKEYDOWN)
            && on_key(hwnd, msg.wParam.0 as u16)
        {
            continue;
        }
        let wheel = if msg.hwnd == hwnd && msg.message == WM_APP_WHEEL {
            // マウスフックが知らせたホイール。
            Some(msg.wParam.0 as isize as i32)
        } else if ours && msg.message == WM_MOUSEWHEEL {
            Some(i32::from(((msg.wParam.0 >> 16) & 0xFFFF) as u16 as i16))
        } else {
            None
        };
        if let Some(delta) = wheel {
            // 回した量を貯めて、1 目盛り（WHEEL_DELTA = 120）ごとに 1 回動かす。
            let steps = with_state(|st| {
                st.wheel += delta;
                let steps = st.wheel / 120;
                st.wheel -= steps * 120;
                steps
            })
            .unwrap_or(0);
            let shift = GetAsyncKeyState(VK_SHIFT.0 as i32) < 0;
            for _ in 0..steps.unsigned_abs() {
                if shift {
                    // Shift+ホイールで不透明度を変える（奥に回すと濃く）。
                    change_opacity(hwnd, steps > 0);
                } else {
                    // ホイールはページを移る（手前に回すと次のページ）。
                    turn_page(hwnd, if steps < 0 { 1 } else { -1 });
                }
            }
            continue;
        }
        if msg.hwnd == list && msg.message == WM_MOUSEMOVE {
            // マウスを乗せた行を選ぶ。マウスが動いていないのに届いたもの（画面や吹き出しを
            // 出し直したときに Windows が送る）では選び直さない（キーで選んだ行を保つ）。
            let moved = with_state(|st| {
                let point = (msg.pt.x, msg.pt.y);
                let moved = st.last_mouse.is_some_and(|last| last != point);
                st.last_mouse = Some(point);
                moved
            })
            .unwrap_or(false);
            if moved {
                select_row_at(hwnd, list, msg.lParam);
            }
        }
        if msg.hwnd == list
            && matches!(msg.message, WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN)
        {
            // 押した行を選ぶ（キーで選んだあと、マウスを動かさずに押したときも、押した行にする）。
            select_row_at(hwnd, list, msg.lParam);
            if msg.message != WM_RBUTTONDOWN {
                with_state(|st| st.pressed_on_list = true);
            }
            // 一覧（リストボックス）には渡さない。渡すと、押された一覧が自分を入力先にしようとして、
            // 一覧の画面がアクティブになってしまう。
            continue;
        }
        if msg.hwnd == list && msg.message == WM_LBUTTONUP {
            let pressed = with_state(|st| std::mem::take(&mut st.pressed_on_list)).unwrap_or(false);
            if pressed && row_at(list, msg.lParam).is_some() {
                select_row_at(hwnd, list, msg.lParam);
                choose_current();
            }
            continue;
        }
        if ours && msg.message == WM_RBUTTONUP {
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            // 行の外（見出しや、行の無い所）で押したときは、行に対する操作を選べなくする
            // （前に選んだ行を、気づかずに消したりしないように）。
            let on_row = msg.hwnd == list && row_at(list, msg.lParam).is_some();
            context_menu(hwnd, point, on_row);
            continue;
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

/// マウスの位置（一覧の中の座標）にある行を選ぶ。
unsafe fn select_row_at(hwnd: HWND, list: HWND, lparam: LPARAM) {
    if let Some(row) = row_at(list, lparam) {
        let page_start = with_state(|st| st.page() * st.page_size).unwrap_or(0);
        select(hwnd, page_start + row);
    }
}

fn is_done() -> bool {
    STATE.with(|s| s.borrow().as_ref().is_none_or(|st| st.done))
}

/// 選んで（または選ばずに）閉じる。
fn finish(chosen: Option<Pick>) {
    with_state(|st| {
        st.outcome.chosen = chosen;
        st.done = true;
    });
}

/// 不透明度を 1 段階変える。
unsafe fn change_opacity(hwnd: HWND, up: bool) {
    let Some(opacity) = with_state(|st| {
        st.opacity = if up {
            (st.opacity + OPACITY_STEP).min(clip_history::MAX_OPACITY)
        } else {
            st.opacity.saturating_sub(OPACITY_STEP).max(clip_history::MIN_OPACITY)
        };
        st.outcome.opacity = Some(st.opacity);
        st.opacity
    }) else {
        return;
    };
    set_opacity(hwnd, opacity);
}

/// キー操作。処理したら `true`。
unsafe fn on_key(hwnd: HWND, vk: u16) -> bool {
    let Some((current, count, page_size, page)) =
        with_state(|st| (st.selected(), st.len(), st.page_size, st.page()))
    else {
        return false;
    };
    let current = current.map_or(-1, |c| c as isize);
    let move_to = |pos: isize| {
        if count > 0 {
            select(hwnd, pos.clamp(0, count as isize - 1) as usize);
        }
    };
    let range = clip_history::page_range(page, count, page_size);
    // 一覧は前面ではないので、押されているかは、キーボードそのものの状態で調べる。Ctrl は、
    // アプリが「離した」を送っても利用者が押したままのことがあるので、フックが見た状態も使う。
    let shift = GetAsyncKeyState(VK_SHIFT.0 as i32) < 0;
    let ctrl = GetAsyncKeyState(VK_CONTROL.0 as i32) < 0 || CTRL_HELD.load(Ordering::SeqCst);
    let (tab, layout, searching, has_query) =
        with_state(|st| (st.tab, st.layout, st.searching, !st.query.is_empty()))
            .unwrap_or((Tab::History, Layout::List, false, false));
    if searching {
        // 検索の入力欄に入力している間は、選ぶ・貼り付ける・閉じるキーだけをここで受け取り、
        // ほかのキー（文字・左右・Home / End・Delete など）は入力欄に渡す。上下は、帯でも選ぶ
        // のに使う（左右は入力欄の中で動くのに使う）。
        match vk {
            v if v == VK_ESCAPE.0 => {
                // 1 回目の Esc は、検索をやめて言葉を消す（2 回目で閉じる）。
                clear_query(hwnd);
                leave_search(hwnd, true);
            }
            v if v == VK_RETURN.0 => choose_current(),
            v if v == VK_UP.0 => move_to(current - 1),
            v if v == VK_DOWN.0 => move_to(current + 1),
            v if v == VK_PRIOR.0 => turn_page(hwnd, -1),
            v if v == VK_NEXT.0 => turn_page(hwnd, 1),
            v if v == VK_TAB.0 => switch_tab(hwnd, tab.step(if shift { -1 } else { 1 })),
            v if v == VK_APPS.0 || (v == VK_F10.0 && shift) => {
                context_menu(hwnd, selected_row_point(hwnd), true);
            }
            _ => return false,
        }
        return true;
    }
    // 帯では左右で選び、上下でタブを切り替える（一覧・パネルではその逆）。
    let (vk_prev, vk_next, vk_tab_prev, vk_tab_next) = if layout == Layout::Band {
        (VK_LEFT.0, VK_RIGHT.0, VK_UP.0, VK_DOWN.0)
    } else {
        (VK_UP.0, VK_DOWN.0, VK_LEFT.0, VK_RIGHT.0)
    };
    match vk {
        // 検索の言葉で絞り込んだまま出し直したときは、1 回目の Esc で絞り込みをやめる。
        v if v == VK_ESCAPE.0 && has_query => clear_query(hwnd),
        v if v == VK_ESCAPE.0 => finish(None),
        // Ctrl+F か / で検索を始める。
        v if v == u16::from(b'F') && ctrl => enter_search(hwnd),
        v if (v == VK_OEM_2.0 || v == VK_DIVIDE.0) && !ctrl => enter_search(hwnd),
        // F で、履歴のタブで出す種類を切り替える（すべて → 文字だけ → 画像だけ）。
        v if v == u16::from(b'F') => cycle_kind(hwnd),
        // Space で、まとめて貼り付けるための印を付ける・外す。
        v if v == VK_SPACE.0 && !ctrl => toggle_mark_current(hwnd),
        v if v == VK_RETURN.0 => choose_current(),
        v if v == vk_prev => move_to(current - 1),
        v if v == vk_next => move_to(current + 1),
        // Ctrl を押していれば、全体の最初・最後へ。
        v if v == VK_HOME.0 && ctrl => move_to(0),
        v if v == VK_END.0 && ctrl => move_to(count as isize - 1),
        v if v == VK_HOME.0 => move_to(range.start as isize),
        v if v == VK_END.0 => move_to(range.end as isize - 1),
        v if v == VK_TAB.0 => switch_tab(hwnd, tab.step(if shift { -1 } else { 1 })),
        // K で「出したままにする」を切り替える。
        v if v == u16::from(b'K') && !ctrl => toggle_keep_open(hwnd),
        // P でピン留めを切り替える（一覧では文字を入力しないので、文字のキーを使える）。
        v if v == u16::from(b'P') && !ctrl => toggle_pin_current(hwnd),
        v if v == VK_PRIOR.0 => turn_page(hwnd, -1),
        v if v == VK_NEXT.0 => turn_page(hwnd, 1),
        v if v == vk_tab_prev => switch_tab(hwnd, tab.step(-1)),
        v if v == vk_tab_next => switch_tab(hwnd, tab.step(1)),
        v if v == VK_DELETE.0 => remove_current(hwnd),
        v if v == VK_APPS.0 || (v == VK_F10.0 && shift) => {
            context_menu(hwnd, selected_row_point(hwnd), true);
        }
        _ => return false,
    }
    true
}

/// タブを切り替える。
unsafe fn switch_tab(hwnd: HWND, tab: Tab) {
    let changed = with_state(|st| {
        let changed = st.tab != tab;
        st.tab = tab;
        changed
    });
    if changed == Some(true) {
        show_page(hwnd);
    }
}

/// 前後のページへ移る（ページの中での行の位置はそのまま。足りなければ最後の行）。
unsafe fn turn_page(hwnd: HWND, delta: isize) {
    let Some((page, row, count, size)) = with_state(|st| {
        let row = st.selected().map_or(0, |s| s % st.page_size);
        (st.page(), row, st.len(), st.page_size)
    }) else {
        return;
    };
    let pages = clip_history::page_count(count, size) as isize;
    let next = page as isize + delta;
    if count == 0 || next < 0 || next >= pages {
        return;
    }
    let range = clip_history::page_range(next as usize, count, size);
    select(hwnd, (range.start + row).min(range.end - 1));
}

/// 選んでいるものを貼り付けるために閉じる。印を付けたものがあれば、それらを付けた順に
/// まとめて貼り付ける。
fn choose_current() {
    let marked = with_state(|st| st.marked_picks()).unwrap_or_default();
    if !marked.is_empty() {
        with_state(|st| st.outcome.ctrl_held = CTRL_HELD.load(Ordering::SeqCst));
        finish(Some(Pick::Many(marked)));
        return;
    }
    choose_selected();
}

/// 選んでいるもの（印にかかわらず、その 1 件）を貼り付けるために閉じる。
fn choose_selected() {
    let chosen = with_state(|st| {
        let pos = st.selected()?;
        Some(match (st.tab, st.image_id(pos)) {
            (Tab::History, Some(id)) => Pick::Image(id),
            (Tab::History, None) => Pick::History(st.full_text(pos)?),
            (Tab::Snippets, _) => Pick::Snippet(st.full_text(pos)?),
        })
    })
    .flatten();
    if chosen.is_some() {
        with_state(|st| st.outcome.ctrl_held = CTRL_HELD.load(Ordering::SeqCst));
        finish(chosen);
    }
}

/// 選んでいる文字（履歴の文字か定型文）を、整えて貼り付けるために閉じる。
fn choose_formatted(format: QuickFormat) {
    let chosen = with_state(|st| {
        let pos = st.selected()?;
        let text = st.full_text(pos)?;
        let pick = match st.tab {
            Tab::History => Pick::History(text),
            Tab::Snippets => Pick::Snippet(text),
        };
        Some(Pick::Formatted(Box::new(pick), format))
    })
    .flatten();
    if chosen.is_some() {
        with_state(|st| st.outcome.ctrl_held = CTRL_HELD.load(Ordering::SeqCst));
        finish(chosen);
    }
}

/// 選んでいる履歴の画像を、保存先を選んで PNG のファイルに書き出す。結果は画面に短く出す。
unsafe fn save_selected_image(hwnd: HWND) {
    let Some(id) = with_state(|st| st.selected().and_then(|p| st.image_id(p))).flatten() else {
        return;
    };
    let now = GetLocalTime();
    let name = format!(
        "画像_{:04}{:02}{:02}_{:02}{:02}{:02}.png",
        now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond
    );
    let Some(path) = ui::choose_file(hwnd, true, "PNG 画像 (*.png)\0*.png\0", "png", &name) else {
        return;
    };
    match clip_image_store::save_png(id, &path) {
        Ok((width, height)) => {
            log::info!("画像の履歴: 画像を保存しました（{width}×{height}）: {}", path.display());
            ime_indicator::notify("保存");
        }
        Err(e) => {
            log::warn!("画像の履歴: 画像を保存できませんでした: {e}（{}）", path.display());
            ime_indicator::notify("失敗");
        }
    }
}

/// 選んでいる履歴を 1 件消す（定型文は定型文の画面で消す）。
unsafe fn remove_current(hwnd: HWND) {
    let removed = with_state(|st| {
        let pos = st.selected()?;
        let actual = st.actual(pos)?;
        if st.tab != Tab::History || actual >= st.history.len() {
            return None;
        }
        match st.history.remove(actual) {
            Item::Text { text, .. } => st.outcome.removed.push(text),
            Item::Image(entry) => st.outcome.removed_images.push(entry.id),
        }
        if let Some(folded) = st.folded.as_mut() {
            folded[0].remove(actual);
        }
        st.marks.removed(Tab::History.index(), actual);
        st.rebuild();
        let count = st.len();
        let selected = (count > 0).then(|| pos.min(count - 1));
        st.set_place(selected, selected.map_or(0, |s| s / st.page_size));
        Some(())
    })
    .flatten();
    if removed.is_some() {
        show_page(hwnd);
    }
}

/// 選んでいる履歴を定型文に登録する。登録できないとき（すでに同じ本文がある・長すぎる・
/// 件数の上限）は、その理由を画面に短く出す。
unsafe fn register_current(hwnd: HWND) {
    let result = with_state(|st| {
        if st.tab != Tab::History {
            return None;
        }
        let text = st.selected().and_then(|pos| st.full_text(pos))?;
        // 定型文は貼り付けるときに {date} などを置き換えるので、履歴の文字にある { } は、
        // そのまま貼り付けられるよう {{ }} にして登録する。
        let escaped = text.replace('{', "{{").replace('}', "}}");
        let Ok(snippet) = snippets::make("", &escaped) else {
            return Some(Err("長すぎ"));
        };
        if st.snippets.iter().any(|s| s.text == snippet.text) {
            return Some(Err("登録済み"));
        }
        if st.snippets.len() >= snippets::MAX_SNIPPETS {
            return Some(Err("上限"));
        }
        st.outcome.registered.push(snippet.text.clone());
        if let Some(folded) = st.folded.as_mut() {
            folded[1].push(snippet_haystack(&snippet));
        }
        st.snippets.push(snippet);
        st.refilter();
        Some(Ok(()))
    })
    .flatten();
    match result {
        Some(Ok(())) => {
            crate::ime_indicator::notify("登録");
            show_page(hwnd);
        }
        Some(Err(reason)) => {
            crate::ime_indicator::notify(reason);
        }
        None => {}
    }
}

/// 選んでいる履歴のピン留めを切り替える。ピン留めしたものは、件数の上限で消えない。
unsafe fn toggle_pin_current(hwnd: HWND) {
    let changed = with_state(|st| {
        let pos = st.selected()?;
        if st.tab != Tab::History {
            return None;
        }
        let actual = st.actual(pos)?;
        match st.history.get_mut(actual)? {
            Item::Text { text, pinned } => {
                *pinned = !*pinned;
                let change = (text.clone(), *pinned);
                st.outcome.pin_changes.push(change);
            }
            Item::Image(entry) => {
                entry.pinned = !entry.pinned;
                let change = (entry.id, entry.pinned);
                st.outcome.image_pin_changes.push(change);
            }
        }
        Some(())
    })
    .flatten();
    if changed.is_some() {
        let _ = InvalidateRect(control(hwnd, ID_LIST), None, true);
    }
}

/// 「出したままにする」（貼り付けても一覧を閉じない）を切り替える。
unsafe fn toggle_keep_open(hwnd: HWND) {
    with_state(|st| {
        st.keep_open = !st.keep_open;
        st.outcome.keep_open = Some(st.keep_open);
    });
    let _ = InvalidateRect(control(hwnd, ID_HEADER), None, true);
}

/// 選んでいるものに、まとめて貼り付けるための印を付ける・外す。
unsafe fn toggle_mark_current(hwnd: HWND) {
    let changed = with_state(|st| {
        let actual = st.selected().and_then(|p| st.actual(p))?;
        st.marks.toggle(st.tab.index(), actual);
        Some(())
    })
    .flatten();
    if changed.is_some() {
        let _ = InvalidateRect(control(hwnd, ID_LIST), None, true);
        let _ = InvalidateRect(control(hwnd, ID_HEADER), None, true);
    }
}

/// 印をすべて外す。
unsafe fn clear_marks(hwnd: HWND) {
    with_state(|st| st.marks.clear());
    let _ = InvalidateRect(control(hwnd, ID_LIST), None, true);
    let _ = InvalidateRect(control(hwnd, ID_HEADER), None, true);
}

/// 履歴のタブで出す種類を切り替える（すべて → 文字だけ → 画像だけ）。定型文のタブでは何もしない。
unsafe fn cycle_kind(hwnd: HWND) {
    let changed = with_state(|st| {
        if st.tab != Tab::History {
            return false;
        }
        st.kind = st.kind.next();
        st.refilter();
        true
    });
    if changed == Some(true) {
        show_page(hwnd);
    }
}

/// 検索の入力欄。
unsafe fn search_edit(hwnd: HWND) -> HWND {
    GetDlgItem(control(hwnd, ID_HEADER), ID_SEARCH).unwrap_or_default()
}

/// 検索を始める: 見出しの下の段に入力欄を出し、一覧を前面にして入力欄に入力できるようにする
/// （日本語入力を使うには、前面にする必要がある）。前面にできなければ、検索をやめる。
unsafe fn enter_search(hwnd: HWND) {
    let edit = search_edit(hwnd);
    let Some((target, already, query)) =
        with_state(|st| (st.target, st.searching, st.query.clone()))
    else {
        return;
    };
    if edit.is_invalid() {
        return;
    }
    if !already {
        // 前面にする前に知らせておく（前面になってすぐ押されたキーも、入力欄に届くように）。
        with_state(|st| st.searching = true);
        SEARCHING.store(true, Ordering::SeqCst);
        let header = control(hwnd, ID_HEADER);
        let _ = InvalidateRect(header, None, true);
        // 描き直して、入力欄を置く範囲を決める。
        let _ = UpdateWindow(header);
        place_search_edit(hwnd);
        let _ = SetWindowTextW(edit, &HSTRING::from(query.as_str()));
        if !bring_to_front(hwnd, target) {
            log::debug!("クリップボードの履歴: 検索のために一覧を前面にできませんでした");
            leave_search(hwnd, false);
            ime_indicator::notify("不可");
            return;
        }
    }
    let _ = SetFocus(edit);
    let end = GetWindowTextLengthW(edit).max(0) as usize;
    SendMessageW(edit, EM_SETSEL, WPARAM(end), LPARAM(end as isize));
}

/// 検索の入力欄を、見出しの下の段の検索の欄に置く（置く範囲が変わったときだけ動かす）。
unsafe fn place_search_edit(hwnd: HWND) {
    let Some(Some(rect)) = with_state(|st| {
        if !st.searching {
            return None;
        }
        let inset = ui::scale(2, st.dpi);
        let rect = RECT {
            left: st.search_rect.left + inset * 2,
            top: st.search_rect.top + inset,
            right: st.search_rect.right - inset,
            bottom: st.search_rect.bottom - inset,
        };
        if rect == st.edit_rect {
            return None;
        }
        st.edit_rect = rect;
        Some(rect)
    }) else {
        return;
    };
    let _ = SetWindowPos(
        search_edit(hwnd),
        HWND_TOP,
        rect.left,
        rect.top,
        (rect.right - rect.left).max(1),
        (rect.bottom - rect.top).max(1),
        SWP_SHOWWINDOW | SWP_NOACTIVATE,
    );
}

/// 検索の入力欄を隠し、入力をやめる。`give_back` なら、貼り付け先に前面を戻す。
unsafe fn leave_search(hwnd: HWND, give_back: bool) {
    let Some(target) = with_state(|st| {
        st.searching = false;
        st.edit_rect = RECT::default();
        st.away = 0;
        st.target
    }) else {
        return;
    };
    SEARCHING.store(false, Ordering::SeqCst);
    let _ = ShowWindow(search_edit(hwnd), SW_HIDE);
    if give_back && !target.is_invalid() && GetForegroundWindow() == hwnd {
        let _ = SetForegroundWindow(target);
    }
    let _ = InvalidateRect(control(hwnd, ID_HEADER), None, true);
}

/// 検索の言葉を消して、絞り込みをやめる。
unsafe fn clear_query(hwnd: HWND) {
    with_state(|st| {
        st.query.clear();
        st.refilter();
    });
    // 入力欄の文字も消す（消したことの知らせでは、もう言葉が空なので何もしない）。
    let _ = SetWindowTextW(search_edit(hwnd), w!(""));
    show_page(hwnd);
}

/// 検索の入力欄の文字が変わった。その言葉で絞り込み直す。
unsafe fn on_search_changed(hwnd: HWND) {
    let edit = search_edit(hwnd);
    let mut buffer = vec![0u16; GetWindowTextLengthW(edit).max(0) as usize + 1];
    let len = GetWindowTextW(edit, &mut buffer).max(0) as usize;
    let text = String::from_utf16_lossy(&buffer[..len.min(buffer.len())]);
    let changed = with_state(|st| {
        if st.query == text {
            return false;
        }
        st.query = text;
        st.refilter();
        true
    });
    if changed == Some(true) {
        show_page(hwnd);
    }
}

/// 右クリックのメニューを出す。
unsafe fn context_menu(hwnd: HWND, point: POINT, on_row: bool) {
    let Some(((tab, has_row, pinned, is_image, keep_open), (marked, any_marks, is_text))) = with_state(|st| {
        let item = st
            .selected()
            .filter(|_| st.tab == Tab::History)
            .and_then(|p| st.actual(p))
            .and_then(|a| st.history.get(a));
        let pinned = item.is_some_and(Item::pinned);
        let is_image = matches!(item, Some(Item::Image(_)));
        let marked = st
            .selected()
            .and_then(|p| st.actual(p))
            .is_some_and(|a| st.marks.order(st.tab.index(), a).is_some());
        (
            (st.tab, on_row && st.selected().is_some(), pinned, is_image, st.keep_open),
            (marked, !st.marks.is_empty(), st.selected().and_then(|p| st.full_text(p)).is_some()),
        )
    }) else {
        return;
    };
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    let row_flag = if has_row { MF_STRING } else { MF_STRING | MF_GRAYED };
    // 印を付けたものがあるときは、Enter でそれらをまとめて貼り付ける。
    let paste_label = if any_marks {
        w!("貼り付け(&V)")
    } else {
        w!("貼り付け(&V)\tEnter")
    };
    let _ = AppendMenuW(menu, row_flag, MENU_PASTE, paste_label);
    // 整えて貼り付け（文字の行だけ。履歴そのものは変えない）。
    if let Ok(formats) = CreatePopupMenu() {
        for (i, (_, name)) in text_transform::QUICK_FORMATS.iter().enumerate() {
            let _ = AppendMenuW(formats, MF_STRING, MENU_FORMAT_BASE + i, &HSTRING::from(*name));
        }
        let format_flag = if has_row && is_text { MF_POPUP } else { MF_POPUP | MF_GRAYED };
        let _ = AppendMenuW(menu, format_flag, formats.0 as usize, w!("整えて貼り付け(&F)"));
    }
    let marks_flag = if any_marks { MF_STRING } else { MF_STRING | MF_GRAYED };
    let _ = AppendMenuW(
        menu,
        marks_flag,
        MENU_PASTE_MARKED,
        w!("選んだものをまとめて貼り付け(&M)\tEnter"),
    );
    let mark_label = if marked {
        w!("まとめて貼り付けから外す(&A)\tSpace")
    } else {
        w!("まとめて貼り付けに加える(&A)\tSpace")
    };
    let _ = AppendMenuW(menu, row_flag, MENU_MARK, mark_label);
    let _ = AppendMenuW(menu, marks_flag, MENU_CLEAR_MARKS, w!("選択をすべて外す(&U)"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    if tab != Tab::Snippets {
        let pin_label = if pinned {
            w!("ピン留めを外す(&I)\tP")
        } else {
            w!("ピン留めする(&I)\tP")
        };
        let _ = AppendMenuW(menu, row_flag, MENU_PIN, pin_label);
        // 定型文に登録できるのは文字だけ。
        let register_flag = if is_image { MF_STRING | MF_GRAYED } else { row_flag };
        let _ = AppendMenuW(menu, register_flag, MENU_REGISTER, w!("定型文に登録(&T)"));
        // 画像の行だけ。
        let image_flag = if has_row && is_image { MF_STRING } else { MF_STRING | MF_GRAYED };
        let _ = AppendMenuW(menu, image_flag, MENU_SAVE_IMAGE, w!("画像をファイルに保存...(&S)"));
        let _ = AppendMenuW(menu, row_flag, MENU_DELETE, w!("この履歴を削除(&D)\tDelete"));
    }
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, MENU_SEARCH, w!("検索(&Q)\tCtrl+F"));
    let keep_flag = if keep_open { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(menu, keep_flag, MENU_KEEP, w!("貼り付けても出したままにする(&K)\tK"));
    let _ = AppendMenuW(menu, MF_STRING, MENU_EDIT_SNIPPETS, w!("定型文の編集...(&E)"));
    let _ = AppendMenuW(menu, MF_STRING, MENU_CLOSE, w!("閉じる(&C)\tEsc"));
    // メニューは、出したウィンドウが前面でないと、キーで選べず、外を押しても閉じない。メニューを
    // 出している間だけ一覧を前面にし、閉じたら元のウィンドウに戻す。
    let target = with_state(|st| st.target).unwrap_or_default();
    MENU_OPEN.store(true, Ordering::SeqCst);
    bring_to_front(hwnd, target);
    let chosen = TrackPopupMenuEx(
        menu,
        (TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN).0,
        point.x,
        point.y,
        hwnd,
        None,
    )
    .0 as usize;
    let _ = DestroyMenu(menu);
    if chosen == MENU_SAVE_IMAGE {
        // 保存先を選ぶ画面は、一覧を前面にしたまま（メニューを出している扱いのまま）出す。
        save_selected_image(hwnd);
    }
    let searching = with_state(|st| st.searching).unwrap_or(false);
    if searching {
        // 検索の入力欄に入力している間は、一覧を前面のままにし、入力欄に戻る。
        let _ = SetFocus(search_edit(hwnd));
    } else if !target.is_invalid()
        && chosen != MENU_EDIT_SNIPPETS
        && chosen != MENU_SEARCH
        && GetForegroundWindow() == hwnd
    {
        // 一覧が前面のままのときだけ戻す（メニューの外のほかのウィンドウを押して閉じたときは、
        // そちらを使いたいので取り返さない）。
        let _ = SetForegroundWindow(target);
    }
    with_state(|st| st.away = 0);
    MENU_OPEN.store(false, Ordering::SeqCst);
    match chosen {
        MENU_SEARCH => enter_search(hwnd),
        MENU_PASTE => choose_selected(),
        MENU_PASTE_MARKED => choose_current(),
        MENU_MARK => toggle_mark_current(hwnd),
        MENU_CLEAR_MARKS => clear_marks(hwnd),
        n if (MENU_FORMAT_BASE..MENU_FORMAT_BASE + text_transform::QUICK_FORMATS.len())
            .contains(&n) =>
        {
            choose_formatted(text_transform::QUICK_FORMATS[n - MENU_FORMAT_BASE].0);
        }
        MENU_REGISTER => register_current(hwnd),
        MENU_PIN => toggle_pin_current(hwnd),
        MENU_KEEP => toggle_keep_open(hwnd),
        MENU_DELETE => remove_current(hwnd),
        MENU_EDIT_SNIPPETS => {
            with_state(|st| st.outcome.edit_snippets = true);
            finish(None);
        }
        MENU_CLOSE => finish(None),
        _ => {}
    }
}

/// 選んでいる行の左下（キーでメニューを出すときの位置）。
unsafe fn selected_row_point(hwnd: HWND) -> POINT {
    let list = control(hwnd, ID_LIST);
    let row = with_state(|st| st.selected().map(|s| s % st.page_size)).flatten().unwrap_or(0);
    let mut rect = RECT::default();
    SendMessageW(list, LB_GETITEMRECT, WPARAM(row), LPARAM(&mut rect as *mut RECT as isize));
    let mut point = POINT {
        x: rect.left + 20,
        y: rect.bottom,
    };
    let _ = ClientToScreen(list, &mut point);
    point
}

/// 位置 `pos` を選ぶ。別のページなら、そのページを出す。
unsafe fn select(hwnd: HWND, pos: usize) {
    let Some(change) = with_state(|st| {
        let page = pos / st.page_size;
        if st.selected() == Some(pos) && st.page() == page {
            return None;
        }
        let changed = page != st.page();
        st.set_place(Some(pos), page);
        Some((changed, pos % st.page_size))
    })
    .flatten() else {
        return;
    };
    match change {
        (true, _) => show_page(hwnd),
        (false, row) => {
            SendMessageW(control(hwnd, ID_LIST), LB_SETCURSEL, WPARAM(row), LPARAM(0));
            update_tip(hwnd);
        }
    }
}

/// 今のタブ・ページを一覧に出し、見出しを描き直す。
unsafe fn show_page(hwnd: HWND) {
    let Some((page, count, selected, size)) =
        with_state(|st| (st.page(), st.len(), st.selected(), st.page_size))
    else {
        return;
    };
    let range = clip_history::page_range(page, count, size);
    let list = control(hwnd, ID_LIST);
    SendMessageW(list, LB_SETCOUNT, WPARAM(range.len()), LPARAM(0));
    let row = selected.filter(|s| range.contains(s)).map(|s| s - range.start);
    SendMessageW(list, LB_SETCURSEL, WPARAM(row.map_or(usize::MAX, |r| r)), LPARAM(0));
    let _ = InvalidateRect(list, None, true);
    let _ = InvalidateRect(control(hwnd, ID_HEADER), None, true);
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
    let on_page = with_state(|st| clip_history::page_range(st.page(), st.len(), st.page_size).len())?;
    (row < on_page).then_some(row)
}

unsafe fn control(hwnd: HWND, id: i32) -> HWND {
    GetDlgItem(hwnd, id).unwrap_or_default()
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

/// 1 行を描く: 番号・最初の行（定型文は名前）・行数。1 行おきに背景の色を少し変える。
unsafe fn draw_row(item: &DRAWITEMSTRUCT) {
    let Ok(row) = usize::try_from(item.itemID) else {
        return;
    };
    let Some(Some((info, dpi, font, colors))) = with_state(|st| {
        let index = st.page() * st.page_size + row;
        st.row(index).map(|info| (info, st.dpi, st.font, st.colors))
    }) else {
        return;
    };
    let Row {
        number,
        shown: text,
        lines,
        pinned,
        mark,
    } = info;
    let hdc = item.hDC;
    let rect = item.rcItem;
    let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
    if with_state(|st| st.layout) == Some(Layout::Band) {
        draw_card(hdc, rect, row, selected);
        return;
    }
    let background = if selected {
        colors.sel_back
    } else if row % 2 == 1 {
        colors.alt
    } else {
        colors.back
    };
    fill(hdc, &rect, background);
    if pinned {
        // ピン留めしたものは、左端に色の線を引いて分かるようにする。
        let bar = RECT {
            right: rect.left + ui::scale(PIN_BAR_W, dpi),
            ..rect
        };
        fill(hdc, &bar, if selected { colors.sel_text } else { colors.sel_back });
    }
    let old_font = SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT);
    let (main, sub) = if selected {
        (colors.sel_text, colors.sel_text)
    } else {
        (colors.text, colors.sub)
    };
    SetTextColor(hdc, COLORREF(sub));
    let number_rect = RECT {
        right: rect.left + ui::scale(NUMBER_W, dpi),
        ..rect
    };
    match mark {
        // 印を付けた行は、番号の代わりに、付けた順番を色の付いた札で出す。
        Some(order) => draw_mark(hdc, number_rect, order, colors, selected, dpi),
        None => {
            let mut number_rect = number_rect;
            draw_line(hdc, &number.to_string(), &mut number_rect, DT_RIGHT);
        }
    }
    SetTextColor(hdc, COLORREF(sub));
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

/// 帯の 1 件（カード）を描く: 上に番号と名前・行数、その下に中身（文字は数行、画像は絵）。
unsafe fn draw_card(hdc: HDC, rect: RECT, row: usize, selected: bool) {
    let Some(Some((number, title, lines, pinned, mark, body, thumb, dpi, font, colors))) =
        with_state(|st| {
            let index = st.page() * st.page_size + row;
            let info = st.row(index)?;
            // 上の行: 定型文は名前、画像は大きさと日時。文字の履歴は番号だけ。
            let is_image = st.image_id(index).is_some();
            let title = if st.tab == Tab::History && !is_image {
                String::new()
            } else {
                info.shown
            };
            let body = st.full_text(index).map(|t| clip_history::tip_text(&t));
            let thumb = st.thumb_at(index);
            Some((
                info.number,
                title,
                info.lines,
                info.pinned,
                info.mark,
                body,
                thumb,
                st.dpi,
                st.font,
                st.colors,
            ))
        })
    else {
        fill(hdc, &rect, with_state(|st| st.colors.back).unwrap_or(0));
        return;
    };
    fill(hdc, &rect, colors.back);
    let gap = ui::scale(CARD_GAP, dpi);
    let pad = ui::scale(CARD_PAD, dpi);
    let card = RECT {
        left: rect.left + gap,
        top: rect.top + gap,
        right: rect.right - gap,
        bottom: rect.bottom - gap,
    };
    fill(hdc, &card, if selected { colors.sel_back } else { colors.alt });
    if pinned {
        let bar = RECT {
            right: card.left + ui::scale(PIN_BAR_W, dpi),
            ..card
        };
        fill(hdc, &bar, if selected { colors.sel_text } else { colors.sel_back });
    }
    let old_font = SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT);
    let (main, sub) = if selected {
        (colors.sel_text, colors.sel_text)
    } else {
        (colors.text, colors.sub)
    };
    let head_h = ui::scale(ROW_H, dpi);
    SetTextColor(hdc, COLORREF(sub));
    let mut head = RECT {
        left: card.left + pad,
        right: card.right - pad,
        bottom: card.top + head_h,
        ..card
    };
    let label = if title.is_empty() {
        number.to_string()
    } else {
        format!("{number}  {title}")
    };
    if lines > 1 {
        let mut lines_rect = RECT {
            left: head.right - ui::scale(LINES_W, dpi),
            ..head
        };
        draw_line(hdc, &format!("{lines}行"), &mut lines_rect, DT_RIGHT);
        head.right = lines_rect.left;
    }
    if let Some(order) = mark {
        // 印を付けたカードは、上の行の左に、付けた順番を色の付いた札で出す。
        let badge = RECT {
            right: head.left + ui::scale(NUMBER_W, dpi),
            ..head
        };
        draw_mark(hdc, badge, order, colors, selected, dpi);
        head.left = badge.right + ui::scale(PAD, dpi);
        SetTextColor(hdc, COLORREF(sub));
    }
    draw_line(hdc, &label, &mut head, DT_LEFT | DT_END_ELLIPSIS);
    let mut inner = RECT {
        left: card.left + pad,
        top: card.top + head_h,
        right: card.right - pad,
        bottom: card.bottom - pad,
    };
    if let Some(thumb) = thumb {
        clip_image_store::draw_thumb(
            hdc,
            &thumb,
            inner.left,
            inner.top,
            inner.right - inner.left,
            inner.bottom - inner.top,
        );
    } else if let Some(body) = body {
        SetTextColor(hdc, COLORREF(main));
        let mut units: Vec<u16> = body.encode_utf16().collect();
        DrawTextW(
            hdc,
            &mut units,
            &mut inner,
            DT_WORDBREAK | DT_EDITCONTROL | DT_NOPREFIX | DT_EXPANDTABS | DT_END_ELLIPSIS,
        );
    }
    SelectObject(hdc, old_font);
}

unsafe fn draw_line(hdc: HDC, text: &str, rect: &mut RECT, align: DRAW_TEXT_FORMAT) {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    DrawTextW(hdc, &mut units, rect, align | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
}

/// まとめて貼り付けるために印を付けた順番を、`rect` の中に色の付いた札で描く（選んでいる行では
/// 色を反対にする）。
unsafe fn draw_mark(hdc: HDC, rect: RECT, order: usize, colors: Palette, selected: bool, dpi: u32) {
    let inset = ui::scale(2, dpi);
    let badge = RECT {
        // ピン留めの線に重ねない。
        left: rect.left + ui::scale(PIN_BAR_W + 1, dpi),
        top: rect.top + inset,
        right: rect.right - inset,
        bottom: rect.bottom - inset,
    };
    let (back, text) = if selected {
        (colors.sel_text, colors.sel_back)
    } else {
        (colors.sel_back, colors.sel_text)
    };
    fill(hdc, &badge, back);
    SetTextColor(hdc, COLORREF(text));
    let mut text_rect = badge;
    draw_line(hdc, &order.to_string(), &mut text_rect, DT_CENTER);
}

unsafe fn fill(hdc: HDC, rect: &RECT, color: u32) {
    let brush = CreateSolidBrush(COLORREF(color));
    FillRect(hdc, rect, brush);
    let _ = DeleteObject(brush);
}

/// 文字の幅（ピクセル）。
unsafe fn text_width(hdc: HDC, text: &str) -> i32 {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &units, &mut size);
    size.cx
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

/// 見出し（タブとページ）を描く。
unsafe fn paint_header(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut whole = RECT::default();
    let _ = GetClientRect(hwnd, &mut whole);
    let mut placed_edit = false;
    if let Some((colors, font, dpi, tab, label, keep_open)) = with_state(|st| {
        let label = if st.filtered() {
            clip_history::filtered_page_label(st.page(), st.len(), st.page_size)
        } else {
            clip_history::page_label(st.page(), st.len(), st.page_size)
        };
        (st.colors, st.font, st.dpi, st.tab, label, st.keep_open)
    }) {
        // 上の段（タブ・出したまま・ページ）と、下の段（検索の欄）。
        let rect = RECT {
            bottom: (whole.top + ui::scale(HEADER_H, dpi)).min(whole.bottom),
            ..whole
        };
        let lower = RECT {
            top: rect.bottom,
            ..whole
        };
        fill(hdc, &whole, colors.header_back);
        let old = SelectObject(hdc, font);
        SetBkMode(hdc, TRANSPARENT);
        // タブ: 選んでいるタブは一覧と同じ色にして、つながって見えるようにする。
        let pad = ui::scale(TAB_PAD, dpi);
        let mut left = rect.left;
        let mut rects = [RECT::default(); 2];
        for (i, (this, name)) in Tab::ALL.into_iter().enumerate() {
            let mut tab_rect = RECT {
                left,
                right: left + text_width(hdc, name) + pad * 2,
                ..rect
            };
            if this == tab {
                fill(hdc, &tab_rect, colors.back);
                // 選んでいるタブの下に、選んだ行と同じ色の線を引いて目立たせる。
                let bar = RECT {
                    top: tab_rect.bottom - ui::scale(2, dpi),
                    ..tab_rect
                };
                fill(hdc, &bar, colors.sel_back);
                SetTextColor(hdc, COLORREF(colors.text));
            } else {
                SetTextColor(hdc, COLORREF(colors.header_text));
            }
            rects[i] = tab_rect;
            draw_line(hdc, name, &mut tab_rect, DT_CENTER);
            left = tab_rect.right;
        }
        // ページ（右寄せ）と、その左の「出したまま」（押すと切り替わる。ON のときは目立たせる）。
        let right = rect.right - ui::scale(PAD + 2, dpi);
        let keep_name = "出したまま";
        let keep_w = text_width(hdc, keep_name) + pad;
        let page_left = (right - text_width(hdc, &label) - pad).max(left);
        let mut keep_rect = RECT {
            left: page_left - keep_w,
            right: page_left,
            ..rect
        };
        // 幅が足りないときは「出したまま」を出さない（タブに重ねない）。
        if keep_rect.left < left {
            keep_rect = RECT::default();
        } else {
            if keep_open {
                fill(hdc, &keep_rect, colors.back);
                let bar = RECT {
                    top: keep_rect.bottom - ui::scale(2, dpi),
                    ..keep_rect
                };
                fill(hdc, &bar, colors.sel_back);
                SetTextColor(hdc, COLORREF(colors.text));
            } else {
                SetTextColor(hdc, COLORREF(blend(colors.header_text, colors.header_back, 55)));
            }
            let mut text_rect = keep_rect;
            draw_line(hdc, keep_name, &mut text_rect, DT_CENTER);
        }
        with_state(|st| {
            st.tab_rects = rects;
            st.keep_rect = keep_rect;
        });
        SetTextColor(hdc, COLORREF(colors.header_text));
        let mut page_rect = RECT {
            left: page_left,
            right,
            ..rect
        };
        draw_line(hdc, &label, &mut page_rect, DT_RIGHT | DT_END_ELLIPSIS);
        placed_edit = paint_filter_row(hdc, lower, colors, dpi);
        SelectObject(hdc, old);
    }
    let _ = EndPaint(hwnd, &ps);
    if placed_edit {
        // 検索の欄の範囲が変わったら、入力欄を動かす（描き終えてから）。
        if let Ok(parent) = windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd) {
            place_search_edit(parent);
        }
    }
}

/// 見出しの下の段（検索の欄）を描く。入力欄に入力しているなら `true`（入力欄を置き直す）。
unsafe fn paint_filter_row(hdc: HDC, row: RECT, colors: Palette, dpi: u32) -> bool {
    let Some((query, searching, tab, kind, marks)) =
        with_state(|st| (st.query.clone(), st.searching, st.tab, st.kind, st.marks.len()))
    else {
        return false;
    };
    let pad = ui::scale(PAD, dpi);
    let inset = ui::scale(2, dpi);
    // 履歴のタブでは、左に出す種類（押すと切り替わる。絞り込んでいるときは目立たせる）。
    let mut kind_rect = RECT::default();
    let mut left = row.left;
    if tab == Tab::History {
        let widest = [KindFilter::All, KindFilter::Text, KindFilter::Image]
            .iter()
            .map(|k| text_width(hdc, k.label()))
            .max()
            .unwrap_or(0);
        kind_rect = RECT {
            left: row.left,
            right: row.left + widest + ui::scale(TAB_PAD, dpi) * 2,
            ..row
        };
        if kind != KindFilter::All {
            fill(hdc, &kind_rect, colors.back);
            let bar = RECT {
                top: kind_rect.bottom - ui::scale(2, dpi),
                ..kind_rect
            };
            fill(hdc, &bar, colors.sel_back);
            SetTextColor(hdc, COLORREF(colors.text));
        } else {
            SetTextColor(hdc, COLORREF(colors.header_text));
        }
        let mut text_rect = kind_rect;
        draw_line(hdc, kind.label(), &mut text_rect, DT_CENTER);
        left = kind_rect.right;
    }
    with_state(|st| st.kind_rect = kind_rect);
    // 印を付けたものがあれば、右にその件数を出す。
    let mut right = row.right - pad;
    if marks > 0 {
        let label = format!("選択 {marks}");
        let mut marks_rect = RECT {
            left: (right - text_width(hdc, &label)).max(left),
            right,
            ..row
        };
        right = marks_rect.left - pad;
        SetTextColor(hdc, COLORREF(colors.header_text));
        draw_line(hdc, &label, &mut marks_rect, DT_RIGHT);
    }
    let search = RECT {
        left: left + pad,
        top: row.top + inset / 2,
        right: right.max(left + pad),
        bottom: row.bottom - inset,
    };
    // 入力欄のような枠（入力しているときは、選んだ行と同じ色の枠にする）。
    fill(hdc, &search, if searching { colors.sel_back } else { colors.border });
    let inner = RECT {
        left: search.left + 1,
        top: search.top + 1,
        right: search.right - 1,
        bottom: search.bottom - 1,
    };
    fill(hdc, &inner, colors.back);
    if !searching {
        // 言葉が無ければ使い方を薄く、あれば絞り込んでいる言葉を出す。
        let (text, color) = if query.is_empty() {
            ("検索（Ctrl+F）".to_string(), colors.sub)
        } else {
            (format!("検索: {query}"), colors.text)
        };
        SetTextColor(hdc, COLORREF(color));
        let mut text_rect = RECT {
            left: inner.left + inset * 2,
            ..inner
        };
        draw_line(hdc, &text, &mut text_rect, DT_LEFT | DT_END_ELLIPSIS);
    }
    with_state(|st| st.search_rect = search);
    searching
}

/// 選んでいるものが 1 行に収まらない（複数行か、途中で切れている）とき、または定型文のときは、
/// 選んだ行の横に全文の吹き出しを出す。画像のときは、大きめの絵を出す。それ以外は隠す。
unsafe fn update_tip(hwnd: HWND) {
    let Some((tip, dpi, font, selected, page, size, tab, layout)) = with_state(|st| {
        (st.tip, st.dpi, st.font, st.selected(), st.page(), st.page_size, st.tab, st.layout)
    }) else {
        return;
    };
    if tip.is_invalid() {
        return;
    }
    let list = control(hwnd, ID_LIST);
    let row = selected.and_then(|s| s.checked_sub(page * size)).filter(|r| *r < size);
    let content = with_state(|st| {
        let pos = selected?;
        let image = st.thumb_at(pos);
        let info = st.row(pos)?;
        Some((st.full_text(pos), image, info.shown, info.lines))
    })
    .flatten();
    let (Some(row), Some((full, image, shown, lines))) = (row, content) else {
        let _ = ShowWindow(tip, SW_HIDE);
        return;
    };
    let mut item_rect = RECT::default();
    SendMessageW(
        list,
        LB_GETITEMRECT,
        WPARAM(row),
        LPARAM(&mut item_rect as *mut RECT as isize),
    );
    let pad = ui::scale(TIP_PAD, dpi);
    let max_w = ui::scale(TIP_MAX_W, dpi);
    let max_h = ui::scale(TIP_MAX_H, dpi);

    let (text, width, height) = if let Some(thumb) = &image {
        // 画像: 絵の大きさに合わせる。
        let (w, h) = clip_images::thumb_size(
            thumb.width,
            thumb.height,
            (max_w - pad * 2).max(1) as u32,
            (max_h - pad * 2).max(1) as u32,
        );
        (String::new(), w as i32 + pad * 2 + 2, h as i32 + pad * 2 + 2)
    } else if let Some(full) = full {
        let fits = if layout == Layout::Band {
            // カードには数行が出ているので、長いものだけ吹き出しにする。
            lines <= 5 && full.chars().count() <= 120
        } else {
            // 一覧の行の、文字を書く幅に収まるか。
            let available = text_rect(&item_rect, lines, dpi);
            let hdc = GetDC(list);
            let old = SelectObject(hdc, font);
            let shown_width = text_width(hdc, &shown);
            SelectObject(hdc, old);
            ReleaseDC(list, hdc);
            lines <= 1 && shown_width <= available.right - available.left
        };
        if fits && (tab == Tab::History || layout == Layout::Band) {
            let _ = ShowWindow(tip, SW_HIDE);
            return;
        }
        // 吹き出しの大きさを、中身に合わせて決める（上限あり）。
        let text = clip_history::tip_text(&full);
        let mut calc = RECT {
            left: 0,
            top: 0,
            right: max_w - pad * 2,
            bottom: 0,
        };
        let hdc = GetDC(tip);
        let old = SelectObject(hdc, font);
        let mut units: Vec<u16> = text.encode_utf16().collect();
        // DT_EDITCONTROL: 空白の無い長い行（URL など）も、幅で折り返す。
        DrawTextW(
            hdc,
            &mut units,
            &mut calc,
            DT_CALCRECT | DT_WORDBREAK | DT_EDITCONTROL | DT_NOPREFIX | DT_EXPANDTABS,
        );
        SelectObject(hdc, old);
        ReleaseDC(tip, hdc);
        let width = ((calc.right - calc.left) + pad * 2 + 2).min(max_w);
        let height = ((calc.bottom - calc.top) + pad * 2 + 2).min(max_h);
        (text, width, height)
    } else {
        let _ = ShowWindow(tip, SW_HIDE);
        return;
    };

    let mut window = RECT::default();
    let _ = GetWindowRect(hwnd, &mut window);
    let mut top_left = POINT {
        x: item_rect.left,
        y: item_rect.top,
    };
    let _ = ClientToScreen(list, &mut top_left);
    let work = work_area(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST));
    let gap = ui::scale(4, dpi);
    let (x, y) = if layout == Layout::Band {
        // 帯: 選んだカードの上。
        (top_left.x, window.top - gap - height)
    } else {
        // 画面の右（入らなければ左）、選んでいる行の高さ。
        let mut x = window.right + gap;
        if x + width > work.right {
            x = window.left - gap - width;
        }
        (x, top_left.y)
    };
    let y = y.clamp(work.top, (work.bottom - height).max(work.top));
    let x = x.clamp(work.left, (work.right - width).max(work.left));
    with_state(|st| {
        st.tip_text = text;
        st.tip_image = image;
    });
    let _ = SetWindowPos(
        tip,
        HWND_TOPMOST,
        x,
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
        // 1 ピクセルの枠: 画面全体を枠の色で塗り、その内側に見出しと一覧を置いている。
        WM_ERASEBKGND => {
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            let border = with_state(|st| st.colors.border).unwrap_or(0);
            fill(HDC(wparam.0 as *mut core::ffi::c_void), &rect, border);
            LRESULT(1)
        }
        // 一覧の、行の無いところの色。
        WM_CTLCOLORLISTBOX => {
            let brush = with_state(|st| st.back_brush).unwrap_or_default();
            LRESULT(brush.0 as isize)
        }
        // 押されても前面にならない。
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // 元のアプリが前面でなくなったら（Alt+Tab などでほかのウィンドウに移ったら）、何もせずに
        // 閉じる。一瞬だけ別のものが前面になっても閉じないよう、続けて何回か見てから決める。
        WM_TIMER if wparam.0 == TIMER_WATCH => {
            if !MENU_OPEN.load(Ordering::SeqCst) && !is_done() {
                let foreground = GetForegroundWindow();
                let left = with_state(|st| {
                    let elsewhere = !st.target.is_invalid()
                        && !foreground.is_invalid()
                        && foreground != st.target
                        && foreground != hwnd;
                    st.away = if elsewhere { st.away + 1 } else { 0 };
                    st.away >= WATCH_STRIKES
                })
                .unwrap_or(false);
                if left {
                    with_state(|st| st.outcome.left_for_other_window = true);
                    finish(None);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            finish(None);
            let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 見出しのプロシージャ。クリックしたタブに切り替える。
unsafe extern "system" fn header_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint_header(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as u16 as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            let hit = |r: &RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
            let (rects, keep, kind, search) =
                with_state(|st| (st.tab_rects, st.keep_rect, st.kind_rect, st.search_rect))
                    .unwrap_or_default();
            let parent = windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd).unwrap_or_default();
            if let Some(i) = rects.iter().position(hit) {
                switch_tab(parent, Tab::from_index(i));
            } else if hit(&keep) {
                toggle_keep_open(parent);
            } else if hit(&kind) {
                cycle_kind(parent);
            } else if hit(&search) {
                enter_search(parent);
            }
            LRESULT(0)
        }
        // 検索の入力欄の文字が変わった。
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            if id == ID_SEARCH && code == EN_CHANGE {
                let parent =
                    windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd).unwrap_or_default();
                on_search_changed(parent);
            }
            LRESULT(0)
        }
        // 検索の入力欄の色（一覧と同じ配色にする）。
        WM_CTLCOLOREDIT => {
            let Some((colors, brush)) = with_state(|st| (st.colors, st.back_brush)) else {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            };
            let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
            SetTextColor(hdc, COLORREF(colors.text));
            SetBkColor(hdc, COLORREF(colors.back));
            LRESULT(brush.0 as isize)
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
            if let Some((text, image, font, dpi, colors)) = with_state(|st| {
                (st.tip_text.clone(), st.tip_image.clone(), st.font, st.dpi, st.colors)
            }) {
                fill(hdc, &rect, colors.tip_back);
                if let Some(thumb) = image {
                    let pad = ui::scale(TIP_PAD, dpi);
                    clip_image_store::draw_thumb(
                        hdc,
                        &thumb,
                        rect.left + pad,
                        rect.top + pad,
                        rect.right - rect.left - pad * 2,
                        rect.bottom - rect.top - pad * 2,
                    );
                    let _ = EndPaint(hwnd, &ps);
                    return LRESULT(0);
                }
                let old = SelectObject(hdc, font);
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(colors.tip_text));
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
                    DT_WORDBREAK | DT_EDITCONTROL | DT_NOPREFIX | DT_EXPANDTABS | DT_END_ELLIPSIS,
                );
                SelectObject(hdc, old);
            } else {
                fill(hdc, &rect, GetSysColor(COLOR_INFOBK));
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
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    // 2 回目以降は登録済みで失敗するが、そのまま使える。
    RegisterClassW(&class);
    CreateWindowExW(
        WINDOW_EX,
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

/// 見出し（自分で描くウィンドウ）を作る。
unsafe fn create_header(parent: HWND) {
    let Ok(instance) = GetModuleHandleW(None) else {
        return;
    };
    let class_name = w!("AtaiPasteHistoryHeader");
    let class = WNDCLASSW {
        lpfnWndProc: Some(header_proc),
        hInstance: instance.into(),
        lpszClassName: class_name,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassW(&class);
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class_name,
        w!(""),
        // 検索の入力欄（子）の上には描かない。
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0),
        0,
        0,
        0,
        0,
        parent,
        HMENU(ID_HEADER as isize as *mut core::ffi::c_void),
        instance,
        None,
    );
}

/// 検索の入力欄（見出しの子。検索を始めるまで隠しておく）を作る。
unsafe fn create_search_edit(header: HWND, font: HFONT) {
    let Ok(instance) = GetModuleHandleW(None) else {
        return;
    };
    let Ok(edit) = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        w!("EDIT"),
        w!(""),
        WINDOW_STYLE(WS_CHILD.0 | ES_AUTOHSCROLL as u32),
        0,
        0,
        0,
        0,
        header,
        HMENU(ID_SEARCH as isize as *mut core::ffi::c_void),
        instance,
        None,
    ) else {
        return;
    };
    SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
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
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
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
