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

use crate::remap_logic::Hotkey;
use crate::TrayMessage;

/// ON 時のアイコン（app.rc で埋め込んだリソース名）。
pub const ICON_ON: &str = "icon_on";
/// OFF 時のアイコン（app.rc で埋め込んだリソース名）。
pub const ICON_OFF: &str = "icon_off";

/// 有効時にラベル先頭へ付ける印。
const MARK_ON: &str = "✔ ";
/// 無効時にラベル先頭へ付ける印（「✔」と同じ幅で字下げを揃える）。
const MARK_OFF: &str = "　 ";

/// キーリマップ機能のメニュー文言（先頭に設定したキーの組み合わせが付く）。
fn remap_label(hotkey: &Hotkey) -> String {
    format!("{} で値貼り付け", hotkey.format())
}
/// 自動起動のメニュー文言。
const LABEL_STARTUP: &str = "自動起動";
/// キー割り当ての有効／無効のメニュー文言。
const LABEL_HOTKEYS: &str = "キー割り当てを使う";
/// 入力モード表示のメニュー文言（表示位置は画面中央とは限らないため「表示」とだけ書く）。
const LABEL_IME_INDICATOR: &str = "入力モードを表示";
/// 設定画面のメニュー文言。
const LABEL_SETTINGS: &str = "設定...";
/// キー割り当て画面のメニュー文言。
const LABEL_HOTKEY_WINDOW: &str = "キー割り当て...";
/// キー割り当ての一覧を表示する項目の文言。
const LABEL_LIST: &str = "キー割り当ての一覧";
/// 更新確認のメニュー文言。
const LABEL_CHECK_UPDATE: &str = "更新を確認";

/// アプリ名（ツールチップの先頭に使う）。
const APP_NAME: &str = "アタイの貼り付け";

/// アプリ名とバージョンの表示（例: `アタイの貼り付け v1.2.2`）。
///
/// ツールチップとメニュー先頭の見出しの両方に使う。
///
/// ツールチップだけではカーソルを合わせないと見えず、Windows 11 では
/// トレイアイコン自体がオーバーフロー（「^」の中）に隠れるため、実質的に
/// 確認できない。メニューを開けば必ず目に入るよう、見出しとしても表示する。
fn app_title() -> String {
    format!("{APP_NAME} v{}", env!("CARGO_PKG_VERSION"))
}

/// 構築したメニュー項目のハンドル。ラベル更新に使う ID を保持する。
pub struct Menu {
    tray: TrayItem,
    remap_id: u32,
    /// キーリマップ項目の文言（チェックの印を除く）。
    remap_text: String,
    hotkeys_id: u32,
    startup_id: u32,
    ime_indicator_id: u32,
}

impl Menu {
    /// トレイアイコンを ON/OFF に応じて切り替える。
    pub fn set_icon(&mut self, enabled: bool) -> Result<(), tray_item::TIError> {
        let icon = if enabled { ICON_ON } else { ICON_OFF };
        self.tray.set_icon(IconSource::Resource(icon))
    }

    /// キーリマップ項目のチェック状態を更新する。
    pub fn set_remap_checked(&mut self, checked: bool) -> Result<(), tray_item::TIError> {
        let label = labeled(&self.remap_text, checked);
        self.tray
            .inner_mut()
            .set_menu_item_label(&label, self.remap_id)
    }

    /// キー割り当て項目のチェック状態を更新する。
    pub fn set_hotkeys_checked(&mut self, checked: bool) -> Result<(), tray_item::TIError> {
        let label = labeled(LABEL_HOTKEYS, checked);
        self.tray
            .inner_mut()
            .set_menu_item_label(&label, self.hotkeys_id)
    }

    /// キーリマップ項目の文言を、設定したキーの組み合わせに合わせて更新する。
    pub fn set_remap_hotkey(
        &mut self,
        hotkey: &Hotkey,
        checked: bool,
    ) -> Result<(), tray_item::TIError> {
        self.remap_text = remap_label(hotkey);
        self.set_remap_checked(checked)
    }

    /// 自動起動項目のチェック状態を更新する。
    pub fn set_startup_checked(&mut self, checked: bool) -> Result<(), tray_item::TIError> {
        let label = labeled(LABEL_STARTUP, checked);
        self.tray
            .inner_mut()
            .set_menu_item_label(&label, self.startup_id)
    }

    /// 入力モード表示項目のチェック状態を更新する。
    pub fn set_ime_indicator_checked(&mut self, checked: bool) -> Result<(), tray_item::TIError> {
        let label = labeled(LABEL_IME_INDICATOR, checked);
        self.tray
            .inner_mut()
            .set_menu_item_label(&label, self.ime_indicator_id)
    }

    /// ダウンロードの進捗をツールチップに表示する。
    pub fn set_progress(&mut self, percent: u64) -> Result<(), tray_item::TIError> {
        self.tray
            .inner_mut()
            .set_tooltip(&format!("{} — 更新を取得中 {percent}%", app_title()))
    }

    /// ツールチップを通常の表示に戻す。
    pub fn clear_progress(&mut self) -> Result<(), tray_item::TIError> {
        self.tray.inner_mut().set_tooltip(&app_title())
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
/// - `hotkey`: 値貼り付けを起動するキーの組み合わせ（メニューの文言に使う）
/// - `enabled`: 起動時のキーリマップ有効状態
/// - `hotkeys`: 起動時のキー割り当て有効状態
/// - `startup`: 起動時の自動起動設定状態
/// - `ime_indicator`: 起動時の入力モード表示の有効状態
pub fn build(
    tx: Sender<TrayMessage>,
    hotkey: &Hotkey,
    enabled: bool,
    hotkeys: bool,
    startup: bool,
    ime_indicator: bool,
) -> Result<Menu, tray_item::TIError> {
    let icon = if enabled { ICON_ON } else { ICON_OFF };
    let mut tray = TrayItem::new(&app_title(), IconSource::Resource(icon))?;

    // 先頭にアプリ名とバージョンを見出しとして置く。add_label は選択できない
    // 項目（MFS_DISABLED）になるので、誤って押される心配がない。
    tray.add_label(&app_title())?;
    tray.inner_mut().add_separator()?;

    // キーリマップの有効／無効
    let tx_toggle = tx.clone();
    let remap_text = remap_label(hotkey);
    let remap_id = tray
        .inner_mut()
        .add_menu_item_with_id(&labeled(&remap_text, enabled), move || {
            let _ = tx_toggle.send(TrayMessage::Toggle);
        })?;

    // キー割り当ての有効／無効
    let tx_hotkeys = tx.clone();
    let hotkeys_id = tray
        .inner_mut()
        .add_menu_item_with_id(&labeled(LABEL_HOTKEYS, hotkeys), move || {
            let _ = tx_hotkeys.send(TrayMessage::ToggleHotkeys);
        })?;

    // 自動起動の有効／無効
    let tx_startup = tx.clone();
    let startup_id = tray
        .inner_mut()
        .add_menu_item_with_id(&labeled(LABEL_STARTUP, startup), move || {
            let _ = tx_startup.send(TrayMessage::ToggleStartup);
        })?;

    // 入力モード表示の有効／無効
    let tx_ime = tx.clone();
    let ime_indicator_id = tray.inner_mut().add_menu_item_with_id(
        &labeled(LABEL_IME_INDICATOR, ime_indicator),
        move || {
            let _ = tx_ime.send(TrayMessage::ToggleImeIndicator);
        },
    )?;

    // 設定項目と操作項目を区切る。
    tray.inner_mut().add_separator()?;

    // 設定画面
    let tx_settings = tx.clone();
    tray.add_menu_item(LABEL_SETTINGS, move || {
        let _ = tx_settings.send(TrayMessage::OpenSettings);
    })?;

    // キー割り当て画面
    let tx_hotkey_window = tx.clone();
    tray.add_menu_item(LABEL_HOTKEY_WINDOW, move || {
        let _ = tx_hotkey_window.send(TrayMessage::OpenHotkeys);
    })?;

    // キー割り当ての一覧
    let tx_list = tx.clone();
    tray.add_menu_item(LABEL_LIST, move || {
        let _ = tx_list.send(TrayMessage::ShowList);
    })?;

    // 更新の確認（押したときだけ通信する）
    let tx_update = tx.clone();
    tray.add_menu_item(LABEL_CHECK_UPDATE, move || {
        let _ = tx_update.send(TrayMessage::CheckUpdate);
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
        remap_text,
        hotkeys_id,
        startup_id,
        ime_indicator_id,
    })
}
