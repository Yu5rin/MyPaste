//! キー割り当ての動作を実行する。
//!
//! キーボードフック（[`crate::keyboard`]）は、割り当てたキーを捕まえたら [`dispatch`] で
//! 動作をここへ渡すだけにして、すぐに戻る。フックの中で時間のかかること（プログラムを開く、
//! クリップボードを読み書きするなど）をすると、Windows がフックを外してしまうことがあるため、
//! 実行は専用のスレッドで行う。
//!
//! 動作の種類と内容の解釈は [`crate::hotkey_rules`] にある。

use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetForegroundWindow, GetShellWindow, GetWindowLongPtrW,
    GetWindowThreadProcessId, SetWindowPos, GA_ROOT, GWL_EXSTYLE, HWND_NOTOPMOST, HWND_TOPMOST,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNORMAL, WS_EX_TOPMOST,
};

use crate::hotkey_rules::{self, Action, LocalTime};
use crate::remap_logic::Hotkey;
use crate::{ime_indicator, sendinput};

/// クリップボードの文字（`CF_UNICODETEXT`）。
const CF_UNICODETEXT: u32 = 13;
/// クリップボードがほかのアプリに使われているときに、開き直す回数と間隔。
const CLIPBOARD_RETRIES: u32 = 10;
const CLIPBOARD_RETRY_WAIT: Duration = Duration::from_millis(20);

/// 実行の依頼。
struct Request {
    action: Action,
    /// 押されたままの修飾キー（割り当てたキーの組み合わせ）。
    held: Hotkey,
}

/// 実行スレッドへの送り口。フックから使う。
static SENDER: Mutex<Option<Sender<Request>>> = Mutex::new(None);

/// 実行スレッドのハンドル。[`Worker::stop`] で止める。
pub struct Worker {
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    /// 実行スレッドを止め、終わるのを待つ（実行中の動作は最後まで行う）。
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        // 送り口を閉じると、実行スレッドの受信が終わってループを抜ける。
        SENDER.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(thread) = self.thread.take() {
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
    let (tx, rx) = mpsc::channel::<Request>();
    *SENDER.lock().unwrap_or_else(|p| p.into_inner()) = Some(tx);
    let thread = std::thread::spawn(move || unsafe {
        // ShellExecuteW のために COM を初期化しておく（関連付けによっては COM を使う）。
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        for request in rx {
            execute(request);
        }
        if com {
            CoUninitialize();
        }
    });
    Worker {
        thread: Some(thread),
    }
}

/// 動作の実行を依頼する（キーボードフックから呼ぶ。すぐに戻る）。
pub fn dispatch(action: Action, held: Hotkey) {
    if let Some(tx) = SENDER.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        let _ = tx.send(Request { action, held });
    }
}

unsafe fn execute(request: Request) {
    let Request { action, held } = request;
    match action {
        Action::SendKeys(keys) => {
            log::debug!("キー割り当て: キーを送ります（{}）", hotkey_rules::format_key_sequence(&keys));
            sendinput::send_key_sequence(&held, &keys);
        }
        Action::TypeText(text) => {
            let text = hotkey_rules::expand_placeholders(&text, &local_time());
            log::debug!("キー割り当て: 文字を入力します（{} 文字）", text.chars().count());
            sendinput::send_text(&held, &text);
        }
        Action::Run { target, args } => {
            sendinput::suppress_menu(&held);
            run(&target, &args);
        }
        Action::PastePlain => paste_plain(&held),
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
    if let Err(e) = write_clipboard_text(&text) {
        log::error!("キー割り当て: クリップボードに書き込めませんでした: {e}");
        sendinput::release_modifiers(held);
        ime_indicator::notify("失敗");
        return;
    }
    log::debug!("キー割り当て: 書式なしで貼り付けます（{} 文字）", text.chars().count());
    let ctrl_v = Hotkey {
        ctrl: true,
        shift: false,
        alt: false,
        win: false,
        vk: 0x56,
    };
    sendinput::send_key_sequence(held, &[ctrl_v]);
}

/// クリップボードを開く。ほかのアプリが使っている間は少し待って開き直す。
unsafe fn open_clipboard() -> bool {
    for _ in 0..CLIPBOARD_RETRIES {
        if OpenClipboard(None).is_ok() {
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
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
        let _ = GlobalUnlock(memory);
        Some(text)
    })();
    let _ = CloseClipboard();
    text.filter(|t| !t.is_empty())
}

/// クリップボードを、文字だけ（書式なし）にする。
unsafe fn write_clipboard_text(text: &str) -> Result<(), String> {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = units.len() * std::mem::size_of::<u16>();
    let memory = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| e.to_string())?;
    let ptr = GlobalLock(memory) as *mut u16;
    if ptr.is_null() {
        let _ = GlobalFree(memory);
        return Err("メモリを確保できませんでした".into());
    }
    std::ptr::copy_nonoverlapping(units.as_ptr(), ptr, units.len());
    let _ = GlobalUnlock(memory);

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
