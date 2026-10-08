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
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetAncestor,
    GetForegroundWindow, GetMessageW, GetShellWindow, GetWindowLongPtrW,
    GetWindowThreadProcessId, KillTimer, MessageBoxW, PeekMessageW, PostMessageW,
    PostQuitMessage, PostThreadMessageW, RegisterClassW, SetForegroundWindow, SetTimer,
    SetWindowPos, TranslateMessage, GA_ROOT, GWL_EXSTYLE, HWND_MESSAGE, HWND_NOTOPMOST,
    HWND_TOPMOST, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MSG, PM_REMOVE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNORMAL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
    WM_CLIPBOARDUPDATE, WM_DESTROYCLIPBOARD, WM_QUIT, WM_RENDERALLFORMATS, WM_RENDERFORMAT,
    WM_TIMER, WNDCLASSW, WS_EX_TOPMOST,
};

use crate::clip_history::{self, History, MenuPosition};
use crate::hotkey_rules::{self, Action, LocalTime};
use crate::remap_logic::Hotkey;
use crate::{clip_store, config, history_window, ime_indicator, keyboard, remap_logic, sendinput, text_transform};

/// クリップボードの文字（`CF_UNICODETEXT`）。
const CF_UNICODETEXT: u32 = 13;
/// クリップボードがほかのアプリに使われているときに、開き直す回数と間隔。
const CLIPBOARD_RETRIES: u32 = 10;
const CLIPBOARD_RETRY_WAIT: Duration = Duration::from_millis(20);

/// 「まとめて貼り付ける」で、Ctrl+V を送ってから貼り付け先が文字を読みに来るのを待つ時間。
/// これを過ぎても読みに来なければ、貼り付けを受け付けない入力欄とみなして 1 文字ずつ入力する。
const PASTE_ACCEPT_WAIT: Duration = Duration::from_millis(500);
/// 貼り付け先が文字を読みに来てから、元のクリップボードに戻すまでの待ち時間
/// （同じ貼り付けの中で、別の形式を続けて読みに来ることがあるため）。
const PASTE_SETTLE_WAIT: Duration = Duration::from_millis(300);

thread_local! {
    /// クリップボードの持ち主になる、見えないウィンドウ（実行スレッドだけで使う）。
    static CLIP_WINDOW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    /// 遅延レンダリングで、求められたら渡す文字（UTF-16 と終端の 0 をバイト列にしたもの）。
    static PENDING_TEXT: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
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
    /// 一覧の出し方（位置・幅・不透明度・1 ページの件数）。
    pub view: history_window::View,
}

static HISTORY_CONFIG: Mutex<HistoryConfig> = Mutex::new(HistoryConfig {
    record: false,
    max_items: clip_history::DEFAULT_ITEMS,
    keep: false,
    view: history_window::View {
        position: MenuPosition::Caret,
        width: clip_history::DEFAULT_WIDTH,
        opacity: clip_history::DEFAULT_OPACITY,
        page_size: clip_history::DEFAULT_PAGE_SIZE,
    },
});

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

/// 履歴の一覧を閉じてから、元のウィンドウに入力が戻るのを待つ時間。
const MENU_REFOCUS_WAIT: Duration = Duration::from_millis(80);

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
    QUEUE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push_back(Request { action, held });
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
    // 二重に登録しないよう、いったん外してから必要なら登録し直す。
    let _ = RemoveClipboardFormatListener(window);
    if !config.record {
        // 記録をやめたら、覚えている履歴も保存した履歴も消す。
        HISTORY.with(|h| h.borrow_mut().clear());
        HISTORY_DIRTY.with(|d| d.set(false));
        HISTORY_LOADED.with(|l| l.set(false));
        clip_store::delete();
        return;
    }
    if !HISTORY_LOADED.with(Cell::get) {
        HISTORY_LOADED.with(|l| l.set(true));
        if config.keep {
            let saved = clip_store::load();
            log::debug!("保存していたクリップボードの履歴を読みました（{} 件）", saved.len());
            HISTORY.with(|h| *h.borrow_mut() = History::from_saved(saved, config.max_items));
        }
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
    let window = clip_window();
    if !window.is_invalid() {
        SetTimer(window, TIMER_SAVE_HISTORY, SAVE_HISTORY_DELAY_MS, None);
    }
}

/// まだ保存していない履歴があれば、今すぐ保存する。
fn save_history_now() {
    if !HISTORY_DIRTY.with(|d| d.replace(false)) || !history_config().keep {
        return;
    }
    let items: Vec<String> = HISTORY.with(|h| h.borrow().items().cloned().collect());
    if let Err(e) = clip_store::save(&items) {
        log::warn!("クリップボードの履歴を保存できませんでした: {e}");
    }
}

/// 覚えている履歴と、保存した履歴を消す。
fn clear_all_history() {
    HISTORY.with(|h| h.borrow_mut().clear());
    HISTORY_DIRTY.with(|d| d.set(false));
    clip_store::delete();
    log::debug!("クリップボードの履歴を消しました");
}

/// クリップボードが変わった。記録してよい文字なら履歴に加える。
unsafe fn record_clipboard() {
    // 貼り付けのために自分で一時的に置いたもの・元に戻したものは記録しない。
    let owner = GetClipboardOwner().ok();
    if owner.is_some() && owner == Some(clip_window()) {
        return;
    }
    if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() || excluded_from_history() {
        return;
    }
    if let Some(text) = read_clipboard_text() {
        if HISTORY.with(|h| h.borrow_mut().push(&text)) {
            mark_history_changed();
        }
    }
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
            Some(request) => execute(request),
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
    let config = history_config();
    if !config.record {
        return;
    }
    let items: Vec<String> = HISTORY.with(|h| h.borrow().items().cloned().collect());
    if items.is_empty() {
        ime_indicator::notify("空");
        return;
    }
    let target = GetForegroundWindow();
    let Some(outcome) = history_window::choose(items, config.view, target) else {
        ime_indicator::notify("不可");
        return;
    };
    if !target.is_invalid() {
        let _ = SetForegroundWindow(target);
    }
    if let Some(opacity) = outcome.opacity {
        // 一覧の上で Shift+ホイールで変えた不透明度を、次からも使う。
        HISTORY_CONFIG.lock().unwrap_or_else(|p| p.into_inner()).view.opacity = opacity;
        if let Err(e) = config::save_clipboard_history_opacity(opacity) {
            log::warn!("クリップボードの履歴の不透明度を保存できませんでした: {e}");
        }
    }
    if !outcome.removed.is_empty() {
        let changed = HISTORY.with(|h| {
            let mut h = h.borrow_mut();
            outcome.removed.iter().fold(false, |changed, text| h.remove(text) || changed)
        });
        if changed {
            log::debug!("クリップボードの履歴: {} 件を消しました", outcome.removed.len());
            mark_history_changed();
        }
    }
    if let Some(text) = outcome.chosen {
        // 元のウィンドウに入力が戻ってから貼り付ける。
        pump_messages_until(MENU_REFOCUS_WAIT, || false);
        log::debug!("クリップボードの履歴から貼り付けます（{} 文字）", text.chars().count());
        paste_text(&Hotkey::unpack(0), &text);
    }
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
    let saved = save_clipboard();
    if let Err(e) = offer_text(text) {
        // クリップボードが使えないときは、1 文字ずつ入力する。
        log::warn!("キー割り当て: クリップボードを使えないため 1 文字ずつ入力します: {e}");
        sendinput::send_text(held, text);
        return;
    }
    sendinput::send_key_sequence(held, &[ctrl_v()]);

    let accepted = pump_messages_until(PASTE_ACCEPT_WAIT, || RENDERED.with(Cell::get));
    if accepted {
        pump_messages_until(PASTE_SETTLE_WAIT, || false);
    }
    PENDING_TEXT.with(|p| p.borrow_mut().take());

    // 自分が持ち主のままなら元に戻す（別のものがコピーされていたら戻さない）。
    if GetClipboardOwner().ok() == Some(clip_window()) {
        match &saved {
            Saved::Contents(Some(saved)) => {
                if let Err(e) = restore_clipboard(saved) {
                    log::warn!("キー割り当て: クリップボードを元に戻せませんでした: {e}");
                }
            }
            Saved::Unavailable => {
                // 元の内容を控えられなかったので、空にして消してしまわないよう、そのままにする。
                log::warn!("キー割り当て: 元のクリップボードを控えられなかったため、元に戻しません");
            }
            Saved::Contents(None) => {
                // もともと空（または控えられなかった）なら、空に戻す。
                if open_clipboard() {
                    let _ = EmptyClipboard();
                    let _ = CloseClipboard();
                }
            }
        }
    } else {
        log::debug!("キー割り当て: クリップボードが新しくなったため、元に戻しません");
    }

    if !accepted {
        log::debug!(
            "キー割り当て: 貼り付けを受け付けない入力欄のため、1 文字ずつ入力します（{}）",
            crate::excel_check::foreground_process_name().unwrap_or_else(|| "不明".into())
        );
        // 修飾キーは Ctrl+V を送るときに離してある。
        sendinput::send_text(&Hotkey::unpack(0), text);
    }
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
unsafe fn render_pending_text() {
    let bytes = PENDING_TEXT.with(|p| p.borrow().clone());
    let Some(bytes) = bytes else {
        return;
    };
    if let Ok(memory) = global_copy(&bytes) {
        if SetClipboardData(CF_UNICODETEXT, HANDLE(memory.0)).is_err() {
            let _ = GlobalFree(memory);
        } else {
            RENDERED.with(|r| r.set(true));
        }
    }
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
            if wparam.0 as u32 == CF_UNICODETEXT {
                render_pending_text();
            }
            LRESULT(0)
        }
        // 持ち主のまま終了するときは、予約を中身に置き換えておく。
        WM_RENDERALLFORMATS => {
            if OpenClipboard(hwnd).is_ok() {
                if GetClipboardOwner().ok() == Some(hwnd) {
                    render_pending_text();
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
        // 別のものがコピーされた（または自分で空にした）。予約は無効になる。
        WM_DESTROYCLIPBOARD => {
            PENDING_TEXT.with(|p| p.borrow_mut().take());
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
    CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class_name,
        w!(""),
        WINDOW_STYLE::default(),
        0,
        0,
        0,
        0,
        HWND_MESSAGE,
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
fn copyable_format(format: u32) -> bool {
    !matches!(format, 2 | 3 | 9 | 14 | 0x80..=0x8E | 0x300..=0x3FF)
}

/// 控えた結果。
enum Saved {
    /// 控えられた（空だった場合は中身が空）。
    Contents(Option<ClipboardSnapshot>),
    /// ほかのアプリが使っていて開けず、控えられなかった。
    Unavailable,
}

/// 今のクリップボードの内容を控える。
unsafe fn save_clipboard() -> Saved {
    if !open_clipboard() {
        return Saved::Unavailable;
    }
    let _ = CloseClipboard();
    Saved::Contents(snapshot_clipboard())
}

/// 今のクリップボードの内容を控える。空なら `None`。
unsafe fn snapshot_clipboard() -> Option<ClipboardSnapshot> {
    if !open_clipboard() {
        return None;
    }
    let mut items = Vec::new();
    let mut format = 0u32;
    loop {
        format = EnumClipboardFormats(format);
        if format == 0 {
            break;
        }
        if !copyable_format(format) {
            continue;
        }
        let Ok(handle) = GetClipboardData(format) else {
            continue;
        };
        let memory = HGLOBAL(handle.0);
        let size = GlobalSize(memory);
        let ptr = GlobalLock(memory) as *const u8;
        if ptr.is_null() {
            continue;
        }
        items.push((format, std::slice::from_raw_parts(ptr, size).to_vec()));
        let _ = GlobalUnlock(memory);
    }
    let _ = CloseClipboard();
    (!items.is_empty()).then_some(ClipboardSnapshot(items))
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
        std::thread::sleep(CLIPBOARD_RETRY_WAIT);
    }
    false
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
