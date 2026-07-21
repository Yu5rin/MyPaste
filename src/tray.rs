//! タスクトレイ制御。
//!
//! `tray-item` クレートを使ってタスクトレイアイコンと右クリックメニューを構築する。
//! メニュー操作はコールバックからチャネル経由でメインスレッドへ [`TrayMessage`] を送り、
//! アイコン切替や終了処理はメインスレッド側で行う（[`crate::main`] 参照）。

use std::sync::mpsc::Sender;

use tray_item::{IconSource, TrayItem};

use crate::TrayMessage;

/// ON 時のアイコン（app.rc で埋め込んだリソース名）。
pub const ICON_ON: &str = "icon_on";
/// OFF 時のアイコン（app.rc で埋め込んだリソース名）。
pub const ICON_OFF: &str = "icon_off";

/// タスクトレイアイコンとメニューを構築する。
///
/// 返した [`TrayItem`] は生存している間だけトレイに表示されるため、
/// 呼び出し側で保持し続けること。
pub fn build(tx: Sender<TrayMessage>) -> Result<TrayItem, tray_item::TIError> {
    // 起動時は ON アイコンで表示する。
    let mut tray = TrayItem::new("アタイの貼り付け", IconSource::Resource(ICON_ON))?;

    // ON/OFF 切替
    let tx_toggle = tx.clone();
    tray.add_menu_item("ON/OFF切替", move || {
        let _ = tx_toggle.send(TrayMessage::Toggle);
    })?;

    // 自動起動 ON/OFF
    let tx_startup = tx.clone();
    tray.add_menu_item("自動起動 ON/OFF", move || {
        let _ = tx_startup.send(TrayMessage::ToggleStartup);
    })?;

    // 終了
    let tx_quit = tx;
    tray.add_menu_item("終了", move || {
        let _ = tx_quit.send(TrayMessage::Quit);
    })?;

    Ok(tray)
}
