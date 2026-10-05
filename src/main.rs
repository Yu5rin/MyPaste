//! アタイの貼り付け — エントリーポイント。
//!
//! Excel 使用時に `Ctrl+B` を `Ctrl+Shift+V` にリマップする常駐アプリ。
//! キーの組み合わせと対象アプリは設定画面（[`settings_window`]）で変えられる。
//!
//! ## スレッド構成
//! - **フックスレッド**: 低レベルキーボードフックを設置し、メッセージループを回して
//!   フックのコールバック配信を受ける（LL フックにはメッセージループが必須）。
//! - **メインスレッド**: タスクトレイ（tray-item）を保持し、メニュー操作を
//!   チャネル経由で受け取ってアイコン切替・自動起動切替・終了処理を行う。
//!   tray-item は内部で独自のメッセージループを持つため、メインスレッドは
//!   チャネル受信に専念できる。終了要求を受けたときは、更新の入れ替えが
//!   実行中であれば（上限付きで）完了を待ってから終了する
//!   （[`wait_for_update_to_finish`]）。
//! - **入力モード表示スレッド**: IME の入力モードを監視し、切り替えた瞬間に
//!   画面中央へ表示する（[`ime_indicator`]）。独自のメッセージループを持ち、
//!   終了時に WM_QUIT を送って止める。
//! - **設定画面スレッド**: 設定画面を開いている間だけ動く（[`settings_window`]）。
//!   保存した内容はチャネルでメインスレッドへ送り、メインスレッドが反映する。
//! - **更新スレッド**: 更新の確認・ダウンロード・適用を行う。通信が起動や
//!   キー操作を妨げないよう、必ず別スレッドで実行する。多重実行の防止は
//!   `update::run` 内部で行う。

// 本番（release）ビルドではコンソールウィンドウを出さない。
// デバッグビルドではパニック出力などを確認できるよう残す。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod excel_check;
mod http;
mod ime_indicator;
mod ime_logic;
mod keyboard;
mod remap_logic;
mod sendinput;
mod settings_window;
mod startup;
mod tray;
mod update;
mod update_logic;

// 仕様上のファイル名 log.rs を保ちつつ、`log` クレートと名前が衝突しないよう
// モジュール名は logging とする。
#[path = "log.rs"]
mod logging;

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK};

/// タスクトレイのメニュー操作をメインスレッドへ伝えるメッセージ。
pub enum TrayMessage {
    /// ON/OFF 切替
    Toggle,
    /// 自動起動 ON/OFF 切替
    ToggleStartup,
    /// 入力モード表示 ON/OFF 切替
    ToggleImeIndicator,
    /// 設定画面を開く
    OpenSettings,
    /// 設定画面で保存された（settings.json への書き込みは済んでいる。反映だけ行う）
    SettingsSaved(Box<config::Settings>),
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
    let mut settings = config::Settings::load();

    // キーリマップのキーと対象アプリを、フックを設置する前に決めておく。
    let hotkey = configure_remap(&settings);

    // --- フックスレッドを起動し、そのスレッド ID を受け取る ---
    // 設置に失敗した場合は Err(エラー内容) が届く。詳細をダイアログに出せるよう
    // スレッド ID だけでなくエラーメッセージも受け渡せる型にしている。
    let (id_tx, id_rx) = mpsc::channel::<Result<u32, String>>();
    let hook_thread = thread::spawn(move || hook_thread_main(id_tx));
    let hook_tid = match id_rx.recv() {
        Ok(Ok(tid)) => tid,
        Ok(Err(e)) => {
            log::error!("フックスレッドの初期化に失敗したため終了します: {e}");
            // 本番ビルドはロガーを初期化しないため、これを出さないと利用者には
            // 「起動しても何も起きない」ようにしか見えない。
            update::message_box(
                &format!(
                    "キーボードフックの設置に失敗したため、アプリを起動できません。\n\n\
                     他のキーボード関連ソフトと競合している可能性があります。\n\n詳細: {e}"
                ),
                MB_OK | MB_ICONERROR,
            );
            let _ = hook_thread.join();
            return;
        }
        Err(_) => {
            // フックスレッドが応答を送らずに終了した（パニックなど）。
            log::error!("フックスレッドから応答がなかったため終了します");
            update::message_box(
                "キーボードフックの初期化中に問題が発生したため、アプリを起動できません。",
                MB_OK | MB_ICONERROR,
            );
            let _ = hook_thread.join();
            return;
        }
    };

    // --- タスクトレイを構築 ---
    // 起動時の状態（キーリマップは ON、自動起動は現在の設定）をメニューに反映する。
    let (tx, rx) = mpsc::channel::<TrayMessage>();
    // --- 入力モード表示を開始（失敗してもこの機能が動かないだけで、起動は続ける） ---
    let ime_indicator = ime_indicator::start(&settings.ime_indicator);

    let mut tray = match tray::build(
        tx.clone(),
        &hotkey,
        keyboard::is_enabled(),
        startup::is_enabled(),
        ime_indicator.is_some() && ime_indicator::is_enabled(),
    ) {
        Ok(t) => t,
        Err(e) => {
            log::error!("トレイ初期化失敗: {e}");
            // フック設置失敗の場合と同様、本番ビルドではログが出ないため
            // MessageBox で知らせる（さもないと無言で終了してしまう）。
            update::message_box(
                &format!(
                    "タスクトレイの初期化に失敗したため、アプリを起動できません。\n\n詳細: {e}"
                ),
                MB_OK | MB_ICONERROR,
            );
            if let Some(indicator) = ime_indicator {
                indicator.stop();
            }
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
            TrayMessage::ToggleImeIndicator => {
                if ime_indicator.is_none() {
                    // 起動時に表示ウィンドウを作れなかった。切り替えても動かないので知らせる。
                    update::message_box(
                        "入力モード表示を開始できなかったため、切り替えられません。",
                        MB_OK | MB_ICONERROR,
                    );
                    continue;
                }
                let on = !ime_indicator::is_enabled();
                ime_indicator::set_enabled(on);
                settings.ime_indicator.enabled = on;
                if let Err(e) = tray.set_ime_indicator_checked(on) {
                    log::warn!("メニュー更新失敗: {e}");
                }
                // 次回の起動でも同じ状態になるよう settings.json に保存する。
                if let Err(e) = config::save_ime_indicator_enabled(on) {
                    log::error!("入力モード表示の設定を保存できませんでした: {e}");
                    update::message_box(
                        &format!(
                            "入力モード表示の設定を保存できませんでした。\n\
                             今回の起動中は切り替えた状態で動きますが、次回の起動では元に戻ります。\n\n詳細: {e}"
                        ),
                        MB_OK | MB_ICONERROR,
                    );
                }
                log::info!("入力モード表示: {}", if on { "有効" } else { "無効" });
            }
            TrayMessage::OpenSettings => {
                // トレイで切り替えた入力モード表示の ON/OFF は settings にも入れてあるが、
                // 念のため実際の状態を渡す。
                let mut current = settings.clone();
                current.ime_indicator.enabled = ime_indicator::is_enabled();
                settings_window::open(tx.clone(), current, startup::is_enabled());
            }
            TrayMessage::SettingsSaved(saved) => {
                settings = *saved;
                let hotkey = configure_remap(&settings);
                ime_indicator::set_params(
                    ime_indicator::Timing {
                        hold_ms: settings.ime_indicator.hold_ms,
                        fade_ms: settings.ime_indicator.fade_ms,
                    },
                    settings.ime_indicator.size,
                );
                if ime_indicator.is_some() {
                    ime_indicator::set_enabled(settings.ime_indicator.enabled);
                }
                if let Err(e) = tray.set_remap_hotkey(&hotkey, keyboard::is_enabled()) {
                    log::warn!("メニュー更新失敗: {e}");
                }
                if let Err(e) = tray.set_ime_indicator_checked(
                    ime_indicator.is_some() && ime_indicator::is_enabled(),
                ) {
                    log::warn!("メニュー更新失敗: {e}");
                }
                if let Err(e) = tray.set_startup_checked(startup::is_enabled()) {
                    log::warn!("メニュー更新失敗: {e}");
                }
                log::info!("設定を反映しました（キー: {}）", hotkey.format());
            }
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
                // 更新の入れ替え（exe ↔ .old ↔ .new のリネーム）が進行中に
                // プロセスを終了させると実行ファイルが壊れる恐れがあるため、
                // 完了を待ってから終了する（通信詰まりなどに備え上限を設ける）。
                wait_for_update_to_finish();
                break;
            }
        }
    }

    // --- 後始末: 設定画面・入力モード表示・フックスレッドを終了させて待つ ---
    settings_window::close();
    if let Some(indicator) = ime_indicator {
        indicator.stop();
    }
    post_quit(hook_tid);
    let _ = hook_thread.join();
    log::info!("アタイの貼り付け 終了");
}

/// 設定からキーリマップのキーと対象アプリを決めてフックへ渡し、使うキーを返す。
///
/// settings.json に使えないキーが書かれていた場合は、既定の Ctrl+B で動く。
fn configure_remap(settings: &config::Settings) -> remap_logic::Hotkey {
    let (hotkey, invalid) = remap_logic::Hotkey::from_setting(&settings.remap.hotkey);
    if let Some(why) = invalid {
        log::warn!(
            "settings.json のキー「{}」は使えないため {} で動きます: {why}",
            settings.remap.hotkey,
            hotkey.format()
        );
    }
    keyboard::configure(
        hotkey,
        remap_logic::effective_target_apps(&settings.remap.target_apps),
    );
    hotkey
}

/// フックスレッド本体。フックを設置し、メッセージループを回す。
fn hook_thread_main(id_tx: mpsc::Sender<Result<u32, String>>) {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, TranslateMessage, MSG};

    unsafe {
        if let Err(e) = keyboard::install() {
            log::error!("キーボードフック設置失敗: {e}");
            let _ = id_tx.send(Err(format!("{e}")));
            return;
        }

        // このスレッドがメッセージループを持つので、スレッド ID を通知する。
        let _ = id_tx.send(Ok(GetCurrentThreadId()));
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

/// 更新処理が実行中なら、完了するまで待ってから終了する。
///
/// [`update::install`] は実行ファイルをリネームで入れ替えている最中があるため、
/// その途中でプロセスを終了させると実行ファイルが壊れた状態になりかねない。
/// ただし通信が詰まるなどして戻ってこない場合に備え、上限（60 秒）を設けて
/// 無限に待ち続けることは避ける。
fn wait_for_update_to_finish() {
    const MAX_WAIT: Duration = Duration::from_secs(60);
    const POLL_INTERVAL: Duration = Duration::from_millis(200);

    if !update::is_running() {
        return;
    }
    log::info!("更新処理が実行中のため、完了を待ってから終了します");

    let start = Instant::now();
    while update::is_running() && start.elapsed() < MAX_WAIT {
        thread::sleep(POLL_INTERVAL);
    }

    if update::is_running() {
        log::warn!(
            "更新処理の完了を待てなかったため（{}秒経過）、終了処理を続行します",
            MAX_WAIT.as_secs()
        );
    }
}

/// 指定スレッドへ WM_QUIT を送り、メッセージループを終了させる。
fn post_quit(thread_id: u32) {
    use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
    unsafe {
        let _ = PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
    }
}
