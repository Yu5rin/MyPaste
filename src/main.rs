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

// 本番（release）ビルドではコンソールウィンドウを出さない。
// デバッグビルドではパニック出力などを確認できるよう残す。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod excel_check;
mod keyboard;
mod sendinput;
mod startup;
mod tray;

// 仕様上のファイル名 log.rs を保ちつつ、`log` クレートと名前が衝突しないよう
// モジュール名は logging とする。
#[path = "log.rs"]
mod logging;

use std::sync::mpsc;
use std::thread;

use tray_item::IconSource;

use windows::Win32::Foundation::{LPARAM, WPARAM};

/// タスクトレイのメニュー操作をメインスレッドへ伝えるメッセージ。
pub enum TrayMessage {
    /// ON/OFF 切替
    Toggle,
    /// 自動起動 ON/OFF 切替
    ToggleStartup,
    /// 終了
    Quit,
}

fn main() {
    logging::init();
    log::info!("アタイの貼り付け 起動");

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
    let (tx, rx) = mpsc::channel::<TrayMessage>();
    let mut tray = match tray::build(tx) {
        Ok(t) => t,
        Err(e) => {
            log::error!("トレイ初期化失敗: {e}");
            post_quit(hook_tid);
            let _ = hook_thread.join();
            return;
        }
    };

    // --- メインループ: トレイのメニュー操作を処理 ---
    for msg in rx {
        match msg {
            TrayMessage::Toggle => {
                let on = keyboard::toggle();
                let icon = if on { tray::ICON_ON } else { tray::ICON_OFF };
                if let Err(e) = tray.set_icon(IconSource::Resource(icon)) {
                    log::warn!("アイコン更新失敗: {e}");
                }
                log::info!("ON/OFF 切替: {}", if on { "ON" } else { "OFF" });
            }
            TrayMessage::ToggleStartup => match startup::toggle() {
                Ok(enabled) => {
                    log::info!("自動起動: {}", if enabled { "有効" } else { "無効" })
                }
                Err(e) => log::error!("自動起動設定失敗: {e}"),
            },
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
