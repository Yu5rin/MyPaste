//! IME 入力モードの画面中央表示。
//!
//! Windows 10 1703 以降の標準機能と同じく、IME の入力モード（あ / A / カ / ｶ / Ａ）を
//! 切り替えた瞬間に、画面中央へ大きくモードを表示してすぐフェードアウトさせる。
//!
//! ## 仕組み
//! - 専用スレッドで 100ms ごとに、前面ウィンドウのフォーカス先の IME 状態を調べる。
//!   `ImmGetDefaultIMEWnd` で得た IME ウィンドウへ `WM_IME_CONTROL` を
//!   `SendMessageTimeoutW`（`SMTO_ABORTIFHUNG`）で送るので、相手が固まっていても
//!   こちらが巻き込まれない。管理者権限のウィンドウなど、取れない場合は何もしない。
//! - 表示するかどうかの判断（同じウィンドウ内でモードが変わったときだけ）や
//!   フェードの計算は [`crate::ime_logic`] にあり、Linux でもテストできる。
//! - 表示ウィンドウはクリック透過（`WS_EX_TRANSPARENT`）・非アクティブ
//!   （`WS_EX_NOACTIVATE` と `WM_MOUSEACTIVATE` で `MA_NOACTIVATE`）・タスクバー非表示
//!   （`WS_EX_TOOLWINDOW`）・最前面（`WS_EX_TOPMOST`）。表示も `SWP_NOACTIVATE` で
//!   行うため、利用者の入力先を奪わない。
//! - 不透明度はレイヤードウィンドウの `SetLayeredWindowAttributes` で変え、角丸は
//!   `SetWindowRgn` で切り抜く。
//!
//! キーボードフック（[`crate::keyboard`]）とは別スレッドで動くため、Ctrl+B の
//! リマップには影響しない。
//!
//! 表示時間・フェードアウトの時間・大きさは [`set_params`] で起動中にも変えられ、設定画面からは
//! [`preview`] で試しに表示できる。

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, DrawTextW,
    EndPaint, FillRect, GetMonitorInfoW, InvalidateRect, MonitorFromWindow, SelectObject,
    SetBkMode, SetTextColor, SetWindowRgn, ANTIALIASED_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
    FF_DONTCARE, FW_BOLD, HFONT, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::Ime::ImmGetDefaultIMEWnd;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
    GetGUIThreadInfo, GetMessageW, GetWindowThreadProcessId, KillTimer, PostMessageW,
    PostThreadMessageW,
    RegisterClassW, SendMessageTimeoutW, SetLayeredWindowAttributes, SetTimer, SetWindowPos,
    ShowWindow, TranslateMessage, GUITHREADINFO, HTTRANSPARENT, HWND_TOPMOST, LWA_ALPHA,
    MA_NOACTIVATE, MSG, SMTO_ABORTIFHUNG, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE,
    WM_APP, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT, WM_QUIT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::config::ImeIndicatorSettings;
use crate::ime_logic::{self, ImeMode, ModeTracker};

/// IME の状態を調べる間隔（ミリ秒）。
const POLL_INTERVAL_MS: u32 = 100;
/// フェード中の再描画の間隔（ミリ秒）。
const ANIM_INTERVAL_MS: u32 = 16;
/// 表示中の最大の不透明度（0〜255）。少しだけ透かして背後を感じさせる。
const MAX_ALPHA: u8 = 230;
/// IME への問い合わせのタイムアウト（ミリ秒）。相手が固まっていても待たない。
const QUERY_TIMEOUT_MS: u32 = 50;

/// 背景色（濃いグレー）。
const BACKGROUND: COLORREF = COLORREF(0x0020_2020);
/// 文字色（白）。
const FOREGROUND: COLORREF = COLORREF(0x00FF_FFFF);

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

/// 試しに表示する（設定画面のプレビュー）。WPARAM = 表示時間とフェードアウトの時間
/// （[`pack_timing`]）、LPARAM = 大きさ。
const WM_APP_PREVIEW: u32 = WM_APP + 1;

/// 機能の ON/OFF。トレイメニューから切り替えられ、監視スレッドが毎回見る。
static ENABLED: AtomicBool = AtomicBool::new(true);
/// 表示を保持する時間（ミリ秒）。
static HOLD_MS: AtomicU64 = AtomicU64::new(400);
/// フェードアウトにかける時間（ミリ秒）。
static FADE_MS: AtomicU64 = AtomicU64::new(ime_logic::DEFAULT_FADE_MS);
/// 表示の一辺（96 DPI 基準のピクセル）。
static BASE_SIZE: AtomicU32 = AtomicU32::new(120);
/// 表示ウィンドウ（プレビューの依頼先）。0 は未作成。
static WINDOW: AtomicIsize = AtomicIsize::new(0);

/// 表示の時間（ミリ秒）。
#[derive(Clone, Copy)]
pub struct Timing {
    /// 消え始めるまでの表示時間。
    pub hold_ms: u64,
    /// フェードアウトにかける時間。
    pub fade_ms: u64,
}

impl Timing {
    /// 設定できる範囲に丸める。
    fn clamped(self) -> Timing {
        Timing {
            hold_ms: self.hold_ms.clamp(ime_logic::HOLD_MS_MIN, ime_logic::HOLD_MS_MAX),
            fade_ms: self.fade_ms.clamp(ime_logic::FADE_MS_MIN, ime_logic::FADE_MS_MAX),
        }
    }
}

/// 範囲に丸めた [`Timing`] を 1 つの WPARAM に詰める（どちらも 16 ビットに収まる）。
fn pack_timing(timing: Timing) -> usize {
    let t = timing.clamped();
    (t.hold_ms as usize) | ((t.fade_ms as usize) << 16)
}

fn unpack_timing(value: usize) -> Timing {
    Timing {
        hold_ms: (value & 0xFFFF) as u64,
        fade_ms: ((value >> 16) & 0xFFFF) as u64,
    }
}

/// 表示時間・フェードアウトの時間・大きさを設定する（起動時と、設定画面で保存したとき）。
/// 範囲外は丸める。
pub fn set_params(timing: Timing, size: u32) {
    let timing = timing.clamped();
    HOLD_MS.store(timing.hold_ms, Ordering::SeqCst);
    FADE_MS.store(timing.fade_ms, Ordering::SeqCst);
    BASE_SIZE.store(clamp_size(size), Ordering::SeqCst);
}

fn clamp_size(size: u32) -> u32 {
    size.clamp(ime_logic::SIZE_MIN, ime_logic::SIZE_MAX)
}

/// 指定した時間と大きさで、試しに「あ」を表示する（機能が OFF でも表示する）。
/// 監視スレッドが動いていなければ何もせず `false` を返す。
pub fn preview(timing: Timing, size: u32) -> bool {
    let window = WINDOW.load(Ordering::SeqCst);
    if window == 0 {
        return false;
    }
    unsafe {
        PostMessageW(
            HWND(window as *mut core::ffi::c_void),
            WM_APP_PREVIEW,
            WPARAM(pack_timing(timing)),
            LPARAM(clamp_size(size) as isize),
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
    // 極端な値で画面を覆ったり、見えなくなったりしないよう幅を持たせて制限する。
    set_params(
        Timing {
            hold_ms: settings.hold_ms,
            fade_ms: settings.fade_ms,
        },
        settings.size,
    );

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
}

/// 表示中の内容。
#[derive(Clone, Copy)]
struct Showing {
    mode: ImeMode,
    started: Instant,
    timing: Timing,
    /// 設定画面のプレビューか（機能が OFF でも消さない）。
    preview: bool,
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
            let size = i32::try_from(lparam.0).unwrap_or(0);
            show(ImeMode::Hiragana, GetForegroundWindow(), unpack_timing(wparam.0), size, true);
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
            return None;
        }
        state.tracker.observe(observed.map(|(w, m)| (w.0 as isize, m)))
    });

    if !enabled {
        // OFF にしたら表示中のものも消す。ただし設定画面のプレビューは残す。
        let previewing = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .and_then(|state| state.showing)
                .is_some_and(|showing| showing.preview)
        });
        if !previewing {
            hide();
        }
        return;
    }
    if let (Some(mode), Some((fg, _))) = (to_show, observed) {
        show(
            mode,
            fg,
            Timing {
                hold_ms: HOLD_MS.load(Ordering::SeqCst),
                fade_ms: FADE_MS.load(Ordering::SeqCst),
            },
            BASE_SIZE.load(Ordering::SeqCst) as i32,
            false,
        );
    }
}

/// 前面ウィンドウのフォーカス先の IME 状態を調べる。取れなければ `None`。
///
/// 戻り値の HWND はフォーカス先のウィンドウ（「同じウィンドウ内での変化か」の判定に使う）。
/// 表示位置の基準には前面ウィンドウを使う（呼び出し側で改めて取る）。
unsafe fn query_foreground_ime() -> Option<(HWND, ImeMode)> {
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

    // フォーカス先（入力欄）を取る。取れなければ前面ウィンドウそのもの。
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let target = if GetGUIThreadInfo(thread_id, &mut info).is_ok() && !info.hwndFocus.is_invalid() {
        info.hwndFocus
    } else {
        foreground
    };

    let ime_window = ImmGetDefaultIMEWnd(target);
    if ime_window.is_invalid() {
        return None;
    }

    let open = send_ime_control(ime_window, IMC_GETOPENSTATUS)?;
    let conversion = send_ime_control(ime_window, IMC_GETCONVERSIONMODE)?;
    Some((target, ime_logic::mode_from_status(open != 0, conversion as u32)))
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

/// 前面ウィンドウのあるモニターの中央にモードを表示する。
///
/// `base_size` は 96 DPI 基準の一辺。`preview` は設定画面からの試し表示か。
unsafe fn show(mode: ImeMode, focus: HWND, timing: Timing, base_size: i32, preview: bool) {
    // 位置とサイズは前面（最上位）ウィンドウを基準にする。フォーカス先が子ウィンドウでも、
    // 前面ウィンドウと同じモニター・同じ DPI になる。
    let foreground = GetForegroundWindow();
    let basis = if foreground.is_invalid() { focus } else { foreground };

    let monitor = MonitorFromWindow(basis, MONITOR_DEFAULTTONEAREST);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !GetMonitorInfoW(monitor, &mut info).as_bool() {
        return;
    }
    let dpi = GetDpiForWindow(basis);

    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(state) = s.as_mut() else { return };

        let size = ime_logic::scale_for_dpi(base_size, dpi);
        let work = info.rcWork;
        let (x, y) = ime_logic::centered_origin(work.left, work.top, work.right, work.bottom, size);

        // 文字の高さは一辺の 55%。DPI が変わったときだけフォントを作り直す。
        let font_height = size * 55 / 100;
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
            mode,
            started: Instant::now(),
            timing,
            preview,
        });
        let hwnd = state.hwnd;

        let radius = size / 4;
        let region = CreateRoundRectRgn(0, 0, size + 1, size + 1, radius, radius);
        // SetWindowRgn が成功すると、リージョンの所有権はウィンドウへ移る（削除しない）。
        SetWindowRgn(hwnd, region, true);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), MAX_ALPHA, LWA_ALPHA);
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
        Some((state.hwnd, ime_logic::alpha_at(elapsed, hold_ms, fade_ms, MAX_ALPHA)))
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
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: state.size,
            bottom: state.size,
        };

        let brush = CreateSolidBrush(BACKGROUND);
        FillRect(hdc, &rect, brush);
        let _ = DeleteObject(brush);

        if let Some(Showing { mode, .. }) = state.showing {
            let old_font = state
                .font
                .map(|(font, _)| SelectObject(hdc, font))
                .unwrap_or(HGDIOBJ::default());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, FOREGROUND);
            let mut text: Vec<u16> = mode.label().encode_utf16().collect();
            DrawTextW(
                hdc,
                &mut text,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            if !old_font.is_invalid() {
                SelectObject(hdc, old_font);
            }
        }
    });

    let _ = EndPaint(hwnd, &ps);
}
