//! IME 入力モードの画面中央表示。
//!
//! Windows 10 1703 以降の標準機能と同じく、IME の入力モード（あ / A / カ / ｶ / Ａ）を
//! 切り替えた瞬間に、画面に大きくモードを表示してすぐフェードアウトさせる。
//!
//! ## 仕組み
//! - 専用スレッドで 100ms ごとに、前面ウィンドウのフォーカス先の IME 状態を調べる。
//!   `ImmGetDefaultIMEWnd` で得た IME ウィンドウへ `WM_IME_CONTROL` を
//!   `SendMessageTimeoutW`（`SMTO_ABORTIFHUNG`）で送るので、相手が固まっていても
//!   こちらが巻き込まれない。管理者権限のウィンドウなど、取れない場合は何もしない。
//! - 表示するかどうかの判断（同じウィンドウ内でモードが変わったときだけ）や
//!   フェード・配置の計算は [`crate::ime_logic`] にあり、Linux でもテストできる。
//! - 表示ウィンドウはクリック透過（`WS_EX_TRANSPARENT`）・非アクティブ
//!   （`WS_EX_NOACTIVATE` と `WM_MOUSEACTIVATE` で `MA_NOACTIVATE`）・タスクバー非表示
//!   （`WS_EX_TOOLWINDOW`）・最前面（`WS_EX_TOPMOST`）。表示も `SWP_NOACTIVATE` で
//!   行うため、利用者の入力先を奪わない。
//! - 不透明度はレイヤードウィンドウの `SetLayeredWindowAttributes` で変え、角丸は
//!   `SetWindowRgn` で切り抜く。
//!
//! ## 出し方の設定（[`Params`]）
//! - 位置: 前面ウィンドウのあるモニターの中央（既定）・マウスカーソルの近く・
//!   入力位置（キャレット）の近く。入力位置は `GetGUIThreadInfo` の `rcCaret` から取るが、
//!   独自に文字を描くアプリ（ブラウザーなど）では取れないことがあり、その場合は中央に出す。
//! - 色（濃い色・明るい色）と不透明度。
//! - 全画面のアプリの間は出さない（`SHQueryUserNotificationState`）。
//!
//! いずれも [`set_params`] で起動中にも変えられ、設定画面からは [`preview`] で試しに
//! 表示できる。キーボードフック（[`crate::keyboard`]）とは別スレッドで動くため、
//! Ctrl+B のリマップには影響しない。

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DeleteObject,
    DrawTextW, EndPaint, FillRect, GetMonitorInfoW, InvalidateRect, MonitorFromPoint,
    MonitorFromWindow, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, ANTIALIASED_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, FF_DONTCARE, FW_BOLD, HFONT, HGDIOBJ, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::Ime::ImmGetDefaultIMEWnd;
use windows::Win32::UI::Shell::{
    SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetForegroundWindow, GetGUIThreadInfo, GetMessageW, GetWindowThreadProcessId, KillTimer,
    PostMessageW, PostThreadMessageW, RegisterClassW, SendMessageTimeoutW,
    SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow, TranslateMessage,
    GUITHREADINFO, HTTRANSPARENT, HWND_TOPMOST, LWA_ALPHA, MA_NOACTIVATE, MSG, SMTO_ABORTIFHUNG,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WM_APP, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT,
    WM_QUIT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::config::ImeIndicatorSettings;
use crate::ime_logic::{self, ImeMode, ModeTracker, Position, Theme};

/// IME の状態を調べる間隔（ミリ秒）。
const POLL_INTERVAL_MS: u32 = 100;
/// フェード中の再描画の間隔（ミリ秒）。
const ANIM_INTERVAL_MS: u32 = 16;
/// IME への問い合わせのタイムアウト（ミリ秒）。相手が固まっていても待たない。
const QUERY_TIMEOUT_MS: u32 = 50;
/// マウスカーソルや入力位置から離す距離（96 DPI 基準のピクセル）。
const NEAR_GAP: i32 = 16;

/// `WM_IME_CONTROL`。IME ウィンドウへ状態を問い合わせるメッセージ。
const WM_IME_CONTROL: u32 = 0x0283;
/// `IMC_GETCONVERSIONMODE`。
const IMC_GETCONVERSIONMODE: usize = 0x0001;
/// `IMC_GETOPENSTATUS`。
const IMC_GETOPENSTATUS: usize = 0x0005;

/// タイマーの識別子: IME 状態の監視。
const TIMER_POLL: usize = 1;
/// タイマーの識別子: フェード。
const TIMER_ANIM: usize = 2;

/// 試しに表示する（設定画面のプレビュー）。表示の設定は [`PREVIEW`] に置いてから送る。
const WM_APP_PREVIEW: u32 = WM_APP + 1;
/// お知らせを表示する（キー割り当ての結果など）。文字は [`NOTICE`] に置いてから送る。
const WM_APP_NOTICE: u32 = WM_APP + 2;

/// 機能の ON/OFF。トレイメニューから切り替えられ、監視スレッドが毎回見る。
static ENABLED: AtomicBool = AtomicBool::new(true);
/// 表示の設定。
static PARAMS: Mutex<Params> = Mutex::new(Params::DEFAULT);
/// プレビューで使う表示の設定（[`preview`] が置き、監視スレッドが取り出す）。
static PREVIEW: Mutex<Option<Params>> = Mutex::new(None);
/// お知らせに出す文字（[`notify`] が置き、監視スレッドが取り出す）。
static NOTICE: Mutex<Option<&'static str>> = Mutex::new(None);
/// 表示ウィンドウ（プレビューの依頼先）。0 は未作成。
static WINDOW: AtomicIsize = AtomicIsize::new(0);

/// 表示の時間（ミリ秒）。
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// 消え始めるまでの表示時間。
    pub hold_ms: u64,
    /// フェードアウトにかける時間。
    pub fade_ms: u64,
}

/// 表示の設定。
#[derive(Debug, Clone, Copy)]
pub struct Params {
    pub timing: Timing,
    /// 一辺（96 DPI 基準のピクセル）。
    pub size: u32,
    pub position: Position,
    pub theme: Theme,
    /// 不透明度（%）。
    pub opacity: u32,
    /// 全画面のアプリの間は表示しない。
    pub hide_in_fullscreen: bool,
}

impl Params {
    const DEFAULT: Params = Params {
        timing: Timing {
            hold_ms: 400,
            fade_ms: ime_logic::DEFAULT_FADE_MS,
        },
        size: 120,
        position: Position::Center,
        theme: Theme::Dark,
        opacity: ime_logic::DEFAULT_OPACITY,
        hide_in_fullscreen: false,
    };

    /// 設定ファイルの値から作る（範囲外は丸め、知らない値は既定にする）。
    pub fn from_settings(settings: &ImeIndicatorSettings) -> Params {
        Params {
            timing: Timing {
                hold_ms: settings.hold_ms,
                fade_ms: settings.fade_ms,
            },
            size: settings.size,
            position: Position::from_setting(&settings.position),
            theme: Theme::from_setting(&settings.theme),
            opacity: settings.opacity,
            hide_in_fullscreen: settings.hide_in_fullscreen,
        }
        .clamped()
    }

    /// 設定できる範囲に丸める。極端な値で画面を覆ったり、見えなくなったりしないように。
    fn clamped(self) -> Params {
        Params {
            timing: Timing {
                hold_ms: self
                    .timing
                    .hold_ms
                    .clamp(ime_logic::HOLD_MS_MIN, ime_logic::HOLD_MS_MAX),
                fade_ms: self
                    .timing
                    .fade_ms
                    .clamp(ime_logic::FADE_MS_MIN, ime_logic::FADE_MS_MAX),
            },
            size: self.size.clamp(ime_logic::SIZE_MIN, ime_logic::SIZE_MAX),
            opacity: self
                .opacity
                .clamp(ime_logic::OPACITY_MIN, ime_logic::OPACITY_MAX),
            ..self
        }
    }
}

/// 表示の設定を変える（起動時と、設定画面で保存したとき）。範囲外は丸める。
pub fn set_params(params: Params) {
    let params = params.clamped();
    log::info!("入力モード表示の設定: {params:?}");
    *PARAMS.lock().unwrap_or_else(|p| p.into_inner()) = params;
}

fn current_params() -> Params {
    *PARAMS.lock().unwrap_or_else(|p| p.into_inner())
}

/// 指定した設定で、試しに「あ」を表示する（機能が OFF でも、全画面のアプリの間でも表示する）。
/// 監視スレッドが動いていなければ何もせず `false` を返す。
pub fn preview(params: Params) -> bool {
    let window = WINDOW.load(Ordering::SeqCst);
    if window == 0 {
        return false;
    }
    *PREVIEW.lock().unwrap_or_else(|p| p.into_inner()) = Some(params.clamped());
    unsafe {
        PostMessageW(
            HWND(window as *mut core::ffi::c_void),
            WM_APP_PREVIEW,
            WPARAM(0),
            LPARAM(0),
        )
        .is_ok()
    }
}

/// 入力モードと同じ見た目で、短いお知らせ（例 `"固定"`）を表示する。キー割り当ての結果を
/// 知らせるのに使う。入力モード表示が OFF でも表示する。監視スレッドが動いていなければ `false`。
pub fn notify(label: &'static str) -> bool {
    let window = WINDOW.load(Ordering::SeqCst);
    if window == 0 {
        return false;
    }
    *NOTICE.lock().unwrap_or_else(|p| p.into_inner()) = Some(label);
    unsafe {
        PostMessageW(
            HWND(window as *mut core::ffi::c_void),
            WM_APP_NOTICE,
            WPARAM(0),
            LPARAM(0),
        )
        .is_ok()
    }
}

/// 機能の ON/OFF を切り替える（トレイメニューから呼ぶ）。
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::SeqCst);
}

/// いま有効かどうか。
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// 監視スレッドのハンドル。[`Indicator::stop`] で確実に止めること。
pub struct Indicator {
    thread: Option<JoinHandle<()>>,
    thread_id: u32,
}

impl Indicator {
    /// 監視スレッドを止め、表示ウィンドウを破棄して終わるのを待つ。
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(thread) = self.thread.take() {
            if self.thread_id != 0 {
                unsafe {
                    let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
                }
            }
            let _ = thread.join();
        }
    }
}

impl Drop for Indicator {
    fn drop(&mut self) {
        // stop() を呼び忘れても、スレッドを残したまま終わらないようにする。
        self.shutdown();
    }
}

/// 監視を始める。表示ウィンドウの作成に失敗した場合は `None`（機能を使わないだけで、
/// アプリの他の機能には影響しない）。
pub fn start(settings: &ImeIndicatorSettings) -> Option<Indicator> {
    set_enabled(settings.enabled);
    set_params(Params::from_settings(settings));

    let (tx, rx) = mpsc::channel::<u32>();
    let thread = std::thread::spawn(move || thread_main(tx));
    match rx.recv() {
        Ok(0) | Err(_) => {
            let _ = thread.join();
            log::warn!("入力モード表示を開始できませんでした");
            None
        }
        Ok(thread_id) => Some(Indicator {
            thread: Some(thread),
            thread_id,
        }),
    }
}

/// 監視スレッドの状態。ウィンドウプロシージャからも触るため、スレッドローカルに置く。
struct State {
    hwnd: HWND,
    tracker: ModeTracker,
    /// 表示中の内容。
    showing: Option<Showing>,
    /// 作成済みのフォントと、その文字の高さ（DPI が変わったら作り直す）。
    font: Option<(HFONT, i32)>,
    /// 現在の一辺（実際のピクセル）。描画に使う。
    size: i32,
    /// 前回、IME の状態を取れたか（取れる・取れないが変わったときだけ記録に残す）。
    last_query_ok: Option<bool>,
}

/// 表示中の内容。
#[derive(Clone, Copy)]
struct Showing {
    /// 表示する文字（入力モードの `あ` など。お知らせでは 2〜3 文字）。
    label: &'static str,
    started: Instant,
    timing: Timing,
    /// 最大の不透明度（0〜255）。
    alpha: u8,
    theme: Theme,
    /// 設定画面のプレビューやお知らせか（機能が OFF でも消さない）。
    forced: bool,
}

/// 前面ウィンドウから読み取った状態。
struct Observed {
    /// フォーカス先のウィンドウ（「同じウィンドウ内での変化か」の判定に使う）。
    focus: HWND,
    mode: ImeMode,
    /// 入力位置（キャレット）の画面座標。取れなければ `None`。
    caret: Option<RECT>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn thread_main(ready: mpsc::Sender<u32>) {
    unsafe {
        let Some(hwnd) = create_window() else {
            let _ = ready.send(0);
            return;
        };
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                tracker: ModeTracker::default(),
                showing: None,
                font: None,
                size: 0,
                last_query_ok: None,
            })
        });
        WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);

        SetTimer(hwnd, TIMER_POLL, POLL_INTERVAL_MS, None);
        let _ = ready.send(GetCurrentThreadId());

        let mut msg = MSG::default();
        loop {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            // 0 = WM_QUIT、-1 = エラー。どちらも抜ける。
            if ret.0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 後始末。タイマー・フォント・ウィンドウを確実に破棄する。
        WINDOW.store(0, Ordering::SeqCst);
        let _ = KillTimer(hwnd, TIMER_POLL);
        let _ = KillTimer(hwnd, TIMER_ANIM);
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().take() {
                if let Some((font, _)) = state.font {
                    let _ = DeleteObject(font);
                }
            }
        });
        let _ = DestroyWindow(hwnd);
    }
}

/// 表示用のウィンドウを作る（最初は非表示）。
unsafe fn create_window() -> Option<HWND> {
    let instance = GetModuleHandleW(None).ok()?;
    let class_name = w!("AtaiPasteImeIndicator");
    let class = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance.into(),
        lpszClassName: class_name,
        ..Default::default()
    };
    if RegisterClassW(&class) == 0 {
        log::warn!("入力モード表示のウィンドウクラスを登録できませんでした");
        return None;
    }
    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
        class_name,
        w!("アタイの貼り付け 入力モード"),
        WS_POPUP,
        0,
        0,
        1,
        1,
        None,
        None,
        instance,
        None,
    )
    .ok()?;
    Some(hwnd)
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // クリックされてもアクティブにならない。
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // マウスは下のウィンドウへ素通しする。
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_TIMER => {
            match wparam.0 {
                TIMER_POLL => on_poll(),
                TIMER_ANIM => on_anim(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_PAINT => {
            on_paint(hwnd);
            LRESULT(0)
        }
        WM_APP_PREVIEW => {
            let params = PREVIEW.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Some(params) = params {
                // 入力位置はプレビューでは取れない（前面は設定画面）ので、中央に出す。
                show(ImeMode::Hiragana.label(), None, &params, true);
            }
            LRESULT(0)
        }
        WM_APP_NOTICE => {
            let label = NOTICE.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Some(label) = label {
                show(label, None, &current_params(), true);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 100ms ごとの監視。
unsafe fn on_poll() {
    let enabled = is_enabled();
    let observed = if enabled { query_foreground_ime() } else { None };

    let to_show = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let state = s.as_mut()?;
        if !enabled {
            state.tracker.reset();
            state.last_query_ok = None;
            return None;
        }
        let ok = observed.is_some();
        if state.last_query_ok != Some(ok) {
            state.last_query_ok = Some(ok);
            if ok {
                log::debug!("入力モード: IME の状態を読み取れるようになりました");
            } else {
                log::debug!(
                    "入力モード: 前面のウィンドウ（{}）の IME の状態を読み取れません",
                    crate::excel_check::foreground_process_name().unwrap_or_else(|| "不明".into())
                );
            }
        }
        state
            .tracker
            .observe(observed.as_ref().map(|o| (o.focus.0 as isize, o.mode)))
    });

    if !enabled {
        // OFF にしたら表示中のものも消す。ただし設定画面のプレビューは残す。
        let previewing = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .and_then(|state| state.showing)
                .is_some_and(|showing| showing.forced)
        });
        if !previewing {
            hide();
        }
        return;
    }
    if let (Some(mode), Some(observed)) = (to_show, observed) {
        let params = current_params();
        if params.hide_in_fullscreen && is_fullscreen_app_running() {
            log::debug!("入力モード: {} に切り替わりました（全画面のアプリのため表示しません）", mode.label());
            return;
        }
        log::debug!("入力モード: {} を表示します（位置: {}）", mode.label(), params.position.as_setting());
        show(mode.label(), observed.caret, &params, false);
    }
}

/// 全画面のアプリ（ゲーム・動画・プレゼンテーションなど）が前面にあるか。
unsafe fn is_fullscreen_app_running() -> bool {
    matches!(
        SHQueryUserNotificationState(),
        Ok(state) if state == QUNS_BUSY
            || state == QUNS_RUNNING_D3D_FULL_SCREEN
            || state == QUNS_PRESENTATION_MODE
    )
}

/// 前面ウィンドウのフォーカス先の IME 状態を調べる。取れなければ `None`。
unsafe fn query_foreground_ime() -> Option<Observed> {
    let foreground = GetForegroundWindow();
    if foreground.is_invalid() {
        return None;
    }

    let mut pid = 0u32;
    let thread_id = GetWindowThreadProcessId(foreground, Some(&mut pid));
    // 自分自身のウィンドウは対象外。
    if thread_id == 0 || pid == GetCurrentProcessId() {
        return None;
    }

    // フォーカス先（入力欄）と入力位置を取る。フォーカス先が取れなければ前面ウィンドウそのもの。
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let have_info = GetGUIThreadInfo(thread_id, &mut info).is_ok();
    let focus = if have_info && !info.hwndFocus.is_invalid() {
        info.hwndFocus
    } else {
        foreground
    };
    let caret = if have_info { caret_rect(&info) } else { None };

    let ime_window = ImmGetDefaultIMEWnd(focus);
    if ime_window.is_invalid() {
        return None;
    }

    let open = send_ime_control(ime_window, IMC_GETOPENSTATUS)?;
    let conversion = send_ime_control(ime_window, IMC_GETCONVERSIONMODE)?;
    Some(Observed {
        focus,
        mode: ime_logic::mode_from_status(open != 0, conversion as u32),
        caret,
    })
}

/// 入力位置（キャレット）を画面座標で返す。キャレットを使っていないアプリでは `None`。
pub(crate) unsafe fn caret_rect(info: &GUITHREADINFO) -> Option<RECT> {
    if info.hwndCaret.is_invalid() {
        return None;
    }
    let rc = info.rcCaret;
    if rc.right <= rc.left && rc.bottom <= rc.top {
        return None;
    }
    let mut top_left = POINT {
        x: rc.left,
        y: rc.top,
    };
    let mut bottom_right = POINT {
        x: rc.right,
        y: rc.bottom,
    };
    if !ClientToScreen(info.hwndCaret, &mut top_left).as_bool()
        || !ClientToScreen(info.hwndCaret, &mut bottom_right).as_bool()
    {
        return None;
    }
    Some(RECT {
        left: top_left.x,
        top: top_left.y,
        right: bottom_right.x,
        bottom: bottom_right.y,
    })
}

/// IME ウィンドウへ `WM_IME_CONTROL` を送り、結果を返す。
///
/// 相手が固まっている・権限が上（UIPI で遮断される）などで届かなければ `None`。
unsafe fn send_ime_control(ime_window: HWND, command: usize) -> Option<usize> {
    let mut result = 0usize;
    let sent = SendMessageTimeoutW(
        ime_window,
        WM_IME_CONTROL,
        WPARAM(command),
        LPARAM(0),
        SMTO_ABORTIFHUNG,
        QUERY_TIMEOUT_MS,
        Some(&mut result),
    );
    (sent.0 != 0).then_some(result)
}

/// モニターの作業領域と DPI。
unsafe fn monitor_info(monitor: HMONITOR) -> Option<(RECT, u32)> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !GetMonitorInfoW(monitor, &mut info).as_bool() {
        return None;
    }
    let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
    let dpi = if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok() {
        dpi_x
    } else {
        96
    };
    Some((info.rcWork, dpi))
}

/// 表示する位置（左上）と一辺（実際のピクセル）を決める。
unsafe fn placement(caret: Option<RECT>, params: &Params) -> Option<(i32, i32, i32)> {
    let base = params.size as i32;
    let near = |point: POINT, below: i32| -> Option<(i32, i32, i32)> {
        let (work, dpi) = monitor_info(MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST))?;
        let size = ime_logic::scale_for_dpi(base, dpi);
        let gap = ime_logic::scale_for_dpi(NEAR_GAP, dpi);
        let (x, y) = ime_logic::origin_near_point(
            point.x,
            point.y,
            below,
            (work.left, work.top, work.right, work.bottom),
            size,
            gap,
        );
        Some((x, y, size))
    };

    match (params.position, caret) {
        (Position::Mouse, _) => {
            let mut cursor = POINT::default();
            if GetCursorPos(&mut cursor).is_ok() {
                // マウスカーソルの絵（およそ 20px）にかからないよう、その下に置く。
                let below = cursor.y + 20;
                if let Some(p) = near(cursor, below) {
                    return Some(p);
                }
            }
        }
        (Position::Caret, Some(rc)) => {
            let point = POINT {
                x: rc.left,
                y: rc.top,
            };
            if let Some(p) = near(point, rc.bottom) {
                return Some(p);
            }
        }
        _ => {}
    }

    // 画面中央（入力位置が取れなかった場合も）。前面（最上位）ウィンドウのあるモニターの
    // 中央に、そのモニターの拡大率で出す。前面ウィンドウ自身の DPI（GetDpiForWindow）は使わない。
    // 高 DPI に対応していないアプリでは拡大率にかかわらず 96 を返し、表示が小さくなるため。
    let foreground = GetForegroundWindow();
    let monitor = MonitorFromWindow(foreground, MONITOR_DEFAULTTONEAREST);
    let (work, dpi) = monitor_info(monitor)?;
    let size = ime_logic::scale_for_dpi(base, dpi);
    let (x, y) = ime_logic::centered_origin(work.left, work.top, work.right, work.bottom, size);
    Some((x, y, size))
}

/// 文字（入力モードやお知らせ）を表示する。`forced` は設定画面のプレビューやお知らせか。
unsafe fn show(label: &'static str, caret: Option<RECT>, params: &Params, forced: bool) {
    let Some((x, y, size)) = placement(caret, params) else {
        return;
    };
    let alpha = ime_logic::opacity_to_alpha(params.opacity);

    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(state) = s.as_mut() else { return };

        // 文字の高さは一辺の 55%。2 文字以上のお知らせは、横に収まるよう小さくする。
        // 高さが変わったときだけフォントを作り直す。
        let chars = label.chars().count().max(1) as i32;
        let font_height = (size * 55 / 100).min(size * 80 / 100 / chars);
        if state.font.map(|(_, h)| h) != Some(font_height) {
            if let Some((old, _)) = state.font.take() {
                let _ = DeleteObject(old);
            }
            let font = CreateFontW(
                -font_height,
                0,
                0,
                0,
                FW_BOLD.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET.0 as u32,
                OUT_DEFAULT_PRECIS.0 as u32,
                CLIP_DEFAULT_PRECIS.0 as u32,
                ANTIALIASED_QUALITY.0 as u32,
                (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
                w!("Yu Gothic UI"),
            );
            if !font.is_invalid() {
                state.font = Some((font, font_height));
            }
        }

        state.size = size;
        state.showing = Some(Showing {
            label,
            started: Instant::now(),
            timing: params.timing,
            alpha,
            theme: params.theme,
            forced,
        });
        let hwnd = state.hwnd;

        let radius = size / 4;
        let region = CreateRoundRectRgn(0, 0, size + 1, size + 1, radius, radius);
        // SetWindowRgn が成功すると、リージョンの所有権はウィンドウへ移る（削除しない）。
        SetWindowRgn(hwnd, region, true);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            x,
            y,
            size,
            size,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let _ = InvalidateRect(hwnd, None, true);
        SetTimer(hwnd, TIMER_ANIM, ANIM_INTERVAL_MS, None);
    });
}

/// フェードの進行。保持時間の間はそのまま、続いて薄くして消す。
unsafe fn on_anim() {
    let (hwnd, alpha) = match STATE.with(|s| {
        let s = s.borrow();
        let state = s.as_ref()?;
        let showing = state.showing?;
        let elapsed = showing.started.elapsed().as_millis() as u64;
        let Timing { hold_ms, fade_ms } = showing.timing;
        Some((
            state.hwnd,
            ime_logic::alpha_at(elapsed, hold_ms, fade_ms, showing.alpha),
        ))
    }) {
        Some(v) => v,
        None => return,
    };

    match alpha {
        Some(a) => {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), a, LWA_ALPHA);
        }
        None => hide(),
    }
}

/// 表示を消す。
unsafe fn hide() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(state) = s.as_mut() else { return };
        if state.showing.take().is_some() {
            let _ = KillTimer(state.hwnd, TIMER_ANIM);
            let _ = ShowWindow(state.hwnd, SW_HIDE);
        }
    });
}

/// 角丸の背景とモードの文字を描く。
unsafe fn on_paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);

    STATE.with(|s| {
        let s = s.borrow();
        let Some(state) = s.as_ref() else { return };
        let Some(showing) = state.showing else { return };
        let (background, foreground) = showing.theme.colors();
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: state.size,
            bottom: state.size,
        };

        let brush = CreateSolidBrush(COLORREF(background));
        FillRect(hdc, &rect, brush);
        let _ = DeleteObject(brush);

        let old_font = state
            .font
            .map(|(font, _)| SelectObject(hdc, font))
            .unwrap_or(HGDIOBJ::default());
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(foreground));
        let mut text: Vec<u16> = showing.label.encode_utf16().collect();
        DrawTextW(
            hdc,
            &mut text,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        if !old_font.is_invalid() {
            SelectObject(hdc, old_font);
        }
    });

    let _ = EndPaint(hwnd, &ps);
}
