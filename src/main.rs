//! アタイの貼り付け — エントリーポイント。
//!
//! Excel 使用時に `Ctrl+B` を `Ctrl+Shift+V` にリマップする常駐アプリ。
//!
//! ## スレッド構成
//! - **フックスレッド**: 低レベルキーボードフックを設置し、メッセージループを回して
//!   フックのコールバック配信を受ける（LL フックにはメッセージループが必須）。
//! - **メインスレッド**: タスクトレイ（tray-item）を保持し、メニュー操作を
//!   チャネル経由で受け取ってアイコン切替・自動起動切替・終了処理を行う。
//!   tray-item は内部で独自のメッセージループを持つため、メインスレッドは
//!   チャネル受信に専念できる。
//! - **更新スレッド**: 更新の確認・ダウンロード・適用を行う。通信が起動や
//!   キー操作を妨げないよう、必ず別スレッドで実行する。

// 本番（release）ビルドではコンソールウィンドウを出さない。
// デバッグビルドではパニック出力などを確認できるよう残す。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod excel_check;
mod http;
mod keyboard;
mod sendinput;
mod startup;
mod tray;
mod update;

// 仕様上のファイル名 log.rs を保ちつつ、`log` クレートと名前が衝突しないよう
// モジュール名は logging とする。
#[path = "log.rs"]
mod logging;

use std::sync::mpsc;
use std::thread;

use windows::Win32::Foundation::{LPARAM, WPARAM};

/// タスクトレイのメニュー操作をメインスレッドへ伝えるメッセージ。
pub enum TrayMessage {
    /// ON/OFF 切替
    Toggle,
    /// 自動起動 ON/OFF 切替
    ToggleStartup,
    /// 更新を確認（メニューからの手動操作）
    CheckUpdate,
    /// 更新ファイルのダウンロード進捗（パーセント）
    UpdateProgress(u64),
    /// 更新処理が終わった（進捗表示を戻す）
    UpdateFinished,
    /// 終了
    Quit,
}

fn main() {
    logging::init();
    log::info!("アタイの貼り付け 起動 (v{})", env!("CARGO_PKG_VERSION"));

    // 前回の更新で残った .old があれば削除する。
    update::cleanup_old();
    let settings = config::Settings::load();

    // --- フックスレッドを起動し、そのスレッド ID を受け取る ---
    let (id_tx, id_rx) = mpsc::channel::<u32>();
    let hook_thread = thread::spawn(move || hook_thread_main(id_tx));
    // フック設置後に送られてくるスレッド ID を待つ（終了時の WM_QUIT 送信に使う）。
    let hook_tid = id_rx.recv().unwrap_or(0);
    if hook_tid == 0 {
        log::error!("フックスレッドの初期化に失敗したため終了します");
        let _ = hook_thread.join();
        return;
    }

    // --- タスクトレイを構築 ---
    // 起動時の状態（キーリマップは ON、自動起動は現在の設定）をメニューに反映する。
    let (tx, rx) = mpsc::channel::<TrayMessage>();
    let mut tray = match tray::build(tx.clone(), keyboard::is_enabled(), startup::is_enabled()) {
        Ok(t) => t,
        Err(e) => {
            log::error!("トレイ初期化失敗: {e}");
            post_quit(hook_tid);
            let _ = hook_thread.join();
            return;
        }
    };

    // --- 起動時の更新確認（設定で有効な場合のみ、1 日 1 回まで） ---
    // 通信が起動を妨げないよう別スレッドで行う。
    if settings.update.check_on_startup {
        let tx_update = tx.clone();
        let settings_for_update = settings.clone();
        thread::spawn(move || update::run(settings_for_update, tx_update, false));
    }

    // --- メインループ: トレイのメニュー操作を処理 ---
    for msg in rx {
        match msg {
            TrayMessage::Toggle => {
                let on = keyboard::toggle();
                if let Err(e) = tray.set_icon(on) {
                    log::warn!("アイコン更新失敗: {e}");
                }
                if let Err(e) = tray.set_remap_checked(on) {
                    log::warn!("メニュー更新失敗: {e}");
                }
                log::info!("キーリマップ: {}", if on { "有効" } else { "無効" });
            }
            TrayMessage::ToggleStartup => match startup::toggle() {
                Ok(enabled) => {
                    if let Err(e) = tray.set_startup_checked(enabled) {
                        log::warn!("メニュー更新失敗: {e}");
                    }
                    log::info!("自動起動: {}", if enabled { "有効" } else { "無効" })
                }
                Err(e) => log::error!("自動起動設定失敗: {e}"),
            },
            TrayMessage::CheckUpdate => {
                // 手動確認。結果（最新である／失敗した）もダイアログで知らせる。
                let tx_update = tx.clone();
                let settings_for_update = settings.clone();
                thread::spawn(move || update::run(settings_for_update, tx_update, true));
            }
            TrayMessage::UpdateProgress(percent) => {
                if let Err(e) = tray.set_progress(percent) {
                    log::warn!("進捗表示の更新に失敗: {e}");
                }
            }
            TrayMessage::UpdateFinished => {
                if let Err(e) = tray.clear_progress() {
                    log::warn!("進捗表示の復帰に失敗: {e}");
                }
            }
            TrayMessage::Quit => {
                log::info!("終了要求を受信");
                break;
            }
        }
    }

    // --- 後始末: フックスレッドを終了させて待つ ---
    post_quit(hook_tid);
    let _ = hook_thread.join();
    log::info!("アタイの貼り付け 終了");
}

/// フックスレッド本体。フックを設置し、メッセージループを回す。
fn hook_thread_main(id_tx: mpsc::Sender<u32>) {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, TranslateMessage, MSG};

    unsafe {
        if let Err(e) = keyboard::install() {
            log::error!("キーボードフック設置失敗: {e}");
            let _ = id_tx.send(0);
            return;
        }

        // このスレッドがメッセージループを持つので、スレッド ID を通知する。
        let _ = id_tx.send(GetCurrentThreadId());
        log::info!("キーボードフック設置完了");

        // LL フックのコールバック配信のためにメッセージループを回す。
        let mut msg = MSG::default();
        loop {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            // 0 = WM_QUIT で終了、-1 = エラー。どちらもループを抜ける。
            if ret.0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // フック解除（リソースリーク防止）。
        keyboard::uninstall();
        log::info!("キーボードフック解除完了");
    }
}

/// 指定スレッドへ WM_QUIT を送り、メッセージループを終了させる。
fn post_quit(thread_id: u32) {
    use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
    unsafe {
        let _ = PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
    }
}
