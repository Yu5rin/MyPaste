//! 更新確認の設定と状態。
//!
//! - **設定** (`settings.json`): 利用者が編集する項目。更新の確認先 URL などを保持する。
//!   ファイルが無い場合や壊れている場合は既定値で動作する（起動を妨げない）。
//! - **状態** (`update_state.json`): アプリが書き込む項目。前回の確認時刻を保持し、
//!   「起動時に 1 日 1 回まで」の判定に使う。
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
/// 起動時チェックの最短間隔（24 時間）。
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// 設定全体。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub update: UpdateSettings,
}

/// 更新確認に関する設定。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// 起動時に更新を確認するか（`false` ならメニューからの手動確認のみ）。
    pub check_on_startup: bool,
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
    /// 設定を読み込む。ファイルが無い・壊れている場合は既定値を返す。
    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            // 初回起動時などファイルが無い場合。既定値で書き出しておくと
            // 利用者が確認先 URL を編集できる。失敗しても無視する。
            let settings = Self::default();
            settings.save_default(&path);
            return settings;
        };
        match serde_json::from_str(&text) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("settings.json の読み込みに失敗したため既定値を使います: {e}");
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

/// 起動時チェックを実行してよいか（前回から 24 時間以上経過しているか）。
pub fn should_check_now() -> bool {
    let last = load_state().last_checked_unix;
    let now = now_unix();
    // 時計が巻き戻った場合（now < last）も確認してよいものとする。
    now < last || now.saturating_sub(last) >= CHECK_INTERVAL_SECS
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
