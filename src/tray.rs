//! タスクトレイ制御。
//!
//! タスクトレイのアイコンと、クリックしたときのメニューを受け持つ。外部のクレートは使わず、
//! `Shell_NotifyIconW` とポップアップメニューを直接使う。
//!
//! - アイコンとメニューは専用のスレッド（見えないウィンドウとメッセージループ）で動かす。
//!   メニュー操作は、チャネルでメインスレッドへ [`TrayMessage`] を送るだけで、実際の切り替えや
//!   終了処理はメインスレッドが行う（[`crate::main`]）。
//! - 有効／無効の状態は、メニュー項目のチェックマーク（`MF_CHECKED`）で表す。メニューは
//!   開くたびに今の状態から作り直す。
//! - エクスプローラーが再起動すると、トレイのアイコンは消える。Windows が知らせる
//!   `TaskbarCreated` を受けて、同じアイコン・ツールチップで登録し直す。
//! - お知らせ（[`Menu::notify`]）は、トレイの吹き出し（通知）で出す。
//! - 一時停止している間は、OFF のアイコンにし、ツールチップに「一時停止中」と出す。
//!   機能ごとの ON/OFF の項目は、選べないようにする（解除したときに元の状態へ戻すため）。

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::SystemTime;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, NOTIFY_ICON_DATA_FLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, KillTimer, LoadIconW,
    LoadImageW, SetTimer, WM_TIMER,
    PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow,
    TrackPopupMenu, TranslateMessage, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, LR_SHARED, MF_CHECKED,
    MF_DISABLED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, SM_CXSMICON, SM_CYSMICON,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE,
    WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
    WS_POPUP,
};

use crate::ui::wide;
use crate::TrayMessage;

/// ON 時のアイコン（app.rc で埋め込んだリソース名）。
const ICON_ON: PCWSTR = w!("icon_on");
/// OFF 時のアイコン（app.rc で埋め込んだリソース名）。
const ICON_OFF: PCWSTR = w!("icon_off");

/// アプリ名（ツールチップ・メニューの見出し・お知らせの題名に使う）。
const APP_NAME: &str = "アタイの貼り付け";

/// トレイのアイコンからの知らせに使うメッセージ。
const WM_APP_TRAY: u32 = WM_APP + 1;
/// アイコンの登録に失敗したとき、もう一度試すためのタイマーと、その間隔（ミリ秒）。
const TIMER_RETRY_ADD: usize = 1;
const RETRY_ADD_MS: u32 = 3000;
/// トレイのアイコンの番号（このアプリのアイコンは 1 つだけ）。
const ICON_ID: u32 = 1;

// メニュー項目の番号。
const CMD_REMAP: usize = 1;
const CMD_HOTKEYS: usize = 2;
const CMD_HISTORY: usize = 3;
const CMD_IME: usize = 4;
const CMD_PAUSE: usize = 5;
const CMD_SETTINGS: usize = 10;
const CMD_HOTKEY_WINDOW: usize = 11;
const CMD_SNIPPETS: usize = 12;
const CMD_LIST: usize = 13;
const CMD_STARTUP: usize = 20;
const CMD_UPDATE: usize = 21;
const CMD_HELP: usize = 22;
const CMD_QUIT: usize = 30;

/// アプリ名とバージョンの表示（例: `アタイの貼り付け v1.2.2`）。
///
/// ツールチップとメニュー先頭の見出しの両方に使う。Windows 11 ではトレイのアイコンが
/// 「^」の中に隠れてツールチップを見にくいので、メニューを開けば必ず目に入るようにしている。
fn app_title() -> String {
    format!("{APP_NAME} v{}", env!("CARGO_PKG_VERSION"))
}

/// メニューとアイコンに出す、今の状態。
#[derive(Clone)]
pub struct State {
    /// 値貼り付けのキーの組み合わせ（メニューの文言に使う）。
    pub remap_hotkey: String,
    /// 値貼り付け（キーリマップ）が有効か。アイコンの絵柄もこれで決まる。
    pub remap: bool,
    /// キー割り当てが有効か。
    pub hotkeys: bool,
    /// クリップボードの履歴を記録しているか。
    pub history: bool,
    /// 入力モードの表示が有効か。
    pub ime_indicator: bool,
    /// Windows の起動時（サインイン時）に自動で起動するか。
    pub startup: bool,
    /// 一時停止しているときは、解除する日時。機能ごとの ON/OFF（上の項目）は利用者が選んだ
    /// ままで、一時停止の間だけ止める。
    pub paused_until: Option<SystemTime>,
}

impl State {
    /// アイコンを ON の絵柄にするか（値貼り付けが有効で、一時停止していないとき）。
    fn icon_on(&self) -> bool {
        self.remap && self.paused_until.is_none()
    }
}

/// 一時停止を解除するまでの残り（分。端数は切り上げる。過ぎていれば 0）。
pub fn minutes_left(until: SystemTime, now: SystemTime) -> u64 {
    until
        .duration_since(now)
        .map(|d| d.as_secs().div_ceil(60))
        .unwrap_or(0)
}

/// トレイのスレッドと共有するもの。
struct Shared {
    state: State,
    tooltip: String,
}

thread_local! {
    /// トレイのスレッドが持つもの（ウィンドウプロシージャから使う）。
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

struct Context {
    tx: Sender<TrayMessage>,
    shared: Arc<Mutex<Shared>>,
    /// エクスプローラーが再起動したことを知らせるメッセージ（`TaskbarCreated`）。
    taskbar_created: u32,
}

/// トレイのアイコン。生きている間だけトレイに表示されるため、呼び出し側で保持し続けること。
pub struct Menu {
    window: AtomicIsize,
    shared: Arc<Mutex<Shared>>,
    thread: Option<JoinHandle<()>>,
}

impl Menu {
    fn hwnd(&self) -> HWND {
        HWND(self.window.load(Ordering::SeqCst) as *mut core::ffi::c_void)
    }

    /// 状態を書き換える（次にメニューを開いたときから表示に出る）。値貼り付けの ON/OFF や
    /// 一時停止が変わったら、アイコンの絵柄とツールチップも替える。
    pub fn update(&self, change: impl FnOnce(&mut State)) {
        let (before, after) = {
            let mut shared = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            let before = (shared.state.icon_on(), shared.state.paused_until.is_some());
            change(&mut shared.state);
            (before, (shared.state.icon_on(), shared.state.paused_until.is_some()))
        };
        if before != after {
            unsafe { notify_icon(self.hwnd(), NIM_MODIFY, NIF_ICON | NIF_TIP, &self.shared, None) };
        }
    }

    /// ダウンロードの進捗をツールチップに表示する。
    pub fn set_progress(&self, percent: u64) {
        self.set_tooltip(format!("{} - 更新を取得中 {percent}%", app_title()));
    }

    /// ツールチップを通常の表示に戻す。
    pub fn clear_progress(&self) {
        self.set_tooltip(app_title());
    }

    fn set_tooltip(&self, tooltip: String) {
        self.shared.lock().unwrap_or_else(|p| p.into_inner()).tooltip = tooltip;
        unsafe { notify_icon(self.hwnd(), NIM_MODIFY, NIF_TIP, &self.shared, None) };
    }

    /// トレイの吹き出し（通知）でお知らせを出す。作業を止めないお知らせに使う。
    pub fn notify(&self, text: &str) {
        unsafe { notify_icon(self.hwnd(), NIM_MODIFY, NIF_INFO, &self.shared, Some(text)) };
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        let hwnd = self.hwnd();
        if !hwnd.is_invalid() {
            unsafe {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// タスクトレイのアイコンを作り、メニューを使えるようにする。
pub fn build(tx: Sender<TrayMessage>, state: State) -> Result<Menu, String> {
    let shared = Arc::new(Mutex::new(Shared {
        state,
        tooltip: app_title(),
    }));
    let (ready_tx, ready_rx) = mpsc::channel::<Result<isize, String>>();
    let thread_shared = Arc::clone(&shared);
    let thread = std::thread::spawn(move || unsafe { thread_main(tx, thread_shared, ready_tx) });
    let window = ready_rx
        .recv()
        .map_err(|_| "トレイのスレッドが始まりませんでした".to_string())??;
    Ok(Menu {
        window: AtomicIsize::new(window),
        shared,
        thread: Some(thread),
    })
}

unsafe fn thread_main(
    tx: Sender<TrayMessage>,
    shared: Arc<Mutex<Shared>>,
    ready: Sender<Result<isize, String>>,
) {
    let instance = match GetModuleHandleW(None) {
        Ok(instance) => instance,
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
    };
    let class_name = w!("AtaiPasteTray");
    let class = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance.into(),
        lpszClassName: class_name,
        ..Default::default()
    };
    RegisterClassW(&class);
    // メッセージ専用ウィンドウには TaskbarCreated が届かないので、見えない普通のウィンドウにする。
    let hwnd = match CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class_name,
        w!("アタイの貼り付け"),
        WS_POPUP,
        0,
        0,
        0,
        0,
        None,
        None,
        instance,
        None,
    ) {
        Ok(hwnd) => hwnd,
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
    };
    let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
    CONTEXT.with(|c| {
        *c.borrow_mut() = Some(Context {
            tx,
            shared: Arc::clone(&shared),
            taskbar_created,
        })
    });
    if !notify_icon(hwnd, NIM_ADD, NIF_ICON | NIF_TIP | NIF_MESSAGE, &shared, None) {
        // サインイン直後など、タスクバーがまだ無いことがある。TaskbarCreated が届いたら登録する。
        log::warn!("トレイのアイコンを登録できませんでした（少し待って、もう一度試します）");
        SetTimer(hwnd, TIMER_RETRY_ADD, RETRY_ADD_MS, None);
    }
    let _ = ready.send(Ok(hwnd.0 as isize));

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    CONTEXT.with(|c| c.borrow_mut().take());
}

/// トレイのアイコンを登録・書き換え・削除する。`info` があれば吹き出しを出す。
unsafe fn notify_icon(
    hwnd: HWND,
    message: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE,
    flags: NOTIFY_ICON_DATA_FLAGS,
    shared: &Arc<Mutex<Shared>>,
    info: Option<&str>,
) -> bool {
    if hwnd.is_invalid() {
        return false;
    }
    let (on, tooltip) = {
        let shared = shared.lock().unwrap_or_else(|p| p.into_inner());
        let tooltip = if shared.state.paused_until.is_some() {
            format!("{}（一時停止中）", shared.tooltip)
        } else {
            shared.tooltip.clone()
        };
        (shared.state.icon_on(), tooltip)
    };
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: flags,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: load_icon(on),
        ..Default::default()
    };
    copy_text(&mut data.szTip, &tooltip);
    if let Some(info) = info {
        copy_text(&mut data.szInfo, info);
        copy_text(&mut data.szInfoTitle, APP_NAME);
        data.dwInfoFlags = NIIF_INFO;
    }
    Shell_NotifyIconW(message, &data).as_bool()
}

/// 文字を、終わりの印つきで固定長の欄に写す（入りきらない分は切り捨てる）。
fn copy_text(target: &mut [u16], text: &str) {
    let Some(room) = target.len().checked_sub(1) else {
        return;
    };
    let mut written = 0;
    for unit in text.encode_utf16().take(room) {
        target[written] = unit;
        written += 1;
    }
    // サロゲートペアの前半だけで終わらないようにする。
    if written > 0 && (0xD800..0xDC00).contains(&target[written - 1]) {
        written -= 1;
    }
    target[written] = 0;
}

/// トレイ用のアイコンを、今の画面の拡大率に合った大きさで読む。
unsafe fn load_icon(on: bool) -> HICON {
    let name = if on { ICON_ON } else { ICON_OFF };
    let Ok(instance) = GetModuleHandleW(None) else {
        return HICON::default();
    };
    let handle = LoadImageW(
        instance,
        name,
        IMAGE_ICON,
        GetSystemMetrics(SM_CXSMICON),
        GetSystemMetrics(SM_CYSMICON),
        LR_DEFAULTCOLOR | LR_SHARED,
    );
    match handle {
        Ok(handle) => HICON(handle.0),
        Err(_) => LoadIconW(instance, name).unwrap_or_default(),
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_TRAY => {
            // 左クリックでも右クリックでもメニューを出す（キーボードで選んだときも）。
            let event = (lparam.0 as u32) & 0xFFFF;
            if matches!(event, WM_LBUTTONUP | WM_RBUTTONUP | WM_CONTEXTMENU) {
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_RETRY_ADD => {
            // サインイン直後は、エクスプローラーが忙しくて登録に失敗することがある。
            let shared = CONTEXT.with(|c| c.borrow().as_ref().map(|ctx| Arc::clone(&ctx.shared)));
            if let Some(shared) = shared {
                if notify_icon(hwnd, NIM_ADD, NIF_ICON | NIF_TIP | NIF_MESSAGE, &shared, None) {
                    let _ = KillTimer(hwnd, TIMER_RETRY_ADD);
                    log::info!("トレイのアイコンを登録しました");
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let shared = CONTEXT.with(|c| c.borrow().as_ref().map(|ctx| Arc::clone(&ctx.shared)));
            if let Some(shared) = shared {
                notify_icon(hwnd, NIM_DELETE, NOTIFY_ICON_DATA_FLAGS(0), &shared, None);
            }
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => {
            let restarted = CONTEXT.with(|c| {
                c.borrow()
                    .as_ref()
                    .filter(|ctx| ctx.taskbar_created != 0 && ctx.taskbar_created == msg)
                    .map(|ctx| Arc::clone(&ctx.shared))
            });
            if let Some(shared) = restarted {
                // エクスプローラーが再起動した。アイコンを登録し直す。
                log::info!("タスクバーが作り直されたため、トレイのアイコンを登録し直します");
                if notify_icon(hwnd, NIM_ADD, NIF_ICON | NIF_TIP | NIF_MESSAGE, &shared, None) {
                    let _ = KillTimer(hwnd, TIMER_RETRY_ADD);
                }
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

/// メニューを今の状態から作って出し、選ばれた項目をメインスレッドへ知らせる。
unsafe fn show_menu(hwnd: HWND) {
    let Some((tx, state)) = CONTEXT.with(|c| {
        c.borrow().as_ref().map(|ctx| {
            let shared = ctx.shared.lock().unwrap_or_else(|p| p.into_inner());
            (ctx.tx.clone(), shared.state.clone())
        })
    }) else {
        return;
    };
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    let add = |id: usize, text: &str, checked: bool, enabled: bool| {
        let text = wide(text);
        let mut flags = if checked { MF_STRING | MF_CHECKED } else { MF_STRING };
        if !enabled {
            flags |= MF_GRAYED;
        }
        let _ = AppendMenuW(menu, flags, id, PCWSTR(text.as_ptr()));
    };
    let item = |id: usize, text: &str, checked: bool| add(id, text, checked, true);
    let separator = || {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    };
    // 先頭にアプリ名とバージョンを見出しとして置く（選べない項目）。
    let title = wide(&app_title());
    let _ = AppendMenuW(menu, MF_STRING | MF_DISABLED | MF_GRAYED, 0, PCWSTR(title.as_ptr()));
    separator();
    // 機能の ON/OFF。一時停止している間は選べない（チェックは利用者が選んだ状態のまま出し、
    // 解除するとその状態に戻る）。
    let switchable = state.paused_until.is_none();
    add(CMD_REMAP, &format!("値貼り付け（{}）(&V)", state.remap_hotkey), state.remap, switchable);
    add(CMD_HOTKEYS, "キー割り当て(&K)", state.hotkeys, switchable);
    add(CMD_HISTORY, "クリップボードの履歴(&H)", state.history, switchable);
    add(CMD_IME, "入力モードの表示(&I)", state.ime_indicator, switchable);
    match state.paused_until {
        Some(until) => {
            // 解除の時刻を過ぎていても、メインスレッドが解除するまでは 1 分と出す。
            let left = minutes_left(until, SystemTime::now()).max(1);
            item(CMD_PAUSE, &format!("一時停止を解除（あと {left} 分）(&Z)"), true);
        }
        None => item(CMD_PAUSE, "30 分間 一時停止(&Z)", false),
    }
    separator();
    // 画面を開く。
    item(CMD_SETTINGS, "設定(&S)...", false);
    item(CMD_HOTKEY_WINDOW, "キー割り当ての編集(&E)...", false);
    item(CMD_SNIPPETS, "定型文の編集(&T)...", false);
    item(CMD_LIST, "キー割り当ての一覧(&L)", false);
    separator();
    item(CMD_STARTUP, "Windows の起動時に実行(&R)", state.startup);
    item(CMD_UPDATE, "更新を確認(&U)", false);
    item(CMD_HELP, "使い方を開く(&P)", false);
    separator();
    item(CMD_QUIT, "終了(&X)", false);

    let mut point = POINT::default();
    let _ = GetCursorPos(&mut point);
    // メニューの外を押したときに閉じるよう、出す前に前面のウィンドウにしておく。
    let _ = SetForegroundWindow(hwnd);
    let chosen = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN,
        point.x,
        point.y,
        0,
        hwnd,
        None,
    );
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);

    let message = match chosen.0 as usize {
        CMD_REMAP => TrayMessage::Toggle,
        CMD_HOTKEYS => TrayMessage::ToggleHotkeys,
        CMD_HISTORY => TrayMessage::ToggleHistory,
        CMD_IME => TrayMessage::ToggleImeIndicator,
        CMD_PAUSE => TrayMessage::TogglePause,
        CMD_SETTINGS => TrayMessage::OpenSettings,
        CMD_HOTKEY_WINDOW => TrayMessage::OpenHotkeys,
        CMD_SNIPPETS => TrayMessage::OpenSnippets,
        CMD_LIST => TrayMessage::ShowList,
        CMD_STARTUP => TrayMessage::ToggleStartup,
        CMD_UPDATE => TrayMessage::CheckUpdate,
        CMD_HELP => TrayMessage::OpenHelp,
        CMD_QUIT => TrayMessage::Quit,
        _ => return,
    };
    let _ = tx.send(message);
}
