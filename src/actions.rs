//! キー割り当ての動作を実行する。
//!
//! キーボードフック（[`crate::keyboard`]）は、割り当てたキーを捕まえたら [`dispatch`] で
//! 動作をここへ渡すだけにして、すぐに戻る。フックの中で時間のかかること（プログラムを開く、
//! クリップボードを読み書きするなど）をすると、Windows がフックを外してしまうことがあるため、
//! 実行は専用のスレッドで行う。
//!
//! 動作の種類と内容の解釈は [`crate::hotkey_rules`] にある。
//!
//! 実行スレッドは、クリップボードの持ち主になるための見えないウィンドウ（メッセージ専用）を
//! 持つ。「文字をまとめて貼り付ける」では、このウィンドウで遅延レンダリング（中身は
//! 求められたときに渡す）を使い、貼り付け先が本当に読みに来たかどうかを確かめる。
//!
//! 「クリップボードの履歴から貼り付け」の割り当てがあるときは、同じウィンドウでクリップボードの
//! 変化（WM_CLIPBOARDUPDATE）を受け取り、コピーされた文字を [`crate::clip_history`] に覚える。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, EnumClipboardFormats,
    GetClipboardData, GetClipboardOwner, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, RemoveClipboardFormatListener, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_SHIFT};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetAncestor,
    GetForegroundWindow, GetMessageW, GetShellWindow, GetWindowLongPtrW, IsHungAppWindow,
    SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_NULL,
    GetWindowThreadProcessId, KillTimer, MessageBoxW, PeekMessageW, PostMessageW,
    PostQuitMessage, PostThreadMessageW, RegisterClassW, SetForegroundWindow, SetTimer,
    SetWindowPos, TranslateMessage, GA_ROOT, GWL_EXSTYLE, HWND_NOTOPMOST, HWND_TOPMOST,
    MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MSG, PM_NOREMOVE,
    PM_QS_SENDMESSAGE, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNORMAL, WM_APP,
    WM_CLIPBOARDUPDATE, WM_DESTROYCLIPBOARD, WM_ENDSESSION, WM_QUIT, WM_RENDERALLFORMATS,
    WM_RENDERFORMAT, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::clip_history::{self, History, MenuPosition};
use crate::clip_images::{self, Index, Part};
use crate::hotkey_rules::{self, Action, LocalTime};
use crate::remap_logic::Hotkey;
use crate::config::Snippet;
use crate::{clip_image_store, clip_store, config, history_window, snippet_window, ime_indicator, keyboard, ocr, remap_logic, sendinput, text_transform};

/// クリップボードの文字（`CF_UNICODETEXT`）。
const CF_UNICODETEXT: u32 = 13;
/// 画像（DIB）とファイルの一覧の形式。
const CF_DIB: u32 = 8;
const CF_HDROP: u32 = 15;
/// クリップボードがほかのアプリに使われているときに、開き直す回数と間隔。
const CLIPBOARD_RETRIES: u32 = 10;
const CLIPBOARD_RETRY_WAIT: Duration = Duration::from_millis(20);

/// 「まとめて貼り付ける」で、Ctrl+V を送ってから貼り付け先が文字を読みに来るのを待つ時間。
/// これを過ぎても読みに来なければ、貼り付けを受け付けない入力欄とみなして 1 文字ずつ入力する。
const PASTE_ACCEPT_WAIT: Duration = Duration::from_millis(500);
/// 貼り付け先が文字を読みに来てから、元のクリップボードに戻すまでの待ち時間
/// （同じ貼り付けの中で、別の形式を続けて読みに来ることがあるため）。
const PASTE_SETTLE_WAIT: Duration = Duration::from_millis(300);
/// 貼り付け先が応答するかを確かめるときに、返事を待つ時間（ミリ秒）。
const HUNG_PROBE_MS: u32 = 200;
/// 貼り付け先が固まっているとき、動き出すのを待つ上限。
const PASTE_HUNG_WAIT: Duration = Duration::from_secs(10);

thread_local! {
    /// クリップボードの持ち主になる、見えないウィンドウ（実行スレッドだけで使う）。
    static CLIP_WINDOW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    /// 遅延レンダリングで、求められたら渡す文字（UTF-16 と終端の 0 をバイト列にしたもの）。
    static PENDING_TEXT: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    /// 遅延レンダリングで、求められたら渡す画像（DIB）。
    static PENDING_IMAGE: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    /// コピーされた画像の目録（中身はファイルにある）。
    static IMAGES: RefCell<Index> = RefCell::new(Index::default());
    /// 画像の目録を読めなかった（ほかのアプリが使っていた など）。この間は画像を覚えない。
    static IMAGES_UNREADABLE: Cell<bool> = const { Cell::new(false) };
    /// 貼り付け先が文字を読みに来たか。
    static RENDERED: Cell<bool> = const { Cell::new(false) };
    /// 動作を実行している途中か（履歴の一覧を出している間も、届いた依頼を割り込ませない）。
    static BUSY: Cell<bool> = const { Cell::new(false) };
    /// コピーされた文字の履歴。
    static HISTORY: RefCell<History> = RefCell::new(History::default());
    /// 保存しておいた履歴を読み込んだか（最初に記録を始めるときに 1 回だけ読む）。
    static HISTORY_LOADED: Cell<bool> = const { Cell::new(false) };
    /// 履歴が変わって、まだファイルに保存していないか。
    static HISTORY_DIRTY: Cell<bool> = const { Cell::new(false) };
    /// 最初に履歴が変わった（まだ保存していない）時刻。続けてコピーしても、いつまでも保存が
    /// 先延ばしにならないようにするため。
    static HISTORY_DIRTY_SINCE: Cell<Option<Instant>> = const { Cell::new(None) };
    /// 前に当てた履歴の使い方（記録するか, 残すか）。保存した履歴を消すのは、利用者が
    /// 記録・残すを ON から OFF にしたときだけにする（起動直後や、設定ファイルが読めず既定値で
    /// 動いているときに消してしまわないように）。
    static HISTORY_APPLIED: Cell<Option<(bool, bool)>> = const { Cell::new(None) };
    /// 保存した履歴を読めなかった（ほかのアプリが使っていた など）。この間は上書きしない。
    static HISTORY_STORE_UNREADABLE: Cell<bool> = const { Cell::new(false) };
}

/// クリップボードの履歴の使い方（[`set_history_config`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryConfig {
    /// 記録するか（設定で ON にしたか、その動作の割り当てがあるとき）。
    pub record: bool,
    /// 覚えておく件数。
    pub max_items: usize,
    /// アプリを終了しても残すか（暗号化してファイルに保存する）。
    pub keep: bool,
    /// 一覧から貼り付けたものを、いちばん上に移すか。
    pub move_to_top: bool,
    /// 画像も記録するか。
    pub record_images: bool,
    /// 一覧の出し方（位置・幅・不透明度・1 ページの件数）。
    pub view: history_window::View,
}

static HISTORY_CONFIG: Mutex<HistoryConfig> = Mutex::new(HistoryConfig {
    record: false,
    max_items: clip_history::DEFAULT_ITEMS,
    keep: false,
    move_to_top: true,
    record_images: true,
    view: history_window::View {
        keep_open: false,
        position: MenuPosition::Caret,
        width: clip_history::DEFAULT_WIDTH,
        opacity: clip_history::DEFAULT_OPACITY,
        page_size: clip_history::DEFAULT_PAGE_SIZE,
        theme: clip_history::Theme::System,
    },
});

/// 今の一覧の不透明度（一覧の上で Shift+ホイールで変えたものを含む）。
pub fn history_opacity() -> u32 {
    history_config().view.opacity
}

/// 今の「貼り付けても一覧を出したままにする」（一覧の上で切り替えたものを含む）。
pub fn history_keep_open() -> bool {
    history_config().view.keep_open
}

fn history_config() -> HistoryConfig {
    *HISTORY_CONFIG.lock().unwrap_or_else(|p| p.into_inner())
}

/// 実行の依頼。
struct Request {
    action: Action,
    /// 押されたままの修飾キー（割り当てたキーの組み合わせ）。
    held: Hotkey,
}

/// 実行を待っている依頼。フックが積み、実行スレッドが取り出す。
static QUEUE: Mutex<VecDeque<Request>> = Mutex::new(VecDeque::new());
/// 実行スレッドの見えないウィンドウ（依頼の知らせ先）。0 は未作成。
static WORKER_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// 依頼が積まれたことを、実行スレッドの見えないウィンドウへ知らせるメッセージ。
const WM_APP_REQUEST: u32 = WM_APP + 1;
/// 履歴の使い方が変わったことを知らせるメッセージ。
const WM_APP_HISTORY: u32 = WM_APP + 2;
/// 履歴を消すよう知らせるメッセージ（設定画面の「履歴を消す」）。
const WM_APP_CLEAR_HISTORY: u32 = WM_APP + 3;

/// 履歴を保存するまでの待ち時間を計るタイマー（続けてコピーしたときに、何度も書かないように）。
const TIMER_SAVE_HISTORY: usize = 1;
const SAVE_HISTORY_DELAY_MS: u32 = 2000;
/// 保存に失敗したとき、もう一度試すまでの時間（ミリ秒）。
const SAVE_HISTORY_RETRY_MS: u32 = 30_000;
/// 続けてコピーされても、最初の変更からこの時間がたったら保存する。
const SAVE_HISTORY_MAX_DELAY: Duration = Duration::from_secs(15);

/// 履歴の一覧を閉じてから、元のウィンドウに入力が戻るのを待つ時間。
const MENU_REFOCUS_WAIT: Duration = Duration::from_millis(80);
/// 画像の文字の読み取りを待つ長さの上限。
const OCR_WAIT: Duration = Duration::from_secs(30);

/// 実行スレッドのハンドル。[`Worker::stop`] で止める。
///
/// 実行スレッドは普段から `GetMessageW` でメッセージを処理し続ける。クリップボードの持ち主に
/// なっていると、ほかのアプリがコピーしたときに Windows から知らせ（WM_DESTROYCLIPBOARD など）が
/// 送られてきて、相手はその処理が終わるのを待つため、止まって待っていてはいけない。
pub struct Worker {
    thread: Option<JoinHandle<()>>,
    thread_id: u32,
}

impl Worker {
    /// 実行スレッドを止め、終わるのを待つ（実行中の動作は最後まで行う）。
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

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 実行スレッドを始める。
pub fn start() -> Worker {
    let (tx, rx) = mpsc::channel::<u32>();
    let thread = std::thread::spawn(move || unsafe {
        // ShellExecuteW のために COM を初期化しておく（関連付けによっては COM を使う）。
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let window = create_clipboard_window();
        CLIP_WINDOW.with(|w| w.set(window));
        WORKER_WINDOW.store(window.0 as isize, Ordering::SeqCst);
        apply_history_setting(window);
        let _ = tx.send(GetCurrentThreadId());

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

        WORKER_WINDOW.store(0, Ordering::SeqCst);
        save_history_now();
        // 終了後は残さない設定なら、画像のファイルも消す（記録していて、その設定を当てたあとだけ。
        // 設定を受け取る前に終了したときに、残していたものを消さないように）。
        if HISTORY_APPLIED.with(Cell::get) == Some((true, false)) {
            clip_image_store::delete_all();
        }
        if !window.is_invalid() {
            let _ = RemoveClipboardFormatListener(window);
            let _ = DestroyWindow(window);
        }
        if com {
            CoUninitialize();
        }
    });
    let thread_id = rx.recv().unwrap_or(0);
    Worker {
        thread: Some(thread),
        thread_id,
    }
}

/// 動作の実行を依頼する（キーボードフックから呼ぶ。すぐに戻る）。
pub fn dispatch(action: Action, held: Hotkey) {
    let window = WORKER_WINDOW.load(Ordering::SeqCst);
    if window == 0 {
        return;
    }
    if action == Action::ClipboardHistory && history_window::is_open() {
        // 一覧を出しているときに、出す操作をもう一度したら閉じる（積んでおくと、閉じたあとで
        // また出てしまう）。実行スレッドを起こすだけで、依頼は積まない。
        history_window::request_close();
    } else if history_window::is_open() {
        // 一覧を出している間に押されたほかの割り当ては、捨てる（積んでおくと、一覧を閉じたあとで
        // 思いがけず実行される）。
        return;
    } else {
        QUEUE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push_back(Request { action, held });
    }
    unsafe {
        let _ = PostMessageW(
            HWND(window as *mut core::ffi::c_void),
            WM_APP_REQUEST,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// クリップボードの履歴の使い方を決める（起動時と、設定・キー割り当てを保存したとき）。
pub fn set_history_config(config: HistoryConfig) {
    *HISTORY_CONFIG.lock().unwrap_or_else(|p| p.into_inner()) = config;
    post_to_worker(WM_APP_HISTORY);
}

/// 覚えている履歴（保存したものも）を消す（設定画面の「履歴を消す」）。
pub fn clear_history() {
    post_to_worker(WM_APP_CLEAR_HISTORY);
}

fn post_to_worker(msg: u32) {
    let window = WORKER_WINDOW.load(Ordering::SeqCst);
    if window != 0 {
        unsafe {
            let _ = PostMessageW(
                HWND(window as *mut core::ffi::c_void),
                msg,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

/// 履歴の使い方に合わせて、クリップボードの変化の知らせを受け取る・やめる、保存した履歴を
/// 読む・消す。
unsafe fn apply_history_setting(window: HWND) {
    if window.is_invalid() {
        return;
    }
    let config = history_config();
    let previous = HISTORY_APPLIED.with(|a| a.replace(Some((config.record, config.keep))));
    // 二重に登録しないよう、いったん外してから必要なら登録し直す。
    let _ = RemoveClipboardFormatListener(window);
    if !config.record {
        // 記録をやめる。保存した履歴（ピン留めを含む）は消さずに残し、記録を ON に戻したときに
        // 読み直す（消したいときは、設定画面の「履歴をすべて削除」を使う）。残さない設定なら、
        // 覚えているものはここで捨てる。
        if previous.is_some_and(|(record, _)| record) {
            save_history_now();
        }
        HISTORY.with(|h| h.borrow_mut().clear());
        IMAGES.with(|i| *i.borrow_mut() = Index::default());
        if !config.keep && previous.is_some() {
            clip_image_store::delete_all();
        }
        HISTORY_DIRTY.with(|d| d.set(false));
        HISTORY_LOADED.with(|l| l.set(false));
        return;
    }
    if !HISTORY_LOADED.with(Cell::get) {
        HISTORY_LOADED.with(|l| l.set(true));
        if config.keep {
            match clip_store::load() {
                Ok(saved) => {
                    log::debug!(
                        "保存していたクリップボードの履歴を読みました（{} 件、ピン留め {} 件）",
                        saved.items.len(),
                        saved.pinned.len()
                    );
                    HISTORY.with(|h| *h.borrow_mut() = History::from_saved(saved, config.max_items));
                    HISTORY_STORE_UNREADABLE.with(|u| u.set(false));
                }
                Err(e) => {
                    // 読めないまま保存すると、残していた履歴を空で上書きしてしまう。
                    log::warn!("保存していたクリップボードの履歴を読めないため、この間は保存しません: {e}");
                    HISTORY_STORE_UNREADABLE.with(|u| u.set(true));
                }
            }
            load_image_index();
        } else {
            // 終了後は残さない設定。前の回のものが残っていれば消す。
            clip_image_store::delete_all();
            IMAGES_UNREADABLE.with(|u| u.set(false));
        }
    }
    let trimmed = IMAGES.with(|i| i.borrow_mut().trim(config.max_items));
    if !trimmed.is_empty() {
        clip_image_store::delete(&trimmed);
        save_image_index();
    }
    let before = HISTORY.with(|h| h.borrow().len());
    HISTORY.with(|h| h.borrow_mut().set_max(config.max_items));
    if config.keep {
        // 件数を減らしたときや、残す設定にしたときは、今の履歴を保存し直す。
        if before != HISTORY.with(|h| h.borrow().len()) || !history_file_exists() {
            mark_history_changed();
        }
    } else {
        HISTORY_DIRTY.with(|d| d.set(false));
        clip_store::delete();
    }
    if let Err(e) = AddClipboardFormatListener(window) {
        log::warn!("クリップボードの履歴を記録できません: {e}");
    } else {
        log::debug!(
            "クリップボードの履歴を記録します（{} 件まで、終了後も残す: {}）",
            config.max_items,
            if config.keep { "はい" } else { "いいえ" }
        );
    }
}

fn history_file_exists() -> bool {
    config::clip_history_file().is_some_and(|p| p.exists())
}

/// 履歴が変わった。残す設定なら、少し待ってから保存する。
unsafe fn mark_history_changed() {
    if !history_config().keep {
        return;
    }
    HISTORY_DIRTY.with(|d| d.set(true));
    let since = HISTORY_DIRTY_SINCE.with(|d| *d.get().get_or_insert_with(Instant::now));
    HISTORY_DIRTY_SINCE.with(|d| d.set(Some(since)));
    // 続けてコピーされても、最初の変更から一定の時間がたったら待たずに保存する。
    if since.elapsed() >= SAVE_HISTORY_MAX_DELAY {
        save_history_now();
        return;
    }
    let window = clip_window();
    if !window.is_invalid() {
        SetTimer(window, TIMER_SAVE_HISTORY, SAVE_HISTORY_DELAY_MS, None);
    }
}

/// まだ保存していない履歴があれば、今すぐ保存する。
fn save_history_now() {
    HISTORY_DIRTY_SINCE.with(|d| d.set(None));
    if !HISTORY_DIRTY.with(|d| d.replace(false)) || !history_config().keep {
        return;
    }
    if HISTORY_STORE_UNREADABLE.with(Cell::get) && !reload_history_store() {
        retry_history_save();
        return;
    }
    let saved = HISTORY.with(|h| h.borrow().to_saved());
    if let Err(e) = clip_store::save(&saved) {
        log::warn!("クリップボードの履歴を保存できませんでした（あとでもう一度試します）: {e}");
        retry_history_save();
    }
}

/// 保存できなかった履歴を、あとでもう一度保存する。
fn retry_history_save() {
    HISTORY_DIRTY.with(|d| d.set(true));
    let window = clip_window();
    if !window.is_invalid() {
        unsafe {
            SetTimer(window, TIMER_SAVE_HISTORY, SAVE_HISTORY_RETRY_MS, None);
        }
    }
}

/// 起動時に読めなかった保存済みの履歴を読み直し、その後に覚えたものを重ねる。読めたら `true`。
fn reload_history_store() -> bool {
    let saved = match clip_store::load() {
        Ok(saved) => saved,
        Err(e) => {
            log::warn!("保存していたクリップボードの履歴を、まだ読めません: {e}");
            return false;
        }
    };
    let max = history_config().max_items;
    HISTORY.with(|h| {
        let mut h = h.borrow_mut();
        let current = h.entries();
        let mut merged = History::from_saved(saved, max);
        for (text, pinned) in current.into_iter().rev() {
            merged.push(&text);
            if pinned {
                merged.pin(&text);
            }
        }
        *h = merged;
    });
    HISTORY_STORE_UNREADABLE.with(|u| u.set(false));
    log::info!("保存していたクリップボードの履歴を読み直しました");
    true
}

/// 覚えている履歴と、保存した履歴を消す（ピン留めしたものは残す）。
unsafe fn clear_all_history() {
    let remaining = HISTORY.with(|h| {
        let mut h = h.borrow_mut();
        h.clear_unpinned();
        h.len()
    });
    if remaining == 0 {
        HISTORY_DIRTY.with(|d| d.set(false));
        clip_store::delete();
    } else {
        mark_history_changed();
        save_history_now();
    }
    let removed = IMAGES.with(|i| i.borrow_mut().clear_unpinned());
    if !removed.is_empty() {
        clip_image_store::delete(&removed);
        save_image_index();
    }
    log::debug!("クリップボードの履歴を消しました（ピン留め {remaining} 件は残します）");
}

/// 記録しないアプリ（大文字のプロセス名）。
static EXCLUDE_APPS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// 記録しないアプリを決める（起動時と、設定を保存したとき）。
pub fn set_history_exclude_apps(apps: Vec<String>) {
    *EXCLUDE_APPS.lock().unwrap_or_else(|p| p.into_inner()) = apps;
}

/// 記録しないアプリでコピーされたものか。コピーしたアプリは、クリップボードの持ち主の
/// ウィンドウから調べる（持ち主がいないときは、前面のアプリとみなす）。
unsafe fn copied_in_excluded_app() -> bool {
    let apps = EXCLUDE_APPS.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if apps.is_empty() {
        return false;
    }
    let owner = GetClipboardOwner().ok().filter(|w| !w.is_invalid());
    let process = match owner {
        Some(window) => crate::excel_check::process_name_of_window(window),
        None => crate::excel_check::foreground_process_name(),
    };
    process.is_some_and(|name| remap_logic::is_target_app(&name, &apps))
}

/// 一時停止中か（トレイの「一時停止」）。記録をやめる設定と違い、覚えている履歴はそのまま残す。
static HISTORY_PAUSED: AtomicBool = AtomicBool::new(false);

/// クリップボードの履歴の記録を一時停止する・再開する（覚えている履歴は消さない）。
pub fn set_history_paused(paused: bool) {
    HISTORY_PAUSED.store(paused, Ordering::SeqCst);
}

/// クリップボードが変わった。記録してよい文字なら履歴に加える。
unsafe fn record_clipboard() {
    // 記録をやめた直後に、すでに届いていた知らせは記録しない。
    if !history_config().record || HISTORY_PAUSED.load(Ordering::SeqCst) {
        return;
    }
    // 貼り付けのために自分で一時的に置いたもの・元に戻したものは記録しない。
    let owner = GetClipboardOwner().ok();
    if owner.is_some() && owner == Some(clip_window()) {
        return;
    }
    let has_text = IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok();
    // 文字があれば文字として覚える（Excel のセルのように、文字と画像の両方を置くアプリがある）。
    // ファイルのコピー（エクスプローラー）は、画像としては覚えない。
    let has_image = !has_text
        && history_config().record_images
        && IsClipboardFormatAvailable(CF_DIB).is_ok()
        && IsClipboardFormatAvailable(CF_HDROP).is_err();
    if (!has_text && !has_image) || excluded_from_history() || copied_in_excluded_app() {
        return;
    }
    if has_image {
        record_image();
    } else if let Some(text) = read_clipboard_text() {
        let stamp = next_stamp();
        if HISTORY.with(|h| h.borrow_mut().push_at(&text, stamp)) {
            mark_history_changed();
        }
    }
}

/// 次に履歴に入るものに付ける通し番号（文字と画像で通しの番号。大きいほど新しい）。
fn next_stamp() -> u64 {
    let text = HISTORY.with(|h| h.borrow().max_stamp());
    let image = IMAGES.with(|i| i.borrow().max_stamp());
    text.max(image) + 1
}

/// 一覧に出す履歴（文字と画像を 1 つにまとめたもの）。
fn history_items() -> Vec<clip_images::Item> {
    let (pinned, texts) = HISTORY.with(|h| {
        let h = h.borrow();
        (h.pinned(), h.unpinned())
    });
    clip_images::merge(pinned, texts, IMAGES.with(|i| i.borrow().entries()))
}

/// 保存していた画像の目録を読む。読めたら `true`。読めない間は画像を覚えず、目録も保存しない
/// （空の目録で上書きして、残していた画像を失わないように）。
fn load_image_index() -> bool {
    match clip_image_store::load_index() {
        Ok((index, set_aside)) => {
            // 目録に無いファイル（途中で終了して残ったもの）を消す。目録を読めずに脇へ置いた
            // ときは消さない（目録が空なので、ピン留めしたものも含めてすべて消えてしまう）。
            if !set_aside {
                clip_image_store::sweep(&index.ids());
            }
            log::debug!("保存していた画像の履歴を読みました（{} 枚）", index.len());
            IMAGES.with(|i| *i.borrow_mut() = index);
            IMAGES_UNREADABLE.with(|u| u.set(false));
            true
        }
        Err(e) => {
            log::warn!("保存していた画像の履歴を読めないため、この間は画像を覚えません: {e}");
            IMAGES_UNREADABLE.with(|u| u.set(true));
            false
        }
    }
}

/// クリップボードの画像を履歴に加える。同じ画像がすでにあれば、いちばん新しい位置へ移す。
unsafe fn record_image() {
    if IMAGES_UNREADABLE.with(Cell::get) && !load_image_index() {
        return;
    }
    let Some(dib) = read_clipboard_bytes(CF_DIB, clip_images::MAX_IMAGE_BYTES) else {
        return;
    };
    let Some(info) = clip_images::dib_info(&dib) else {
        return;
    };
    let hash = clip_images::hash(&dib);
    if let Some(id) = IMAGES.with(|i| i.borrow().find(hash, info.width, info.height)) {
        let stamp = next_stamp();
        if IMAGES.with(|i| i.borrow_mut().touch(id, stamp)) {
            save_image_index();
        }
        return;
    }
    let id = IMAGES.with(|i| i.borrow_mut().take_id());
    let stored = match clip_image_store::save_image(id, &dib) {
        Ok(stored) => stored,
        Err(e) => {
            log::warn!("画像の履歴: 画像を保存できませんでした: {e}");
            return;
        }
    };
    if let Some(thumb) = clip_image_store::make_thumb(&dib, info) {
        clip_image_store::save_thumb(id, &thumb);
    }
    let now = local_time();
    let entry = clip_images::Entry {
        id,
        width: info.width,
        height: info.height,
        stored,
        hash,
        time: format!("{}/{} {:02}:{:02}", now.month, now.day, now.hour, now.minute),
        pinned: false,
        stamp: next_stamp(),
    };
    let removed = IMAGES.with(|i| i.borrow_mut().push(entry, history_config().max_items));
    clip_image_store::delete(&removed);
    save_image_index();
    log::debug!(
        "画像の履歴: {}×{} を覚えました（{} KB、古い {} 枚を消しました）",
        info.width,
        info.height,
        stored / 1024,
        removed.len()
    );
}

/// 画像の目録を保存する。
fn save_image_index() {
    if IMAGES_UNREADABLE.with(Cell::get) {
        return;
    }
    let result = IMAGES.with(|i| clip_image_store::save_index(&i.borrow()));
    if let Err(e) = result {
        log::warn!("画像の履歴: 目録を保存できませんでした: {e}");
    }
}

/// クリップボードの、メモリで渡される形式の中身を読む。`max` より大きいものは読まない。
unsafe fn read_clipboard_bytes(format: u32, max: usize) -> Option<Vec<u8>> {
    if !open_clipboard() {
        return None;
    }
    let bytes = (|| {
        let handle = GetClipboardData(format).ok()?;
        let memory = HGLOBAL(handle.0);
        let size = GlobalSize(memory);
        if size == 0 || size > max {
            return None;
        }
        let ptr = GlobalLock(memory) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
        let _ = GlobalUnlock(memory);
        Some(bytes)
    })();
    let _ = CloseClipboard();
    bytes
}

/// パスワード管理ソフトなどが「記録しないで」と印を付けた内容か。
unsafe fn excluded_from_history() -> bool {
    for name in [
        w!("ExcludeClipboardContentFromMonitorProcessing"),
        w!("Clipboard Viewer Ignore"),
    ] {
        let format = RegisterClipboardFormatW(name);
        if format != 0 && IsClipboardFormatAvailable(format).is_ok() {
            return true;
        }
    }
    // 「クリップボードの履歴（Win+V）に入れてよいか」が 0（入れない）なら記録しない。
    let format = RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"));
    if format != 0 && IsClipboardFormatAvailable(format).is_ok() {
        return read_clipboard_u32(format) == Some(0);
    }
    false
}

/// クリップボードの、4 バイトの数値の形式を読む。
unsafe fn read_clipboard_u32(format: u32) -> Option<u32> {
    if !open_clipboard() {
        return None;
    }
    let value = (|| {
        let handle = GetClipboardData(format).ok()?;
        let memory = HGLOBAL(handle.0);
        if GlobalSize(memory) < 4 {
            return None;
        }
        let ptr = GlobalLock(memory) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let mut bytes = [0u8; 4];
        std::ptr::copy_nonoverlapping(ptr, bytes.as_mut_ptr(), 4);
        let _ = GlobalUnlock(memory);
        Some(u32::from_le_bytes(bytes))
    })();
    let _ = CloseClipboard();
    value
}

/// 積まれている依頼を順に実行する。実行中（貼り付けの待ち時間や履歴の一覧を出している間）に
/// 届いた知らせからは実行しないので、同時に 2 つ実行することはない。
unsafe fn run_queued_requests() {
    if BUSY.with(Cell::get) {
        return;
    }
    BUSY.with(|b| b.set(true));
    loop {
        let request = QUEUE.lock().unwrap_or_else(|p| p.into_inner()).pop_front();
        match request {
            Some(request) => {
                execute(request);
                keyboard::repress_released_modifiers();
            }
            None => break,
        }
    }
    BUSY.with(|b| b.set(false));
}

unsafe fn execute(request: Request) {
    let Request { action, held } = request;
    match action {
        Action::SendKeys(keys) => {
            log::debug!("キー割り当て: キーを送ります（{}）", hotkey_rules::format_key_sequence(&keys));
            sendinput::send_key_sequence(&held, &keys);
        }
        Action::TypeText { text, paste } => {
            let text = hotkey_rules::expand_placeholders(&text, &local_time());
            if paste {
                log::debug!(
                    "キー割り当て: 文字をまとめて貼り付けます（{} 文字）",
                    text.chars().count()
                );
                paste_text(&held, &text);
            } else {
                log::debug!("キー割り当て: 文字を 1 文字ずつ入力します（{} 文字）", text.chars().count());
                sendinput::send_text(&held, &text);
            }
        }
        Action::Run { target, args } => {
            sendinput::suppress_menu(&held);
            run(&target, &args);
        }
        Action::PastePlain => paste_plain(&held),
        Action::PasteTransform(transforms) => paste_transformed(&held, &transforms),
        Action::ClipboardHistory => paste_from_history(&held),
        Action::PasteSnippet(name) => paste_snippet(&held, &name),
        Action::ShowList => {
            sendinput::suppress_menu(&held);
            show_assignment_list();
        }
        Action::ToggleTopmost => {
            sendinput::suppress_menu(&held);
            toggle_topmost();
        }
        Action::Block => sendinput::suppress_menu(&held),
    }
}

/// 今の日付・時刻（PC の時刻）。
unsafe fn local_time() -> LocalTime {
    let t = GetLocalTime();
    LocalTime {
        year: t.wYear,
        month: t.wMonth,
        day: t.wDay,
        hour: t.wHour,
        minute: t.wMinute,
    }
}

/// プログラム・ファイル・URL を開く（エクスプローラーでダブルクリックしたときと同じ）。
unsafe fn run(target: &str, args: &str) {
    log::info!("キー割り当て: 開きます: {target} {args}");
    let target_w = HSTRING::from(target);
    let args_w = HSTRING::from(args);
    let result = ShellExecuteW(
        None,
        w!("open"),
        &target_w,
        if args.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(args_w.as_ptr())
        },
        PCWSTR::null(),
        SW_SHOWNORMAL,
    );
    // 32 以下は失敗（ShellExecuteW の仕様）。
    if result.0 as isize <= 32 {
        log::error!("キー割り当て: 開けませんでした（{target}、コード {}）", result.0 as isize);
        ime_indicator::notify("失敗");
    }
}

/// 書式なしで貼り付ける。クリップボードの文字だけを取り出してクリップボードに入れ直し、
/// Ctrl+V を送る。
unsafe fn paste_plain(held: &Hotkey) {
    let Some(text) = read_clipboard_text() else {
        log::debug!("キー割り当て: クリップボードに文字が無いため、書式なし貼り付けをしません");
        sendinput::release_modifiers(held);
        ime_indicator::notify("空");
        return;
    };
    if let Err(e) = write_clipboard(&text, false) {
        log::error!("キー割り当て: クリップボードに書き込めませんでした: {e}");
        sendinput::release_modifiers(held);
        ime_indicator::notify("失敗");
        return;
    }
    log::debug!("キー割り当て: 書式なしで貼り付けます（{} 文字）", text.chars().count());
    sendinput::send_key_sequence(held, &[ctrl_v()]);
}

/// クリップボードの文字を整えて貼り付ける。クリップボードの中身は変えない
/// （貼り付けたあと、元の内容に戻る）。
unsafe fn paste_transformed(held: &Hotkey, transforms: &[text_transform::Transform]) {
    let Some(text) = read_clipboard_text() else {
        log::debug!("キー割り当て: クリップボードに文字が無いため、整えて貼り付けをしません");
        sendinput::release_modifiers(held);
        ime_indicator::notify("空");
        return;
    };
    let text = text_transform::apply(&text, transforms);
    if text.is_empty() {
        sendinput::release_modifiers(held);
        ime_indicator::notify("空");
        return;
    }
    log::debug!(
        "キー割り当て: 文字を整えて貼り付けます（{}、{} 文字）",
        text_transform::format_list(transforms),
        text.chars().count()
    );
    paste_text(held, &text);
}

/// クリップボードの履歴の一覧（[`history_window`]）を出し、選んだものを貼り付ける。
unsafe fn paste_from_history(held: &Hotkey) {
    // 押されたままの修飾キーを先に離す（一覧でのキー操作に混ざらないように）。
    sendinput::release_modifiers(held);
    if !history_config().record {
        return;
    }
    // 貼り付けている間（一覧を出し直すまでの間）も「出している」とみなし、その間に一覧を出す
    // 操作をされたら、閉じる頼みとして受け取る（新しい依頼として積まない）。
    // 一覧を出したキーの Ctrl は、上で離したので Windows に聞いても分からない。利用者がまだ
    // 押しているか（フックが「離した」を見ていないか）を渡す。
    history_window::begin_session(held.ctrl && keyboard::ctrl_released_by_us());
    show_history_until_closed();
    history_window::end_session();
}

/// 一覧を出し、選ばれたものを貼り付ける。「出したままにする」のときは、閉じられるまで繰り返す。
unsafe fn show_history_until_closed() {
    let target = GetForegroundWindow();
    let mut resume = None;
    loop {
        let config = history_config();
        let items = history_items();
        let snippets = snippets();
        if items.is_empty() && snippets.is_empty() {
            ime_indicator::notify("空");
            return;
        }
        let Some(outcome) =
            history_window::choose(items, snippets, config.view, target, resume.take())
        else {
            ime_indicator::notify("不可");
            return;
        };
        // Ctrl を押しながら選んだときも、出したままにする（続けていくつも貼り付けられる）。
        let keep_open = outcome.keep_open.unwrap_or(config.view.keep_open) || outcome.ctrl_held;
        let place = outcome.resume.clone();
        // 出し直すかを、貼り付ける前に決める。出し直さないなら、ここで一覧を出し終えたことにする
        // （貼り付けている間に押されたほかのキー割り当てを捨てず、一覧を出す操作も閉じる頼みに
        // しない）。出し直すなら、その間に押されたキーは元のアプリに届けず、出し直した一覧に渡す。
        let reopen = keep_open
            && outcome.chosen.is_some()
            && !outcome.left_for_other_window
            && !target.is_invalid()
            && !history_window::close_requested();
        if reopen {
            history_window::set_reopening();
        } else {
            history_window::end_session();
        }
        let Some(moved_to) = apply_history_outcome(outcome, &config, target) else {
            return;
        };
        // 貼り付け先が前面でなくなっていたら（閉じられた など）、出し直さない。貼り付けている間に
        // 一覧を出す操作をされたときも、閉じる。
        if !reopen
            || history_window::close_requested()
            || target.is_invalid()
            || GetForegroundWindow() != target
        {
            return;
        }
        // 貼り付けたものがいちばん上に移ったら、選ぶ位置もそれに合わせる。
        resume = Some(match moved_to {
            Some(position) => place.with_selected(position),
            None => place,
        });
    }
}

/// 一覧の結果を反映する（消す・ピン留め・登録・貼り付け）。貼り付けたら `Some`（貼り付けたものを
/// いちばん上に移したときは、その位置を添える）。
unsafe fn apply_history_outcome(
    outcome: history_window::Outcome,
    config: &HistoryConfig,
    target: HWND,
) -> Option<Option<usize>> {
    // ほかのウィンドウをクリックして閉じたときは、そちらを使いたいので元のウィンドウに戻さない。
    // 定型文の編集を開くときも戻さない（戻すと、開いた画面が元のウィンドウの後ろに隠れる）。
    // 一覧は前面にしないので、ふつうは元のウィンドウが前面のまま（右クリックのメニューを使った
    // あとなど、前面でなくなっていたときだけ戻す）。
    if !target.is_invalid()
        && !outcome.left_for_other_window
        && !outcome.edit_snippets
        && GetForegroundWindow() != target
    {
        let _ = SetForegroundWindow(target);
    }
    if let Some(opacity) = outcome.opacity {
        // 一覧の上で Shift+ホイールで変えた不透明度を、次からも使う。
        HISTORY_CONFIG.lock().unwrap_or_else(|p| p.into_inner()).view.opacity = opacity;
        if let Err(e) = config::save_clipboard_history_opacity(opacity) {
            log::warn!("クリップボードの履歴の不透明度を保存できませんでした: {e}");
        }
    }
    if let Some(keep_open) = outcome.keep_open {
        HISTORY_CONFIG.lock().unwrap_or_else(|p| p.into_inner()).view.keep_open = keep_open;
        if let Err(e) = config::save_clipboard_history_keep_open(keep_open) {
            log::warn!("一覧を出したままにする設定を保存できませんでした: {e}");
        }
    }
    if !outcome.removed.is_empty() {
        let changed = HISTORY.with(|h| {
            let mut h = h.borrow_mut();
            outcome.removed.iter().map(|text| h.remove(text)).filter(|removed| *removed).count() > 0
        });
        if changed {
            log::debug!("クリップボードの履歴: {} 件を消しました", outcome.removed.len());
            mark_history_changed();
        }
    }
    if !outcome.pin_changes.is_empty() {
        // ピン留めを外したものは、いちばん新しいものとして戻す（文字と画像で通しの番号。画像より
        // 下に並ばないように）。
        let mut stamp = next_stamp();
        let changed = HISTORY.with(|h| {
            let mut h = h.borrow_mut();
            outcome.pin_changes.iter().fold(false, |changed, (text, pin)| {
                let done = if *pin {
                    h.pin(text)
                } else {
                    stamp += 1;
                    h.unpin_at(text, stamp - 1)
                };
                done || changed
            })
        });
        if changed {
            mark_history_changed();
        }
    }
    if !outcome.removed_images.is_empty() || !outcome.image_pin_changes.is_empty() {
        IMAGES.with(|i| {
            let mut i = i.borrow_mut();
            for id in &outcome.removed_images {
                i.remove(*id);
            }
            for (id, pin) in &outcome.image_pin_changes {
                i.set_pinned(*id, *pin);
            }
        });
        clip_image_store::delete(&outcome.removed_images);
        let trimmed = IMAGES.with(|i| i.borrow_mut().trim(config.max_items));
        clip_image_store::delete(&trimmed);
        save_image_index();
    }
    if !outcome.registered.is_empty() {
        register_snippets(&outcome.registered);
    }
    if outcome.edit_snippets {
        snippet_window::open();
    }
    let pick = outcome.chosen?;
    let mut moved_to = None;
    let parts = match pick {
        // 印を付けたものは、付けた順に。続く文字は改行でつないで 1 回で貼り付ける。
        history_window::Pick::Many(picks) => {
            let parts: Vec<Part> = picks
                .into_iter()
                .filter_map(|pick| resolve_pick(pick, config, &mut moved_to))
                .collect();
            log::debug!("クリップボードの履歴・定型文から、{} 件をまとめて貼り付けます", parts.len());
            clip_images::batch(parts)
        }
        pick => resolve_pick(pick, config, &mut moved_to).into_iter().collect(),
    };
    // 元のウィンドウに入力が戻ってから貼り付ける。
    pump_messages_until(MENU_REFOCUS_WAIT, || false);
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            // 前の貼り付けを、貼り付け先が終えるのを少し待つ。
            pump_messages_until(MENU_REFOCUS_WAIT, || false);
        }
        paste_part(part);
    }
    // 貼り付けのために離した Shift を、利用者がまだ押していれば押し直す。
    keyboard::repress_released_modifiers();
    Some(moved_to)
}

/// 一覧で選んだものを、貼り付けるもの（文字か画像）にする。履歴の文字と画像は、いちばん上に
/// 移す（設定で選べる。ピン留めしたものは動かさない）。移したら `moved_to` にその位置を入れる。
unsafe fn resolve_pick(
    pick: history_window::Pick,
    config: &HistoryConfig,
    moved_to: &mut Option<usize>,
) -> Option<Part> {
    match pick {
        history_window::Pick::History(text) => {
            let stamp = next_stamp();
            if config.move_to_top && HISTORY.with(|h| h.borrow_mut().move_to_top_at(&text, stamp)) {
                mark_history_changed();
                // ピン留めしたものの次が、いちばん上。
                *moved_to = Some(pinned_count());
            }
            Some(Part::Text(text))
        }
        // 定型文は {date} などを置き換えてから貼り付ける。
        history_window::Pick::Snippet(text) => Some(Part::Text(hotkey_rules::expand_placeholders(
            &text,
            &local_time(),
        ))),
        history_window::Pick::Image(id) => {
            let stamp = next_stamp();
            if config.move_to_top && IMAGES.with(|i| i.borrow_mut().touch(id, stamp)) {
                save_image_index();
                *moved_to = Some(pinned_count());
            }
            Some(Part::Image(id))
        }
        // 整えて貼り付ける。履歴そのものは変えず、並びも動かさない。
        history_window::Pick::Formatted(inner, format) => {
            let text = match *inner {
                history_window::Pick::History(text) => text,
                history_window::Pick::Snippet(text) => {
                    hotkey_rules::expand_placeholders(&text, &local_time())
                }
                _ => return None,
            };
            let text = text_transform::quick_format(&text, format);
            if text.is_empty() {
                ime_indicator::notify("空");
                return None;
            }
            Some(Part::Text(text))
        }
        history_window::Pick::Ocr(id) => recognize_image(id).map(Part::Text),
        // まとめたものの中に、まとめたものは入れない。
        history_window::Pick::Many(_) => None,
    }
}

/// 履歴の画像の文字を、Windows の文字の読み取り（OCR）で読む。読めなかったとき・何も無かったときは、
/// 画面に短く知らせて `None`。
unsafe fn recognize_image(id: u64) -> Option<String> {
    let Some(dib) = clip_image_store::load_image(id) else {
        log::warn!("画像の履歴: 文字を読み取る画像を読めませんでした");
        ime_indicator::notify("不可");
        return None;
    };
    // 読み取りは時間がかかることがあるので、別のスレッドで行い、その間もメッセージを処理する。
    let worker = std::thread::spawn(move || ocr::recognize(&dib));
    pump_messages_until(OCR_WAIT, || worker.is_finished());
    if !worker.is_finished() {
        log::warn!("画像の履歴: 文字の読み取りが終わらないため、やめました");
        ime_indicator::notify("不可");
        return None;
    }
    match worker.join() {
        Ok(Ok(text)) if !text.trim().is_empty() => {
            log::debug!("画像の履歴: 文字を読み取りました（{} 文字）", text.chars().count());
            Some(text)
        }
        Ok(Ok(_)) => {
            log::debug!("画像の履歴: 画像から文字を読み取れませんでした");
            ime_indicator::notify("文字なし");
            None
        }
        Ok(Err(ocr::Failure::NoLanguage)) => {
            log::warn!("画像の履歴: 文字を読み取れる言語が Windows に入っていません");
            ime_indicator::notify("OCR なし");
            None
        }
        Ok(Err(ocr::Failure::Failed(e))) => {
            log::warn!("画像の履歴: 文字を読み取れませんでした: {e}");
            ime_indicator::notify("不可");
            None
        }
        Err(_) => {
            log::warn!("画像の履歴: 文字の読み取りが途中で止まりました");
            ime_indicator::notify("不可");
            None
        }
    }
}

/// 文字か画像を、一覧から選んだものとして貼り付ける。
unsafe fn paste_part(part: &Part) {
    match part {
        Part::Text(text) => {
            log::debug!("クリップボードの履歴・定型文から貼り付けます（{} 文字）", text.chars().count());
            paste_text_with(&Hotkey::unpack(0), text, || send_history_paste());
        }
        Part::Image(id) => paste_image(*id),
    }
}

/// 一覧から選んだものを貼り付ける Ctrl+V を送る。
///
/// - 利用者が Shift を押したままだと Ctrl+Shift+V（値の貼り付け など）になるので、先に離す
///   （貼り付けのあとで [`keyboard::repress_released_modifiers`] が押し直す）。
/// - 利用者が Ctrl を押したまま（続けて貼り付ける）なら、Ctrl の「離した」は送らない。送ると、
///   押しているのに Windows からは離されているように見え、続けて押した Ctrl+Z が `z` になる。
unsafe fn send_history_paste() {
    if GetAsyncKeyState(VK_SHIFT.0 as i32) < 0 {
        sendinput::release_modifiers(&Hotkey {
            shift: true,
            ..Hotkey::unpack(0)
        });
    }
    if history_window::ctrl_held() {
        sendinput::send_v_holding_ctrl();
        // 送る間に利用者が Ctrl を離していたら、押したままにしない。
        if !history_window::ctrl_held() {
            sendinput::release_modifier(VK_CONTROL.0);
        }
    } else {
        sendinput::send_key_sequence(&Hotkey::unpack(0), &[ctrl_v()]);
    }
}

/// ピン留めしている件数（文字と画像の合計。一覧では、その次が「いちばん上」）。
fn pinned_count() -> usize {
    history_items().iter().filter(|item| item.pinned()).count()
}

/// 履歴の画像を貼り付ける。クリップボードに画像を一時的に置いて Ctrl+V を送り、
/// 貼り付け先が読み終えたら元の内容に戻す。
unsafe fn paste_image(id: u64) {
    let Some(dib) = clip_image_store::load_image(id) else {
        log::warn!("画像の履歴: 画像を読めませんでした");
        ime_indicator::notify("不可");
        return;
    };
    log::debug!("画像の履歴から貼り付けます（{} KB）", dib.len() / 1024);
    let saved = save_clipboard();
    if let Err(e) = offer_image(dib) {
        log::warn!("画像の履歴: クリップボードを使えません: {e}");
        ime_indicator::notify("不可");
        return;
    }
    send_history_paste();
    let (accepted, target_hung) = wait_for_paste();
    // 元の内容を控えられなかった（とても大きい・写し取れない形式だけ）ときは、元に戻せない。
    // 貼り付けた画像をクリップボードに残す（予約のままにしない）。元が画像なら履歴にある。
    finish_offer(&saved, CF_DIB);
    if target_hung {
        log::warn!("画像の履歴: 貼り付け先が応答しないため、貼り付けをやめました");
        ime_indicator::notify("応答なし");
    } else if !accepted {
        log::debug!("画像の履歴: 貼り付け先が画像を受け取りませんでした");
        ime_indicator::notify("不可");
    }
}

/// 名前で選んだ定型文を貼り付ける（キー割り当て「定型文を貼り付け」）。
unsafe fn paste_snippet(held: &Hotkey, name: &str) {
    let found = crate::snippets::find_by_name(&snippets(), name).cloned();
    let Some(snippet) = found else {
        log::warn!("キー割り当て: 定型文「{name}」が見つかりません");
        sendinput::release_modifiers(held);
        ime_indicator::notify("なし");
        return;
    };
    let text = hotkey_rules::expand_placeholders(&snippet.text, &local_time());
    log::debug!("キー割り当て: 定型文「{name}」を貼り付けます（{} 文字）", text.chars().count());
    paste_text(held, &text);
}

/// 定型文（[`crate::snippets`]）。起動時・設定の読み込み・定型文の画面で保存したときに差し替える。
static SNIPPETS: Mutex<Vec<Snippet>> = Mutex::new(Vec::new());

/// 定型文を差し替える。
pub fn set_snippets(list: Vec<Snippet>) {
    *SNIPPETS.lock().unwrap_or_else(|p| p.into_inner()) = list;
}

/// 今の定型文。
pub fn snippets() -> Vec<Snippet> {
    SNIPPETS.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// 履歴の一覧で「定型文に登録」したものを、定型文の最後に足して保存する（同じ本文は足さない）。
fn register_snippets(texts: &[String]) {
    let mut list = snippets();
    let mut added = 0;
    for text in texts {
        // 一覧の側で、{ } を {{ }} にしてある（そのまま貼り付けられるように）。
        let Ok(snippet) = crate::snippets::make("", text) else {
            continue;
        };
        if list.len() >= crate::snippets::MAX_SNIPPETS || list.iter().any(|s| s.text == snippet.text) {
            continue;
        }
        list.push(snippet);
        added += 1;
    }
    if added == 0 {
        return;
    }
    if let Err(e) = config::save_snippets(&list) {
        log::error!("定型文を保存できませんでした: {e}");
        ime_indicator::notify("失敗");
        return;
    }
    log::info!("履歴から定型文に {added} 件を登録しました");
    // 一覧の「定型文」タブでも、グループごとにまとまって並ぶようにする。
    crate::snippets::sort_by_group(&mut list, None);
    set_snippets(list);
}

/// 開いている一覧の表示（2 つ以上同時に開かない）。
static LIST_OPEN: AtomicBool = AtomicBool::new(false);

/// 値貼り付けのキーと、キー割り当ての一覧を表示する（トレイメニューとキー割り当てから使う）。
/// 閉じるまで待たない。
pub fn show_assignment_list() {
    if LIST_OPEN.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let settings = config::Settings::load();
        let apps = remap_logic::effective_target_apps(&settings.remap.target_apps);
        let text = hotkey_rules::list_text(
            (keyboard::hotkey(), &apps, keyboard::is_enabled()),
            keyboard::rules_enabled(),
            &settings.hotkeys,
        );
        let text = HSTRING::from(text);
        unsafe {
            MessageBoxW(
                None,
                &text,
                w!("アタイの貼り付け - キー割り当ての一覧"),
                MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
            );
        }
        LIST_OPEN.store(false, Ordering::SeqCst);
    });
}

/// 文字をまとめて（一度に）貼り付ける。貼り付けを受け付けない入力欄では 1 文字ずつ入力する。
///
/// 1. 今のクリップボードの内容を控える。
/// 2. 文字そのものは置かず、「求められたら渡す」（遅延レンダリング）という予約だけを置いて
///    Ctrl+V を送る。予約にはクリップボードの履歴（Win+V）に残さない印を付ける。
/// 3. 貼り付け先が読みに来たら文字を渡す。少し待って、元の内容に戻す。
/// 4. 待っても読みに来なければ、貼り付けを受け付けない入力欄とみなし、元の内容に戻してから
///    1 文字ずつ入力する（予約の段階では文字を渡していないので、二重には入らない）。
///
/// 待っている間に利用者が別のものをコピーした場合は、それを消さないよう元に戻さない。
unsafe fn paste_text(held: &Hotkey, text: &str) {
    paste_text_with(held, text, || sendinput::send_key_sequence(held, &[ctrl_v()]));
}

/// [`paste_text`] の、Ctrl+V の送り方を選べるもの（履歴の一覧からは [`send_history_paste`]）。
unsafe fn paste_text_with(held: &Hotkey, text: &str, send_paste: impl FnOnce()) {
    let saved = save_clipboard();
    if matches!(saved, Saved::Unavailable) {
        // 元の内容を控えられないときは、クリップボードを使うと元に戻せず失われるので、
        // 1 文字ずつ入力する。
        log::debug!("キー割り当て: 元のクリップボードを控えられないため、1 文字ずつ入力します");
        sendinput::send_text(held, text);
        return;
    }
    if let Err(e) = offer_text(text) {
        // クリップボードが使えないときは、1 文字ずつ入力する。
        log::warn!("キー割り当て: クリップボードを使えないため 1 文字ずつ入力します: {e}");
        sendinput::send_text(held, text);
        return;
    }
    send_paste();

    let (accepted, target_hung) = wait_for_paste();
    finish_offer(&saved, CF_UNICODETEXT);

    if target_hung {
        // 最後まで応答が無い。入力しても届かず、あとでまとめて入ってしまうので、あきらめる。
        log::warn!("キー割り当て: 貼り付け先が応答しないため、貼り付けをやめました");
        ime_indicator::notify("応答なし");
    } else if !accepted {
        log::debug!(
            "キー割り当て: 貼り付けを受け付けない入力欄のため、1 文字ずつ入力します（{}）",
            crate::excel_check::foreground_process_name().unwrap_or_else(|| "不明".into())
        );
        // 修飾キーは Ctrl+V を送るときに離してある。ただし、利用者が Ctrl を押したまま履歴の
        // 一覧から選んだときは Ctrl を押したままにしているので、入力の前に離す（押したままだと
        // ショートカットになる。あとで押し直す）。
        let ctrl = GetAsyncKeyState(VK_CONTROL.0 as i32) < 0;
        sendinput::send_text(
            &Hotkey {
                ctrl,
                ..Hotkey::unpack(0)
            },
            text,
        );
    }
}

/// Ctrl+V を送ったあと、貼り付け先が中身を読みに来るのを待つ。(読みに来たか, 相手が固まったままか)。
unsafe fn wait_for_paste() -> (bool, bool) {
    let mut accepted = pump_messages_until(PASTE_ACCEPT_WAIT, || RENDERED.with(Cell::get));
    // 貼り付け先が固まっている（再計算中の Excel など）だけなら、Ctrl+V はまだ相手に届いていない。
    // ここで 1 文字ずつ入力すると、相手が動き出したときに貼り付けと入力の両方が入るので、
    // 相手が応答するまで待つ。
    let mut target_hung = false;
    if !accepted {
        let waiting_since = Instant::now();
        while foreground_is_hung() && waiting_since.elapsed() < PASTE_HUNG_WAIT {
            target_hung = true;
            if pump_messages_until(PASTE_ACCEPT_WAIT, || RENDERED.with(Cell::get)) {
                break;
            }
        }
        if target_hung {
            // 動き出した相手が、届いていた Ctrl+V を処理するのを待つ。
            accepted = pump_messages_until(PASTE_ACCEPT_WAIT, || RENDERED.with(Cell::get));
            target_hung = !accepted && foreground_is_hung();
        }
    }
    if accepted {
        pump_messages_until(PASTE_SETTLE_WAIT, || false);
    }
    (accepted, target_hung)
}

/// 貼り付けのために置いた予約を片付ける。元の内容に戻せたとき（または別のものがコピーされて
/// いたとき）は、予約の中身を捨てる。戻せなかったとき（元の内容を控えられなかった・ほかの
/// アプリがクリップボードを開いたままだった）は、予約を中身に置き換えて、空の予約を残さない
/// （残すと、次の Ctrl+V で何も貼り付けられない）。置き換えもできなければ、中身を持ったままに
/// して、あとで求められたら渡す。
unsafe fn finish_offer(saved: &Saved, format: u32) {
    if restore_after_paste(saved) || render_offer(format) {
        match format {
            CF_DIB => PENDING_IMAGE.with(|p| p.borrow_mut().take()),
            _ => PENDING_TEXT.with(|p| p.borrow_mut().take()),
        };
    }
}

/// 予約しておいた中身を、クリップボードに渡しておく。渡せた（または、もう自分が持ち主でない）
/// なら `true`。
unsafe fn render_offer(format: u32) -> bool {
    if GetClipboardOwner().ok() != Some(clip_window()) {
        return true;
    }
    if !open_clipboard() {
        log::warn!("キー割り当て: クリップボードを使えないため、貼り付けたものを渡せませんでした");
        return false;
    }
    let done = GetClipboardOwner().ok() != Some(clip_window()) || render_pending(format);
    let _ = CloseClipboard();
    done
}

/// 貼り付けのために置いたものを、元の内容に戻す。戻せた（または別のものがコピーされていて、
/// 戻さなくてよい）なら `true`。
unsafe fn restore_after_paste(saved: &Saved) -> bool {
    // 自分が持ち主のままなら元に戻す（別のものがコピーされていたら戻さない）。
    if GetClipboardOwner().ok() != Some(clip_window()) {
        log::debug!("キー割り当て: クリップボードが新しくなったため、元に戻しません");
        return true;
    }
    match saved {
        Saved::Contents(Some(saved)) => match restore_clipboard(saved) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("キー割り当て: クリップボードを元に戻せませんでした: {e}");
                false
            }
        },
        // 控えられなかったときは、元に戻せない（文字はここまで来ない。クリップボードを使わずに
        // 入力する）。
        Saved::Unavailable => false,
        // もともと空（または控えられなかった）なら、空に戻す。
        Saved::Contents(None) => {
            if open_clipboard() {
                let _ = EmptyClipboard();
                let _ = CloseClipboard();
                true
            } else {
                false
            }
        }
    }
}

/// 画像を、遅延レンダリングでクリップボードに置く（[`offer_text`] の画像版）。
unsafe fn offer_image(dib: Vec<u8>) -> Result<(), String> {
    let window = clip_window();
    if window.is_invalid() {
        return Err("クリップボード用のウィンドウがありません".into());
    }
    if !open_clipboard() {
        return Err("ほかのアプリがクリップボードを使っています".into());
    }
    let result = EmptyClipboard().map_err(|e| e.to_string());
    if result.is_ok() {
        PENDING_IMAGE.with(|p| *p.borrow_mut() = Some(dib));
        RENDERED.with(|r| r.set(false));
        let _ = SetClipboardData(CF_DIB, HANDLE(std::ptr::null_mut()));
        mark_exclude_from_history();
    }
    let _ = CloseClipboard();
    result
}

/// 前面のウィンドウが、メッセージに応答していないか。
unsafe fn foreground_is_hung() -> bool {
    let window = GetForegroundWindow();
    if window.is_invalid() {
        return false;
    }
    if IsHungAppWindow(window).as_bool() {
        return true;
    }
    // IsHungAppWindow が「固まっている」と言うのは 5 秒ほどたってから。数秒だけ忙しい相手も
    // 見分けられるよう、何もしないメッセージを送って、すぐに返事が来るかを確かめる。
    let mut result = 0usize;
    SendMessageTimeoutW(
        window,
        WM_NULL,
        WPARAM(0),
        LPARAM(0),
        SMTO_ABORTIFHUNG,
        HUNG_PROBE_MS,
        Some(&mut result),
    )
    .0 == 0
}

/// 文字を、遅延レンダリングでクリップボードに予約する（中身は求められたときに渡す）。
unsafe fn offer_text(text: &str) -> Result<(), String> {
    let window = clip_window();
    if window.is_invalid() {
        return Err("クリップボード用のウィンドウがありません".into());
    }
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
    if !open_clipboard() {
        return Err("ほかのアプリがクリップボードを使っています".into());
    }
    // EmptyClipboard で前の予約が消える（WM_DESTROYCLIPBOARD）ので、そのあとで用意する。
    let result = EmptyClipboard().map_err(|e| e.to_string());
    if result.is_ok() {
        PENDING_TEXT.with(|p| *p.borrow_mut() = Some(bytes));
        RENDERED.with(|r| r.set(false));
        // 中身を渡さずに形式だけを置く（遅延レンダリング）。この呼び出しは成功しても
        // ハンドルを返さないので、結果は見ない。
        let _ = SetClipboardData(CF_UNICODETEXT, HANDLE(std::ptr::null_mut()));
        mark_exclude_from_history();
    }
    let _ = CloseClipboard();
    result
}

/// メッセージを処理しながら、`done` が真になるか `timeout` が過ぎるまで待つ。
/// 貼り付け先からの「文字を渡して」（WM_RENDERFORMAT）は、ここで処理される。
/// `done` が真になったら `true`。
///
/// 次の依頼の知らせ（WM_APP_REQUEST）はここでは取り出さない（今の動作が終わってから
/// 順に実行する）。WM_QUIT を取り出してしまった場合は、置き直して元のループに任せる。
unsafe fn pump_messages_until(timeout: Duration, done: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    let mut msg = MSG::default();
    loop {
        // WM_APP_REQUEST の前後の範囲だけを取り出す（送られてきたメッセージは、
        // 範囲にかかわらず PeekMessageW の中で処理される）。
        while PeekMessageW(&mut msg, None, 0, WM_APP_REQUEST - 1, PM_REMOVE).as_bool()
            || PeekMessageW(&mut msg, None, WM_APP_REQUEST + 1, u32::MAX, PM_REMOVE).as_bool()
        {
            if msg.message == WM_QUIT {
                PostQuitMessage(msg.wParam.0 as i32);
                return done();
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if done() {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 予約しておいた文字を、クリップボードに渡す（クリップボードは開かれている前提）。
/// 渡せなかった（渡すものはあったのに、クリップボードに置けなかった）ときだけ `false`。
unsafe fn render_pending(format: u32) -> bool {
    let bytes = match format {
        CF_UNICODETEXT => PENDING_TEXT.with(|p| p.borrow().clone()),
        CF_DIB => PENDING_IMAGE.with(|p| p.borrow().clone()),
        _ => None,
    };
    let Some(bytes) = bytes else {
        return true;
    };
    if let Ok(memory) = global_copy(&bytes) {
        if SetClipboardData(format, HANDLE(memory.0)).is_err() {
            let _ = GlobalFree(memory);
        } else {
            RENDERED.with(|r| r.set(true));
            return true;
        }
    }
    false
}

/// クリップボード用の見えないウィンドウのプロシージャ。
unsafe extern "system" fn clip_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // 貼り付け先が文字を求めてきた（クリップボードは相手が開いている）。
        WM_RENDERFORMAT => {
            render_pending(wparam.0 as u32);
            LRESULT(0)
        }
        // 持ち主のまま終了するときは、予約を中身に置き換えておく。
        WM_RENDERALLFORMATS => {
            if OpenClipboard(hwnd).is_ok() {
                if GetClipboardOwner().ok() == Some(hwnd) {
                    render_pending(CF_UNICODETEXT);
                    render_pending(CF_DIB);
                }
                let _ = CloseClipboard();
            }
            LRESULT(0)
        }
        // 依頼が積まれた。
        WM_APP_REQUEST => {
            run_queued_requests();
            LRESULT(0)
        }
        // 履歴の使い方が変わった。
        WM_APP_HISTORY => {
            apply_history_setting(hwnd);
            LRESULT(0)
        }
        WM_APP_CLEAR_HISTORY => {
            clear_all_history();
            LRESULT(0)
        }
        // 履歴を保存する時間になった。
        WM_TIMER if wparam.0 == TIMER_SAVE_HISTORY => {
            let _ = KillTimer(hwnd, TIMER_SAVE_HISTORY);
            save_history_now();
            LRESULT(0)
        }
        // クリップボードが変わった（履歴を記録しているときだけ届く）。
        WM_CLIPBOARDUPDATE => {
            record_clipboard();
            LRESULT(0)
        }
        // Windows の終了・サインアウト。まだ保存していない履歴を保存する。
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                save_history_now();
            }
            LRESULT(0)
        }
        // 別のものがコピーされた（または自分で空にした）。予約は無効になる。
        WM_DESTROYCLIPBOARD => {
            PENDING_TEXT.with(|p| p.borrow_mut().take());
            PENDING_IMAGE.with(|p| p.borrow_mut().take());
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// クリップボード用の見えないウィンドウ（メッセージ専用）を作る。作れなければ無効なハンドル。
unsafe fn create_clipboard_window() -> HWND {
    let Ok(instance) = GetModuleHandleW(None) else {
        return HWND::default();
    };
    let class_name = w!("AtaiPasteClipboard");
    let class = WNDCLASSW {
        lpfnWndProc: Some(clip_wnd_proc),
        hInstance: instance.into(),
        lpszClassName: class_name,
        ..Default::default()
    };
    RegisterClassW(&class);
    // メッセージ専用（HWND_MESSAGE）にはしない。Windows の終了・サインアウトの知らせ
    // （WM_ENDSESSION）は、表示していない普通のウィンドウにも届くが、メッセージ専用には届かない。
    CreateWindowExW(
        WS_EX_TOOLWINDOW,
        class_name,
        w!(""),
        WS_POPUP,
        0,
        0,
        0,
        0,
        None,
        None,
        instance,
        None,
    )
    .unwrap_or_default()
}

fn clip_window() -> HWND {
    CLIP_WINDOW.with(Cell::get)
}

/// クリップボードの今の内容に、クリップボードの履歴（Win+V）やクラウドへの同期に残さない
/// 印を付ける（クリップボードは開かれている前提）。
unsafe fn mark_exclude_from_history() {
    let format = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
    if format != 0 {
        if let Ok(mark) = global_copy(&[0]) {
            if SetClipboardData(format, HANDLE(mark.0)).is_err() {
                let _ = GlobalFree(mark);
            }
        }
    }
}

/// Ctrl+V。
fn ctrl_v() -> Hotkey {
    Hotkey {
        ctrl: true,
        shift: false,
        alt: false,
        win: false,
        vk: 0x56,
    }
}

/// 控えたクリップボードの内容（形式と中身）。
struct ClipboardSnapshot(Vec<(u32, Vec<u8>)>);

/// メモリの中身として写し取れる形式か。画像（ビットマップ）やメタファイルなど、
/// メモリ以外のもので渡される形式は写し取れないので除く（画像は CF_DIB として残る）。
/// アプリ専用の形式（CF_PRIVATEFIRST〜LAST）も、中身の持ち方が決まっていないので除く。
fn copyable_format(format: u32) -> bool {
    !matches!(format, 2 | 3 | 9 | 14 | 0x80..=0x8E | 0x200..=0x3FF)
}

/// 控える内容の大きさの上限（大きすぎるものは控えず、クリップボードを使わずに入力する）。
const SNAPSHOT_MAX_BYTES: usize = 256 * 1024 * 1024;

/// 控えた結果。
enum Saved {
    /// 控えられた（空だった場合は中身が空）。
    Contents(Option<ClipboardSnapshot>),
    /// 控えられなかった（ほかのアプリが使っている、写し取れない形式しかない、大きすぎる）。
    /// このときはクリップボードに触らない。
    Unavailable,
}

/// 今のクリップボードの内容を控える（開くのは 1 回だけ。開いている間に中身が変わらないように）。
unsafe fn save_clipboard() -> Saved {
    if !open_clipboard() {
        return Saved::Unavailable;
    }
    let mut items = Vec::new();
    let mut total = 0usize;
    let mut any_format = false;
    let mut format = 0u32;
    let result = loop {
        format = EnumClipboardFormats(format);
        if format == 0 {
            break if items.is_empty() && any_format {
                // 形式はあるのに 1 つも写し取れなかった（メタファイルだけ など）。
                Saved::Unavailable
            } else {
                Saved::Contents((!items.is_empty()).then_some(ClipboardSnapshot(items)))
            };
        }
        any_format = true;
        if !copyable_format(format) {
            continue;
        }
        let Ok(handle) = GetClipboardData(format) else {
            continue;
        };
        let memory = HGLOBAL(handle.0);
        let size = GlobalSize(memory);
        total = total.saturating_add(size);
        if total > SNAPSHOT_MAX_BYTES {
            break Saved::Unavailable;
        }
        let ptr = GlobalLock(memory) as *const u8;
        if ptr.is_null() {
            continue;
        }
        items.push((format, std::slice::from_raw_parts(ptr, size).to_vec()));
        let _ = GlobalUnlock(memory);
    };
    let _ = CloseClipboard();
    result
}

/// 控えた内容をクリップボードに戻す。
unsafe fn restore_clipboard(saved: &ClipboardSnapshot) -> Result<(), String> {
    // 貼り付けたアプリが、読み終えたあともしばらくクリップボードを開いたままのことがある。
    // 元に戻せないと利用者のクリップボードが失われるので、ほかより長く（約 1 秒）待つ。
    if !open_clipboard_with_retries(CLIPBOARD_RETRIES * 5) {
        return Err("ほかのアプリがクリップボードを使っています".into());
    }
    let _ = EmptyClipboard();
    for (format, data) in &saved.0 {
        if let Ok(memory) = global_copy(data) {
            if SetClipboardData(*format, HANDLE(memory.0)).is_err() {
                let _ = GlobalFree(memory);
            }
        }
    }
    let _ = CloseClipboard();
    Ok(())
}

/// バイト列を、クリップボードに渡せるメモリに写す。
unsafe fn global_copy(data: &[u8]) -> Result<HGLOBAL, String> {
    let memory = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1)).map_err(|e| e.to_string())?;
    let ptr = GlobalLock(memory) as *mut u8;
    if ptr.is_null() {
        let _ = GlobalFree(memory);
        return Err("メモリを確保できませんでした".into());
    }
    std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
    let _ = GlobalUnlock(memory);
    Ok(memory)
}

/// クリップボードを開く。ほかのアプリが使っている間は少し待って開き直す。
unsafe fn open_clipboard() -> bool {
    open_clipboard_with_retries(CLIPBOARD_RETRIES)
}

/// クリップボードを開く（開き直す回数を指定する）。
unsafe fn open_clipboard_with_retries(retries: u32) -> bool {
    for _ in 0..retries {
        // 持ち主のウィンドウを渡して開く（None だと EmptyClipboard 後の持ち主が無くなり、
        // SetClipboardData が失敗することがある）。
        if OpenClipboard(clip_window()).is_ok() {
            return true;
        }
        wait_handling_sent_messages(CLIPBOARD_RETRY_WAIT);
    }
    false
}

/// 少し待つ。待っている間も、ほかのアプリから送られてきたメッセージ（貼り付け先からの
/// 「文字を渡して」など）は処理する。処理しないと、相手がクリップボードを開いたまま
/// こちらの返事を待ち続け、いつまでも開けなくなることがある。
unsafe fn wait_handling_sent_messages(duration: Duration) {
    let start = Instant::now();
    let mut msg = MSG::default();
    while start.elapsed() < duration {
        // PM_QS_SENDMESSAGE: 送られてきたメッセージだけを処理し、ほかは取り出さない。
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE | PM_QS_SENDMESSAGE);
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// クリップボードの文字を読む。文字が無ければ `None`。
unsafe fn read_clipboard_text() -> Option<String> {
    if !open_clipboard() {
        return None;
    }
    let text = (|| {
        let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
        let memory = HGLOBAL(handle.0);
        let ptr = GlobalLock(memory) as *const u16;
        if ptr.is_null() {
            return None;
        }
        // 終わりの 0 を探す。相手のデータに 0 が無くても、確保された大きさを超えて読まない。
        let max = GlobalSize(memory) / std::mem::size_of::<u16>();
        let mut len = 0usize;
        while len < max && *ptr.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
        let _ = GlobalUnlock(memory);
        Some(text)
    })();
    let _ = CloseClipboard();
    text.filter(|t| !t.is_empty())
}

/// クリップボードを、文字だけ（書式なし）にする。`exclude_history` が真なら、Windows の
/// クリップボードの履歴（Win+V）やクラウドへの同期に残さないよう印を付ける
/// （一時的に使うだけのとき）。
unsafe fn write_clipboard(text: &str, exclude_history: bool) -> Result<(), String> {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
    let memory = global_copy(&bytes)?;

    if !open_clipboard() {
        let _ = GlobalFree(memory);
        return Err("ほかのアプリがクリップボードを使っています".into());
    }
    let result = EmptyClipboard()
        .and_then(|()| SetClipboardData(CF_UNICODETEXT, HANDLE(memory.0)))
        .map(|_| ())
        .map_err(|e| e.to_string());
    if result.is_err() {
        // 渡せなかったメモリは自分で解放する（渡せたらクリップボードのものになる）。
        let _ = GlobalFree(memory);
    } else if exclude_history {
        mark_exclude_from_history();
    }
    let _ = CloseClipboard();
    result
}

/// 前面のウィンドウを、常に手前に表示する・やめるを切り替える。
unsafe fn toggle_topmost() {
    let foreground = GetForegroundWindow();
    if foreground.is_invalid() {
        return;
    }
    let window = GetAncestor(foreground, GA_ROOT);
    let window = if window.is_invalid() { foreground } else { window };
    // デスクトップや、このアプリ自身の画面は対象外。
    let mut pid = 0u32;
    GetWindowThreadProcessId(window, Some(&mut pid));
    if window == GetShellWindow() || pid == GetCurrentProcessId() {
        return;
    }

    let was_topmost = is_topmost(window);
    let after = if was_topmost { HWND_NOTOPMOST } else { HWND_TOPMOST };
    let result = SetWindowPos(window, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    let now_topmost = is_topmost(window);
    if result.is_err() || now_topmost == was_topmost {
        // 管理者として実行しているアプリなどには効かない。
        log::warn!(
            "キー割り当て: 常に手前に表示を切り替えられませんでした（{}）",
            crate::excel_check::foreground_process_name().unwrap_or_else(|| "不明".into())
        );
        ime_indicator::notify("不可");
        return;
    }
    log::info!(
        "キー割り当て: 常に手前に表示を {} にしました（{}）",
        if now_topmost { "ON" } else { "OFF" },
        crate::excel_check::foreground_process_name().unwrap_or_else(|| "不明".into())
    );
    ime_indicator::notify(if now_topmost { "固定" } else { "解除" });
}

unsafe fn is_topmost(window: HWND) -> bool {
    (GetWindowLongPtrW(window, GWL_EXSTYLE) as u32) & WS_EX_TOPMOST.0 != 0
}
