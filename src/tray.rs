//! タスクトレイ制御。
//!
//! `tray-item` クレートを使ってタスクトレイアイコンと右クリックメニューを構築する。
//! メニュー操作はコールバックからチャネル経由でメインスレッドへ [`TrayMessage`] を送り、
//! アイコン切替や終了処理はメインスレッド側で行う（[`crate::main`] 参照）。
//!
//! 有効／無効の状態はメニュー項目のラベル先頭に「✔」を付けて表す。
//! `tray-item` はチェックマーク付きメニュー（`MFS_CHECKED`）を公開していないため、
//! [`TrayItem::inner_mut`] 経由で `set_menu_item_label` を呼び、ラベルを差し替える。

use std::sync::mpsc::Sender;

use tray_item::{IconSource, TrayItem};

use crate::TrayMessage;

/// ON 時のアイコン（app.rc で埋め込んだリソース名）。
pub const ICON_ON: &str = "icon_on";
/// OFF 時のアイコン（app.rc で埋め込んだリソース名）。
pub const ICON_OFF: &str = "icon_off";

/// 有効時にラベル先頭へ付ける印。
const MARK_ON: &str = "✔ ";
/// 無効時にラベル先頭へ付ける印（「✔」と同じ幅で字下げを揃える）。
const MARK_OFF: &str = "　 ";

/// キーリマップ機能のメニュー文言。
const LABEL_REMAP: &str = "Ctrl+B で値貼り付け";
/// 自動起動のメニュー文言。
const LABEL_STARTUP: &str = "自動起動";

/// 構築したメニュー項目のハンドル。ラベル更新に使う ID を保持する。
pub struct Menu {
    tray: TrayItem,
    remap_id: u32,
    startup_id: u32,
}

impl Menu {
    /// トレイアイコンを ON/OFF に応じて切り替える。
    pub fn set_icon(&mut self, enabled: bool) -> Result<(), tray_item::TIError> {
        let icon = if enabled { ICON_ON } else { ICON_OFF };
        self.tray.set_icon(IconSource::Resource(icon))
    }

    /// キーリマップ項目のチェック状態を更新する。
    pub fn set_remap_checked(&mut self, checked: bool) -> Result<(), tray_item::TIError> {
        let label = labeled(LABEL_REMAP, checked);
        self.tray
            .inner_mut()
            .set_menu_item_label(&label, self.remap_id)
    }

    /// 自動起動項目のチェック状態を更新する。
    pub fn set_startup_checked(&mut self, checked: bool) -> Result<(), tray_item::TIError> {
        let label = labeled(LABEL_STARTUP, checked);
        self.tray
            .inner_mut()
            .set_menu_item_label(&label, self.startup_id)
    }
}

/// チェック状態に応じた表示ラベルを組み立てる。
fn labeled(text: &str, checked: bool) -> String {
    let mark = if checked { MARK_ON } else { MARK_OFF };
    format!("{mark}{text}")
}

/// タスクトレイアイコンとメニューを構築する。
///
/// 返した [`Menu`] は生存している間だけトレイに表示されるため、
/// 呼び出し側で保持し続けること。
///
/// - `enabled`: 起動時のキーリマップ有効状態
/// - `startup`: 起動時の自動起動設定状態
pub fn build(
    tx: Sender<TrayMessage>,
    enabled: bool,
    startup: bool,
) -> Result<Menu, tray_item::TIError> {
    let icon = if enabled { ICON_ON } else { ICON_OFF };
    let mut tray = TrayItem::new("アタイの貼り付け", IconSource::Resource(icon))?;

    // キーリマップの有効／無効
    let tx_toggle = tx.clone();
    let remap_id = tray
        .inner_mut()
        .add_menu_item_with_id(&labeled(LABEL_REMAP, enabled), move || {
            let _ = tx_toggle.send(TrayMessage::Toggle);
        })?;

    // 自動起動の有効／無効
    let tx_startup = tx.clone();
    let startup_id = tray
        .inner_mut()
        .add_menu_item_with_id(&labeled(LABEL_STARTUP, startup), move || {
            let _ = tx_startup.send(TrayMessage::ToggleStartup);
        })?;

    // 区切り線を挟んで「終了」を分ける。
    tray.inner_mut().add_separator()?;

    // 終了
    let tx_quit = tx;
    tray.add_menu_item("終了", move || {
        let _ = tx_quit.send(TrayMessage::Quit);
    })?;

    Ok(Menu {
        tray,
        remap_id,
        startup_id,
    })
}
