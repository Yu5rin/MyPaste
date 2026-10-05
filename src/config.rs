//! 更新確認の設定と状態。
//!
//! - **設定** (`settings.json`): 利用者が編集する項目。更新の確認先 URL などを保持する。
//!   ファイルが無い場合や壊れている場合は既定値で動作する（起動を妨げない）。
//! - **状態** (`update_state.json`): アプリが書き込む項目。前回の確認時刻を保持する。
//!   既定では起動のたびに確認するため使わないが、`check_interval_hours` に
//!   0 以外を設定したときの間隔判定に使う。
//!
//! 確認先の URL をコードに直書きせず設定ファイルに持たせているのは、
//! 将来リポジトリを移しても設定を変えるだけで済むようにするためと、
//! **どこへ通信するのかを利用者が確認できるようにする**ためである。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// 設定ファイル名。
const SETTINGS_FILE: &str = "settings.json";
/// 状態ファイル名。
const STATE_FILE: &str = "update_state.json";

/// 設定全体。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub update: UpdateSettings,
    /// IME 入力モードの画面中央表示（[`crate::ime_indicator`]）。
    pub ime_indicator: ImeIndicatorSettings,
}

/// IME 入力モードの画面中央表示に関する設定。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImeIndicatorSettings {
    /// 表示するか（トレイメニュー「入力モードを画面中央に表示」からも切り替えられる）。
    pub enabled: bool,
    /// 表示を保持する時間（ミリ秒）。このあと 0.25 秒でフェードアウトする。
    pub hold_ms: u64,
    /// 表示する四角の一辺（96 DPI 基準のピクセル。実際の DPI に合わせて拡大する）。
    pub size: u32,
}

impl Default for ImeIndicatorSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            hold_ms: 400,
            size: 120,
        }
    }
}

/// 更新確認に関する設定。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// 起動時に更新を確認するか（`false` ならメニューからの手動確認のみ）。
    pub check_on_startup: bool,
    /// 起動時チェックの最短間隔（時間）。
    ///
    /// `0` なら**起動のたびに**確認する（既定）。`24` にすると 1 日 1 回までになる。
    pub check_interval_hours: u64,
    /// GitHub Releases API のエンドポイント。
    pub api_url: String,
    /// 更新が見つかったときにブラウザで開くページ。
    pub releases_page: String,
    /// リリースに添付された実行ファイルの名前。
    pub asset_name: String,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            check_on_startup: true,
            // 既定は 0 = 起動のたびに確認する。
            check_interval_hours: 0,
            api_url: "https://api.github.com/repos/Yu5rin/MyPaste/releases/latest".to_string(),
            releases_page: "https://github.com/Yu5rin/MyPaste/releases/latest".to_string(),
            asset_name: "Atai-paste.exe".to_string(),
        }
    }
}

/// 前回の確認時刻を保持する状態。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct UpdateState {
    /// 前回チェックした UNIX 時刻（秒）。未確認なら 0。
    last_checked_unix: u64,
}

impl Settings {
    /// 設定を読み込む。ファイルが無い場合は既定値を書き出して返す。
    /// ファイルはあるが読み込み・解析に失敗した場合は、利用者の編集内容を
    /// 破棄しないよう、既定値の**上書き保存はせず**その場限りの既定値を返す。
    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 初回起動時などファイルが無い場合。既定値で書き出しておくと
                // 利用者が確認先 URL を編集できる。失敗しても無視する。
                let settings = Self::default();
                settings.save_default(&path);
                return settings;
            }
            Err(e) => {
                // ファイルはあるが読めない（例: UTF-16 で保存されている等）。
                // ここで既定値を書き戻すと利用者の編集内容を消してしまうため、
                // 上書き保存はせず、今回だけ既定値で動作する。
                log::warn!("settings.json を読み込めなかったため既定値で動作します: {e}");
                return Self::default();
            }
        };

        // メモ帳などで保存すると UTF-8 の BOM (U+FEFF) が先頭に付くことがある。
        // 付いたままだと serde_json が解析に失敗するため取り除く。
        let text = strip_bom(&text);

        match serde_json::from_str(text) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("settings.json の解析に失敗したため既定値を使います: {e}");
                Self::default()
            }
        }
    }

    /// 既定の設定をファイルへ書き出す（初回起動時のみ。失敗は無視）。
    fn save_default(&self, path: &Path) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }
}

/// IME 入力モード表示の ON/OFF だけを `settings.json` に書き込む。
///
/// ファイル全体を [`Settings`] で書き直すと、利用者が書いた未知の項目や
/// 書式が失われるため、JSON として読んでこの 1 項目だけを差し替える。
/// ファイルが読めない・JSON として解釈できない場合は、利用者の編集内容を
/// 壊さないよう**書き込まずに** `Err` を返す（今回の切り替えはメモリ上だけ有効）。
pub fn save_ime_indicator_enabled(enabled: bool) -> Result<(), String> {
    let path = settings_path().ok_or("設定ファイルの場所を決められません")?;
    let mut root: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(strip_bom(&text))
            .map_err(|e| format!("settings.json を解釈できないため保存しませんでした: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            serde_json::to_value(Settings::default()).map_err(|e| e.to_string())?
        }
        Err(e) => return Err(format!("settings.json を読めないため保存しませんでした: {e}")),
    };
    let obj = root
        .as_object_mut()
        .ok_or("settings.json の形が想定と違うため保存しませんでした")?;
    let section = obj
        .entry("ime_indicator")
        .or_insert_with(|| serde_json::json!({}));
    let section = section
        .as_object_mut()
        .ok_or("settings.json の ime_indicator の形が想定と違うため保存しませんでした")?;
    section.insert("enabled".into(), serde_json::Value::Bool(enabled));
    let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("settings.json に書き込めませんでした: {e}"))
}

/// 文字列の先頭に UTF-8 の BOM (`\u{feff}`) が付いていれば取り除く。
fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// 起動時チェックを実行してよいか。
///
/// `interval_hours` が `0` なら常に `true`（起動のたびに確認する）。
/// それ以外は、前回の確認から指定時間以上経過している場合だけ `true` を返す。
pub fn should_check_now(interval_hours: u64) -> bool {
    is_due(now_unix(), load_state().last_checked_unix, interval_hours)
}

/// 確認すべきかの判定そのもの（時刻を引数に取る純粋な関数。テストのため分離）。
fn is_due(now: u64, last: u64, interval_hours: u64) -> bool {
    if interval_hours == 0 {
        // 毎回確認する。
        return true;
    }
    // 時計が巻き戻った場合（now < last）も確認してよいものとする。
    now < last || now.saturating_sub(last) >= interval_hours.saturating_mul(3600)
}

/// 「今チェックした」ことを記録する（失敗は無視。起動を妨げない）。
pub fn mark_checked() {
    let state = UpdateState {
        last_checked_unix: now_unix(),
    };
    if let (Some(path), Ok(text)) = (state_path(), serde_json::to_string_pretty(&state)) {
        let _ = std::fs::write(path, text);
    }
}

/// 状態ファイルを読み込む（無ければ既定値）。
fn load_state() -> UpdateState {
    state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// 現在の UNIX 時刻（秒）。
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 設定ファイルのパス。
fn settings_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join(SETTINGS_FILE))
}

/// 状態ファイルのパス。
fn state_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join(STATE_FILE))
}

/// 設定・状態を置くディレクトリ。
///
/// ポータブル運用を優先して実行ファイルと同じフォルダを使う。ただし
/// `Program Files` 配下など書き込めない場所に置かれている場合は、
/// `%LOCALAPPDATA%\Atai-paste` にフォールバックする。
fn data_dir() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    if is_writable(&exe_dir) {
        return Some(exe_dir);
    }
    let local = std::env::var_os("LOCALAPPDATA")?;
    let dir = PathBuf::from(local).join("Atai-paste");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 指定ディレクトリに書き込めるかを、一時ファイルを作って確かめる。
pub fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(".atai-paste-write-test");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_due, strip_bom};

    const HOUR: u64 = 3600;

    #[test]
    fn strip_bom_removes_leading_marker() {
        let with_bom = "\u{feff}{\"update\":{}}";
        assert_eq!(strip_bom(with_bom), "{\"update\":{}}");
    }

    #[test]
    fn strip_bom_leaves_normal_text_untouched() {
        let text = "{\"update\":{}}";
        assert_eq!(strip_bom(text), text);
    }

    #[test]
    fn strip_bom_only_strips_leading_occurrence() {
        // 途中に現れる U+FEFF はそのまま残す（BOM は先頭にのみ意味を持つ）。
        let text = "a\u{feff}b";
        assert_eq!(strip_bom(text), text);
    }

    #[test]
    fn interval_zero_always_checks() {
        // 既定の 0 は「起動のたびに確認」。直前に確認していても必ず true。
        assert!(is_due(1_000_000, 1_000_000, 0));
        assert!(is_due(1_000_000, 999_999, 0));
        assert!(is_due(0, 0, 0));
    }

    #[test]
    fn interval_respects_elapsed_time() {
        let last = 1_000_000;
        // 24 時間ちょうどで確認する。1 秒でも足りなければ待つ。
        assert!(!is_due(last + 24 * HOUR - 1, last, 24));
        assert!(is_due(last + 24 * HOUR, last, 24));
        assert!(is_due(last + 48 * HOUR, last, 24));
    }

    #[test]
    fn clock_going_backwards_still_checks() {
        // 時計が巻き戻っても確認できなくならないこと。
        assert!(is_due(500, 1_000_000, 24));
    }

    #[test]
    fn never_checked_before() {
        // 未確認（last = 0）なら確認する。
        assert!(is_due(1_000_000, 0, 24));
    }

    #[test]
    fn huge_interval_does_not_overflow() {
        // 極端な設定値でも panic しないこと（saturating 演算）。
        assert!(!is_due(1_000_000, 999_999, u64::MAX));
    }
}
