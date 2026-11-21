//! 設定と更新確認の状態。
//!
//! - **設定** (`settings.json`): 利用者が編集する項目。キーリマップのキーや対象アプリ、
//!   入力モード表示、更新の確認先 URL などを保持する。設定画面（[`crate::settings_window`]）
//!   からも書き換えられる。
//!   ファイルが無い場合や壊れている場合は既定値で動作する（起動を妨げない）。
//! - **状態** (`update_state.json`): アプリが書き込む項目。前回の確認時刻を保持する。
//!   既定では起動のたびに確認するため使わないが、`check_interval_hours` に
//!   0 以外を設定したときの間隔判定に使う。
//!
//! 確認先の URL をコードに直書きせず設定ファイルに持たせているのは、
//! 将来リポジトリを移しても設定を変えるだけで済むようにするためと、
//! **どこへ通信するのかを利用者が確認できるようにする**ためである。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// settings.json を書き換えている間持つ鍵（[`save_patch`] と [`replace_settings`]）。
static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

/// 設定ファイル名。
const SETTINGS_FILE: &str = "settings.json";
/// 動作の記録のファイル名。
const LOG_FILE: &str = "log.txt";
/// 状態ファイル名。
const STATE_FILE: &str = "update_state.json";
/// クリップボードの履歴を残しておくファイル名（暗号化して保存する。[`crate::clip_store`]）。
const CLIP_HISTORY_FILE: &str = "clip_history.dat";

/// 設定全体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// キーリマップ（[`crate::keyboard`]）。
    pub remap: RemapSettings,
    pub update: UpdateSettings,
    /// IME 入力モードの画面中央表示（[`crate::ime_indicator`]）。
    pub ime_indicator: ImeIndicatorSettings,
    /// トラブル調査用の動作の記録（[`crate::logging`]）。
    pub log: LogSettings,
    /// キー割り当て（[`crate::hotkey_rules`]）。上から順に調べる。
    pub hotkeys: Vec<HotkeyRuleSetting>,
    /// キー割り当てを使うか（トレイメニューの「キー割り当て」）。
    pub hotkeys_enabled: bool,
    /// クリップボードの履歴（[`crate::clip_history`]）。
    pub clipboard_history: ClipboardHistorySettings,
    /// 定型文（[`crate::snippets`]）。上から順に一覧に並べる。
    pub snippets: Vec<Snippet>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            remap: RemapSettings::default(),
            update: UpdateSettings::default(),
            ime_indicator: ImeIndicatorSettings::default(),
            log: LogSettings::default(),
            hotkeys: Vec::new(),
            hotkeys_enabled: true,
            clipboard_history: ClipboardHistorySettings::default(),
            snippets: Vec::new(),
        }
    }
}

/// 定型文 1 つ分。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snippet {
    /// 一覧に出す名前（空なら本文の最初の行を出す）。
    pub name: String,
    /// 貼り付ける本文（改行は \n）。
    pub text: String,
    /// まとめておくためのグループ（空ならグループ無し）。
    pub group: String,
}

/// クリップボードの履歴に関する設定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardHistorySettings {
    /// コピーした文字を記録し、一覧から貼り付けられるようにするか。
    pub enabled: bool,
    /// 一覧を出す操作（`"hotkey"` / `"double_ctrl"` / `"double_shift"` / `"double_alt"`）。
    pub trigger: String,
    /// `trigger` が `"hotkey"` のときのキーの組み合わせ。
    pub hotkey: String,
    /// 2 回押しとみなす間隔（ミリ秒）。
    pub double_tap_ms: u32,
    /// 覚えておく件数（10〜100）。
    pub max_items: usize,
    /// アプリを終了しても履歴を残すか（暗号化してファイルに保存する）。
    pub keep_after_exit: bool,
    /// 一覧を出す位置（`"caret"` / `"mouse"`）。
    pub position: String,
    /// 一覧の画面の幅（96 DPI 基準のピクセル。200〜600）。
    pub width: u32,
    /// 一覧の画面の不透明度（%。30〜100）。一覧の上で Shift+ホイールでも変えられる。
    pub opacity: u32,
    /// 一覧の 1 ページに並べる件数（10〜40）。
    pub page_size: usize,
    /// 一覧の配色（`"system"` / `"light"` / `"dark"` / `"blue"` / `"green"`）。
    pub theme: String,
    /// 一覧から貼り付けたものを、履歴のいちばん上に移すか。
    pub move_to_top: bool,
    /// 記録しないアプリ（プロセス名）。そのアプリでコピーしたものは履歴に入れない。
    pub exclude_apps: Vec<String>,
    /// 画像（スクリーンショットなど）も記録するか。
    pub record_images: bool,
    /// 一覧から貼り付けても、一覧を出したままにするか。
    pub keep_open: bool,
}

impl Default for ClipboardHistorySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            trigger: "hotkey".to_string(),
            hotkey: "Ctrl+Alt+H".to_string(),
            double_tap_ms: crate::clip_history::DEFAULT_DOUBLE_TAP_MS,
            max_items: crate::clip_history::DEFAULT_ITEMS,
            keep_after_exit: true,
            position: "caret".to_string(),
            width: crate::clip_history::DEFAULT_WIDTH,
            opacity: crate::clip_history::DEFAULT_OPACITY,
            page_size: crate::clip_history::DEFAULT_PAGE_SIZE,
            theme: "system".to_string(),
            move_to_top: true,
            exclude_apps: Vec::new(),
            record_images: true,
            keep_open: false,
        }
    }
}

/// キー割り当て 1 つ分の設定（`settings.json` の書き方そのまま）。
///
/// 中身の解釈と検証は [`crate::hotkey_rules::Rule::from_setting`] で行う。読めない値でも
/// 設定ファイル全体を無効にしないよう、ここでは文字列のまま持つ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyRuleSetting {
    /// 使うか（false なら一時的に止めておける）。
    pub enabled: bool,
    /// 起動するキーの組み合わせ（例 `"Ctrl+Alt+V"`）。
    pub hotkey: String,
    /// 動作の種類（`"send_keys"` / `"type_text"` / `"run"` / `"paste_plain"` /
    /// `"toggle_topmost"` / `"block"`）。
    pub action: String,
    /// 動作の内容（送るキー・入力する文字・開くプログラムなど）。
    pub value: String,
    /// プログラムを開くときの引数。
    pub args: String,
    /// 対象アプリ（プロセス名）。空ならすべてのアプリ。
    pub apps: Vec<String>,
    /// 「文字を入力する」の入れ方。`"paste"`（クリップボードを使って一度に貼り付ける。既定）か
    /// `"keys"`（1 文字ずつキー入力として送る）。ほかの動作では使わない。
    pub input: String,
}

impl Default for HotkeyRuleSetting {
    fn default() -> Self {
        Self {
            enabled: true,
            hotkey: String::new(),
            action: String::new(),
            value: String::new(),
            args: String::new(),
            apps: Vec::new(),
            input: "paste".to_string(),
        }
    }
}

/// トラブル調査用の動作の記録に関する設定。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LogSettings {
    /// `log.txt` に動作を記録するか（既定は記録しない）。
    pub enabled: bool,
}

/// キーリマップに関する設定。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RemapSettings {
    /// 値貼り付けを使うか（トレイメニューの「値貼り付け」）。
    pub enabled: bool,
    /// 値貼り付けを起動するキーの組み合わせ（例 `"Ctrl+B"`）。
    /// 書き方は [`crate::remap_logic::Hotkey::parse`] を参照。
    pub hotkey: String,
    /// リマップする対象アプリのプロセス名（例 `"EXCEL.EXE"`）。
    pub target_apps: Vec<String>,
}

impl Default for RemapSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            hotkey: crate::remap_logic::DEFAULT_HOTKEY.to_string(),
            target_apps: vec![crate::remap_logic::DEFAULT_TARGET_APP.to_string()],
        }
    }
}

/// IME 入力モードの画面中央表示に関する設定。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImeIndicatorSettings {
    /// 表示するか（トレイメニュー「入力モードを画面中央に表示」からも切り替えられる）。
    pub enabled: bool,
    /// 表示を保持する時間（ミリ秒）。設定画面では秒で入力する。
    pub hold_ms: u64,
    /// 続いてフェードアウトにかける時間（ミリ秒）。0 はすぐ消す。設定画面では秒で入力する。
    pub fade_ms: u64,
    /// 表示する四角の一辺（96 DPI 基準のピクセル。実際の DPI に合わせて拡大する）。
    pub size: u32,
    /// 表示する位置（`"center"` / `"mouse"` / `"caret"`）。知らない値は画面中央。
    pub position: String,
    /// 色（`"dark"` / `"light"`）。知らない値は濃い色。
    pub theme: String,
    /// 不透明度（%。30〜100）。
    pub opacity: u32,
    /// 全画面のアプリ（ゲーム・動画・プレゼンテーションなど）の間は表示しないか。
    pub hide_in_fullscreen: bool,
    /// スクリーンショットや画面共有に、入力モードの表示を写さないか。
    pub hide_from_capture: bool,
    /// 切り替えたときに、画面に大きく表示するか。
    pub flash: bool,
    /// いまの入力モードを、小さな印で出すか。
    pub badge_enabled: bool,
    /// 小さな印を出す場所（`"caret"` / `"mouse"`。`"off"` は出さない）。
    pub badge: String,
    /// 小さな印の一辺（96 DPI 基準のピクセル。12〜48）。
    pub badge_size: u32,
    /// 小さな印を出すとき（`"auto"`＝おまかせ / `"typing"`＝打ち始めたときだけ / `"always"`＝出し続ける）。
    pub badge_when: String,
    /// 「打ち始めたときだけ」のときに、出しておく時間（ミリ秒。1000〜10000）。
    pub badge_hold_ms: u64,
    /// 小さな印の不透明度（%。20〜100）。
    pub badge_opacity: u32,
    /// 「おまかせ」のとき、マウスカーソルが文字入力の形（I の形）になったら小さな印を出すか。
    #[serde(rename = "badge_on_text_cursor")]
    pub badge_on_ibeam: bool,
    /// 「おまかせ」のとき、入力位置の横では出し続けるか（打っている間は消す）。
    pub badge_keep_at_caret: bool,
    /// アプリに切り替えたときに、入力モードを自動で切り替えるか（入力モードの表示とは別に使える）。
    pub app_modes_enabled: bool,
    /// アプリごとの入力モード。空なら何もしない。読めない件は飛ばす。
    #[serde(deserialize_with = "lenient_app_modes")]
    pub app_modes: Vec<AppImeModeSetting>,
}

/// アプリごとの入力モード 1 件（`ime_indicator.app_modes` の要素）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppImeModeSetting {
    /// プロセス名（例 `EXCEL.EXE`）。
    pub app: String,
    /// `"on"`＝日本語（ひらがな） / `"off"`＝英字（IME オフ）。知らない値の件は使わない。
    pub mode: String,
}

/// `app_modes` を読む。1 件が読めなくても、入力モードの表示の設定全体を既定に戻さないよう、
/// 読めない件だけを飛ばす（配列でなければ空にする）。
fn lenient_app_modes<'de, D>(deserializer: D) -> Result<Vec<AppImeModeSetting>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|item| serde_json::from_value(item).ok())
            .collect(),
        _ => Vec::new(),
    })
}

impl Default for ImeIndicatorSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            hold_ms: 400,
            fade_ms: crate::ime_logic::DEFAULT_FADE_MS,
            size: 120,
            position: "center".to_string(),
            theme: "dark".to_string(),
            opacity: crate::ime_logic::DEFAULT_OPACITY,
            hide_in_fullscreen: true,
            hide_from_capture: true,
            flash: true,
            badge_enabled: false,
            badge: "caret".to_string(),
            badge_size: crate::ime_logic::DEFAULT_BADGE_SIZE,
            badge_when: "auto".to_string(),
            badge_hold_ms: crate::ime_logic::DEFAULT_BADGE_HOLD_MS,
            badge_opacity: crate::ime_logic::DEFAULT_BADGE_OPACITY,
            badge_on_ibeam: false,
            badge_keep_at_caret: true,
            app_modes_enabled: true,
            app_modes: Vec::new(),
        }
    }
}

/// 動いている間に更新を確認する間隔の既定（時間）。
pub const DEFAULT_CHECK_EVERY_HOURS: u64 = 6;

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
    /// 動いている間に確認する間隔（時間）。`0` なら動いている間は確認しない（既定 6）。
    pub check_every_hours: u64,
    /// 新しい版が見つかったときの動き。`"ask"`（尋ねてから更新・既定）/ `"auto"`（尋ねずに更新）/
    /// `"notify"`（知らせるだけ）。
    pub mode: String,
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
            check_every_hours: DEFAULT_CHECK_EVERY_HOURS,
            mode: "ask".to_string(),
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
                // この時点では動作の記録がまだ始まっていないので、起動したあとで利用者に知らせる。
                log::warn!("settings.json を読み込めなかったため既定値で動作します: {e}");
                report_load_problem(&path, &format!("{e}"), &[read_error_reason(&e)], true);
                return Self::default();
            }
        };

        // メモ帳などで保存すると UTF-8 の BOM (U+FEFF) が先頭に付くことがある。
        // 付いたままだと serde_json が解析に失敗するため取り除く。
        let text = strip_bom(&text);

        match serde_json::from_str(text) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("settings.json をそのままでは読めません: {e}");
                // 1 か所の書き間違いで全部を既定値にすると、キー割り当てや定型文が空に見え、
                // 次に保存したときに本当に消えてしまう。読める部分は読む。
                let (settings, problems) = match serde_json::from_str::<serde_json::Value>(text) {
                    Ok(value) => Self::from_value_lenient(&value),
                    Err(_) => (
                        Self::default(),
                        vec!["ファイル全体を JSON として読めません（かっこやカンマの書き間違いなど）".to_string()],
                    ),
                };
                report_load_problem(&path, &format!("{e}"), &problems, false);
                settings
            }
        }
    }

    /// 型の合わない所があっても、読める部分だけを読む。読めなかった所は既定値にして、
    /// どこが読めなかったかを返す。
    pub fn from_value_lenient(value: &serde_json::Value) -> (Self, Vec<String>) {
        fn section<T: serde::de::DeserializeOwned>(
            value: &serde_json::Value,
            key: &str,
            target: &mut T,
            problems: &mut Vec<String>,
        ) {
            if let Some(part) = value.get(key) {
                match serde_json::from_value(part.clone()) {
                    Ok(parsed) => *target = parsed,
                    Err(e) => problems.push(format!("{key}: {e}")),
                }
            }
        }
        /// 配列は 1 件ずつ読み、読めない件だけを飛ばす。
        fn list<T: serde::de::DeserializeOwned>(
            value: &serde_json::Value,
            key: &str,
            target: &mut Vec<T>,
            problems: &mut Vec<String>,
        ) {
            match value.get(key) {
                None => {}
                Some(serde_json::Value::Array(items)) => {
                    for (i, item) in items.iter().enumerate() {
                        match serde_json::from_value(item.clone()) {
                            Ok(parsed) => target.push(parsed),
                            Err(e) => problems.push(format!("{key} の {} 件目: {e}", i + 1)),
                        }
                    }
                }
                Some(_) => problems.push(format!("{key}: 配列ではありません")),
            }
        }
        let mut settings = Self::default();
        let mut problems = Vec::new();
        if !value.is_object() {
            problems.push("いちばん外側が { } ではありません".to_string());
            return (settings, problems);
        }
        section(value, "remap", &mut settings.remap, &mut problems);
        section(value, "update", &mut settings.update, &mut problems);
        section(value, "ime_indicator", &mut settings.ime_indicator, &mut problems);
        section(value, "log", &mut settings.log, &mut problems);
        section(value, "hotkeys_enabled", &mut settings.hotkeys_enabled, &mut problems);
        section(value, "clipboard_history", &mut settings.clipboard_history, &mut problems);
        list(value, "hotkeys", &mut settings.hotkeys, &mut problems);
        list(value, "snippets", &mut settings.snippets, &mut problems);
        // 同じグループの定型文が続くように並べる（手で書き換えたファイルでも一覧がまとまるよう）。
        crate::snippets::sort_by_group(&mut settings.snippets, None);
        (settings, problems)
    }

    /// 既定の設定をファイルへ書き出す（初回起動時のみ。失敗は無視）。
    fn save_default(&self, path: &Path) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }
}

/// settings.json を読めなかった所の説明（起動時に 1 度だけ知らせる）。
static LOAD_PROBLEM: Mutex<Option<String>> = Mutex::new(None);

/// settings.json を読めなかったとき（ファイルはある）の、利用者向けの理由。
fn read_error_reason(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::InvalidData {
        // read_to_string は、UTF-8 として読めないと InvalidData を返す。
        "文字コードが UTF-8 ではありません（UTF-16 や Shift_JIS で保存されている可能性があります）"
            .to_string()
    } else {
        format!("ファイルを読めません: {e}")
    }
}

/// settings.json に読めない所があったことを覚えておく。最初の 1 回は、直す前の中身を
/// `settings.json.bak` に写しておく（このあとの保存で、読めなかった所が既定値に置き換わるため）。
/// `unreadable` は、ファイルそのものを読めなかった（文字コードが違うなど）とき。このときは
/// 全体を既定値で動かし、設定の保存もできない（[`save_patch`] が書き込まない）。
fn report_load_problem(path: &Path, error: &str, problems: &[String], unreadable: bool) {
    // 知らせるのも写しを作るのも、起動してから最初の 1 回だけ（あとの読み込みで写しを
    // 作り直すと、保存で変わったあとの中身で、元の中身の写しを上書きしてしまう）。
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if REPORTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut slot = LOAD_PROBLEM.lock().unwrap_or_else(|p| p.into_inner());
    let mut backup = path.as_os_str().to_owned();
    backup.push(".bak");
    let backup = PathBuf::from(backup);
    let copied = std::fs::copy(path, &backup).is_ok();
    let mut message = if unreadable {
        format!(
            "設定ファイル（settings.json）を読めなかったため、すべて既定値で動いています。\n\n{}\n\n理由:\n",
            path.display()
        )
    } else {
        format!(
            "設定ファイル（settings.json）に読めない所があったため、その部分は既定値で動いています。\n\n{}\n\n読めなかった所:\n",
            path.display()
        )
    };
    for problem in problems.iter().take(8) {
        message.push_str(&format!("・{problem}\n"));
    }
    if problems.is_empty() {
        message.push_str(&format!("・{error}\n"));
    }
    if copied {
        message.push_str(&format!(
            "\n元の中身は、次のファイルに写してあります。\n{}\n",
            backup.display()
        ));
    }
    if unreadable {
        message.push_str(
            "\nこのままでは設定を保存できません。メモ帳などで文字コードを UTF-8 にして保存し直してから、\
             アプリを起動し直してください。",
        );
    } else {
        message.push_str("\nこのあと設定を保存すると、読めなかった所は既定値に置き換わります。");
    }
    *slot = Some(message);
}

/// settings.json を読めなかった所の説明を取り出す（あれば、起動時に利用者へ知らせる）。
pub fn take_load_problem() -> Option<String> {
    LOAD_PROBLEM.lock().unwrap_or_else(|p| p.into_inner()).take()
}

/// IME 入力モード表示の ON/OFF だけを `settings.json` に書き込む（トレイメニューから使う）。
/// 値貼り付けの ON/OFF だけを書き込む（トレイメニューで切り替えたとき）。
pub fn save_remap_enabled(enabled: bool) -> Result<(), String> {
    save_patch(&serde_json::json!({ "remap": { "enabled": enabled } }))
}

/// キー割り当ての ON/OFF だけを書き込む（トレイメニューで切り替えたとき）。
pub fn save_hotkeys_enabled(enabled: bool) -> Result<(), String> {
    save_patch(&serde_json::json!({ "hotkeys_enabled": enabled }))
}

/// クリップボードの履歴の ON/OFF だけを書き込む（トレイメニューで切り替えたとき）。
pub fn save_clipboard_history_enabled(enabled: bool) -> Result<(), String> {
    save_patch(&serde_json::json!({ "clipboard_history": { "enabled": enabled } }))
}

pub fn save_ime_indicator_enabled(enabled: bool) -> Result<(), String> {
    save_patch(&serde_json::json!({ "ime_indicator": { "enabled": enabled } }))
}

/// 設定画面で編集する項目だけを `settings.json` に書き込む。
///
/// 更新の確認先 URL など、設定画面に出していない項目には触れない。
pub fn save_from_settings_window(settings: &Settings) -> Result<(), String> {
    save_patch(&serde_json::json!({
        "remap": {
            "hotkey": settings.remap.hotkey,
            "target_apps": settings.remap.target_apps,
        },
        "ime_indicator": {
            "enabled": settings.ime_indicator.enabled,
            "hold_ms": settings.ime_indicator.hold_ms,
            "fade_ms": settings.ime_indicator.fade_ms,
            "size": settings.ime_indicator.size,
            "position": settings.ime_indicator.position,
            "theme": settings.ime_indicator.theme,
            "opacity": settings.ime_indicator.opacity,
            "hide_in_fullscreen": settings.ime_indicator.hide_in_fullscreen,
            "hide_from_capture": settings.ime_indicator.hide_from_capture,
            "flash": settings.ime_indicator.flash,
            "badge_enabled": settings.ime_indicator.badge_enabled,
            "badge": settings.ime_indicator.badge,
            "badge_size": settings.ime_indicator.badge_size,
            "badge_when": settings.ime_indicator.badge_when,
            "badge_hold_ms": settings.ime_indicator.badge_hold_ms,
            "badge_opacity": settings.ime_indicator.badge_opacity,
            "badge_on_text_cursor": settings.ime_indicator.badge_on_ibeam,
            "badge_keep_at_caret": settings.ime_indicator.badge_keep_at_caret,
            "app_modes_enabled": settings.ime_indicator.app_modes_enabled,
            "app_modes": settings.ime_indicator.app_modes,
        },
        "log": {
            "enabled": settings.log.enabled,
        },
        "update": {
            "check_on_startup": settings.update.check_on_startup,
            "check_every_hours": settings.update.check_every_hours,
            "mode": settings.update.mode,
        },
        "clipboard_history": settings.clipboard_history,
    }))
}

/// 定型文を `settings.json` に書き込む（定型文の画面と、履歴から登録したとき）。
/// 同じグループの定型文が続くように並べてから書く。
pub fn save_snippets(snippets: &[Snippet]) -> Result<(), String> {
    let mut snippets = snippets.to_vec();
    crate::snippets::sort_by_group(&mut snippets, None);
    let value = serde_json::to_value(snippets).map_err(|e| e.to_string())?;
    save_patch(&serde_json::json!({ "snippets": value }))
}

/// クリップボードの履歴の一覧の不透明度だけを書き込む（一覧の上で Shift+ホイールで変えたとき）。
pub fn save_clipboard_history_opacity(opacity: u32) -> Result<(), String> {
    save_patch(&serde_json::json!({ "clipboard_history": { "opacity": opacity } }))
}

/// 一覧を出したままにするかだけを書き込む（一覧の上で切り替えたとき）。
pub fn save_clipboard_history_keep_open(keep_open: bool) -> Result<(), String> {
    save_patch(&serde_json::json!({ "clipboard_history": { "keep_open": keep_open } }))
}

/// キー割り当ての一覧を `settings.json` に書き込む（キー割り当て画面から使う）。
pub fn save_hotkeys(hotkeys: &[HotkeyRuleSetting]) -> Result<(), String> {
    let value = serde_json::to_value(hotkeys).map_err(|e| e.to_string())?;
    save_patch(&serde_json::json!({ "hotkeys": value }))
}

/// 設定とキー割り当てを、まとめて書き出す（PC の引っ越し用）。
///
/// `settings.json` の中身（利用者が書いた項目も含む）に、自動起動の状態（`autostart`。
/// スタートアップのショートカットで表しているため settings.json には無い）を添える。
/// クリップボードの履歴の中身は含めない。
pub fn export_all(autostart: bool) -> Result<String, String> {
    let mut root = match settings_path().map(|p| std::fs::read_to_string(&p)) {
        Some(Ok(text)) => serde_json::from_str::<serde_json::Value>(strip_bom(&text))
            .map_err(|e| format!("settings.json を解釈できないため書き出せません: {e}"))?,
        // ファイルはあるのに読めないときは、既定値を書き出さない（正しい控えに見えてしまう）。
        Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!(
                "settings.json を読めないため書き出せません。\n{}",
                read_error_reason(&e)
            ))
        }
        _ => serde_json::to_value(Settings::load()).map_err(|e| e.to_string())?,
    };
    let object = root
        .as_object_mut()
        .ok_or("settings.json の形が想定と違うため書き出せません")?;
    object.insert(EXPORT_MARK.into(), serde_json::json!(env!("CARGO_PKG_VERSION")));
    object.insert(AUTOSTART_KEY.into(), serde_json::json!(autostart));
    serde_json::to_string_pretty(&root).map_err(|e| e.to_string())
}

/// 書き出したファイルの印（書き出した版）。
const EXPORT_MARK: &str = "exported_by_version";
/// 書き出したファイルの、自動起動の状態。
const AUTOSTART_KEY: &str = "autostart";

/// 読み込んだ設定。
#[derive(Debug)]
pub struct Imported {
    /// settings.json にそのまま書く中身（書き出し用の項目は除いたもの）。
    pub root: serde_json::Value,
    /// 読み取った設定。
    pub settings: Settings,
    /// 自動起動の状態（書かれていなければ `None` = 今のまま）。
    pub autostart: Option<bool>,
}

/// 書き出したファイル（または settings.json）を読む。このアプリの設定らしい項目が 1 つも
/// 無ければ、誤ったファイルとみなして `Err`。
pub fn parse_import(text: &str) -> Result<Imported, String> {
    let mut root: serde_json::Value = serde_json::from_str(strip_bom(text))
        .map_err(|e| format!("JSON として読めません（{e}）"))?;
    let object = root.as_object_mut().ok_or("設定の形が想定と違います")?;
    const KNOWN: [&str; 7] = [
        "remap",
        "hotkeys",
        "ime_indicator",
        "clipboard_history",
        "snippets",
        "update",
        "log",
    ];
    if !KNOWN.iter().any(|k| object.contains_key(*k)) {
        return Err("このアプリの設定が書かれていません".into());
    }
    let autostart = object.remove(AUTOSTART_KEY).and_then(|v| v.as_bool());
    object.remove(EXPORT_MARK);
    let settings: Settings = serde_json::from_value(root.clone())
        .map_err(|e| format!("設定の形が想定と違います（{e}）"))?;
    Ok(Imported {
        root,
        settings,
        autostart,
    })
}

/// 読み込んだ設定を、今の設定と合わせて仕上げる（[`Imported::root`] と `settings` を直す）。
///
/// - 読み込んだファイルにキー割り当て・定型文が書かれていなければ、今のものを残す
///   （前の版で書き出したファイルや、一部だけを書いたファイルで消えてしまわないように）。
/// - 更新の確認先（`update` の URL・ファイル名）は、読み込んだファイルの値を使わず今の値を残す
///   （他人から受け取ったファイルで、別の配布元から更新させられないように）。
pub fn merge_import(mut imported: Imported, current: &Settings) -> Result<Imported, String> {
    let object = imported
        .root
        .as_object_mut()
        .ok_or("設定の形が想定と違います")?;
    if !object.contains_key("hotkeys") {
        object.insert("hotkeys".into(), serde_json::to_value(&current.hotkeys).map_err(|e| e.to_string())?);
    }
    if !object.contains_key("snippets") {
        object.insert("snippets".into(), serde_json::to_value(&current.snippets).map_err(|e| e.to_string())?);
    }
    let update = object
        .entry("update")
        .or_insert_with(|| serde_json::json!({}));
    if !update.is_object() {
        *update = serde_json::json!({});
    }
    if let Some(update) = update.as_object_mut() {
        update.insert("api_url".into(), current.update.api_url.clone().into());
        update.insert("releases_page".into(), current.update.releases_page.clone().into());
        update.insert("asset_name".into(), current.update.asset_name.clone().into());
    }
    imported.settings = serde_json::from_value(imported.root.clone())
        .map_err(|e| format!("設定の形が想定と違います（{e}）"))?;
    Ok(imported)
}

/// 読み込んだ設定で `settings.json` を置き換える。
pub fn replace_settings(root: &serde_json::Value) -> Result<(), String> {
    let _lock = SETTINGS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = settings_path().ok_or("設定ファイルの場所を決められません")?;
    let text = serde_json::to_string_pretty(root).map_err(|e| e.to_string())?;
    // 読み込みで置き換える前の内容を残す（JSON として読めるものだけ。壊れたものは残さない）。
    if let Ok(current) = std::fs::read_to_string(&path) {
        if serde_json::from_str::<serde_json::Value>(strip_bom(&current)).is_ok() && current != text {
            back_up_settings(&path, current.as_bytes());
        }
    }
    write_replacing(&path, text.as_bytes())
        .map_err(|e| format!("settings.json に書き込めませんでした: {e}"))
}

/// クリップボードの履歴を残しておくファイルのパス。
pub fn clip_history_file() -> Option<PathBuf> {
    data_dir().map(|d| d.join(CLIP_HISTORY_FILE))
}

/// クリップボードの履歴の画像を置くフォルダー。
pub fn clip_images_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("clip_images"))
}

/// ファイルを、途中で壊れないように書き換える（[`write_replacing`]）。
pub fn write_file_safely(path: &Path, data: &[u8]) -> std::io::Result<()> {
    write_replacing(path, data)
}

/// 設定全体の読み込みで扱うファイルの大きさの上限（定型文が多くても読めるよう、キー割り当てより大きい）。
pub const SETTINGS_FILE_MAX_BYTES: u64 = 128 * 1024 * 1024;

/// 書き出し・読み込みで扱うファイルの大きさの上限（誤って大きなファイルを選んだときに備える）。
pub const HOTKEYS_FILE_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// キー割り当てを、書き出し用の JSON にする。形は `settings.json` と同じ
/// （`{ "hotkeys": [...] }`）なので、ほかの PC の settings.json もそのまま読み込める。
pub fn hotkeys_to_json(hotkeys: &[HotkeyRuleSetting]) -> Result<String, String> {
    let value = serde_json::json!({ "hotkeys": hotkeys });
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

/// 書き出したファイル（または settings.json）から、キー割り当てを読む。
/// `{ "hotkeys": [...] }` のほか、割り当ての配列そのものも受け付ける。
pub fn hotkeys_from_json(text: &str) -> Result<Vec<HotkeyRuleSetting>, String> {
    let value: serde_json::Value = serde_json::from_str(strip_bom(text))
        .map_err(|e| format!("JSON として読めません（{e}）"))?;
    let list = match value {
        serde_json::Value::Object(mut root) => root
            .remove("hotkeys")
            .ok_or("キー割り当て（hotkeys）が書かれていません")?,
        list @ serde_json::Value::Array(_) => list,
        _ => return Err("キー割り当ての形が想定と違います".into()),
    };
    serde_json::from_value(list).map_err(|e| format!("キー割り当ての形が想定と違います（{e}）"))
}

/// `patch` に書かれた項目だけを `settings.json` に上書きする。
///
/// ファイル全体を [`Settings`] で書き直すと、利用者が書いた未知の項目が
/// 失われるため、JSON として読んで差分だけを重ねる（[`merge_json`]）。
/// ファイルが読めない・JSON として解釈できない場合は、利用者の編集内容を
/// 壊さないよう**書き込まずに** `Err` を返す。
fn save_patch(patch: &serde_json::Value) -> Result<(), String> {
    // 複数の画面・スレッドから書くので、読んでから書き終えるまでを 1 つずつにする
    // （同時に書くと、片方の変更が失われることがある）。
    let _lock = SETTINGS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = settings_path().ok_or("設定ファイルの場所を決められません")?;
    // 書き換える前の内容（バックアップに使う。ファイルが無いときは None）。
    let mut previous: Option<String> = None;
    let mut root: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(text) => {
            let root = serde_json::from_str(strip_bom(&text)).map_err(|e| {
                format!(
                    "settings.json を解釈できないため保存しませんでした。\n\
                     ファイルを直すか、削除してからもう一度保存してください。\n\n{}\n{e}",
                    path.display()
                )
            })?;
            previous = Some(text);
            root
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            serde_json::to_value(Settings::default()).map_err(|e| e.to_string())?
        }
        Err(e) => return Err(format!("settings.json を読めないため保存しませんでした: {e}")),
    };
    if !root.is_object() {
        return Err("settings.json の形が想定と違うため保存しませんでした".into());
    }
    merge_json(&mut root, patch);
    let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    // 中身が変わるときだけ、書き換える前の内容を残す（失敗しても保存は続ける）。
    if let Some(previous) = previous.filter(|p| *p != text) {
        back_up_settings(&path, previous.as_bytes());
    }
    write_replacing(&path, text.as_bytes())
        .map_err(|e| format!("settings.json に書き込めませんでした: {e}"))
}

/// ファイルを書き換える。いったん隣の一時ファイルに書いてから置き換えるので、書き込みの
/// 途中で電源が切れたりしても、元のファイルが壊れた中途半端な状態で残らない。
fn write_replacing(path: &Path, data: &[u8]) -> std::io::Result<()> {
    // 一時ファイルの名前は書くたびに変える（同時に書いても、互いの一時ファイルを壊さない）。
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = PathBuf::from(temp);
    std::fs::write(&temp, data)?;
    // Windows でも、既にあるファイルを置き換えられる（MoveFileExW の置き換え指定）。
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// 設定のバックアップを置くフォルダーの名前（`settings.json` と同じフォルダーの中に作る）。
const BACKUP_FOLDER: &str = "backup";
/// 残しておくバックアップの数（これより古いものは消す）。
const BACKUP_KEEP: usize = 10;
/// 前のバックアップからこれだけ経っていなければ、新しく作らない（続けて変えたときに、
/// 変更のたびに作って古い世代を押し出してしまわないように）。
const BACKUP_MIN_INTERVAL_SECS: i64 = 10 * 60;
/// バックアップのファイル名の前後（間に日時 `YYYYMMDD-HHMMSS` を入れる）。
const BACKUP_PREFIX: &str = "settings-";
const BACKUP_SUFFIX: &str = ".json";

/// 日時（秒まで）。バックアップの名前に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Stamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Stamp {
    /// UNIX 時刻（秒）を、協定世界時の日時にする。
    pub fn from_unix(secs: u64) -> Self {
        let days = (secs / 86_400) as i64;
        let rest = secs % 86_400;
        // 1970-01-01 からの日数を年月日にする（H. Hinnant の civil_from_days）。
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Self {
            year: year.clamp(0, 9999) as u16,
            month: month as u8,
            day: day as u8,
            hour: (rest / 3600) as u8,
            minute: (rest / 60 % 60) as u8,
            second: (rest % 60) as u8,
        }
    }

    /// 1970-01-01 00:00:00 からの秒数（時差は考えない。2 つの日時の差を出すのに使う）。
    fn to_secs(self) -> i64 {
        let (y, m, d) = (i64::from(self.year), i64::from(self.month), i64::from(self.day));
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let mp = if m > 2 { m - 3 } else { m + 9 };
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        days * 86_400 + i64::from(self.hour) * 3600 + i64::from(self.minute) * 60 + i64::from(self.second)
    }
}

/// 今の日時を返す関数（Windows の現地時刻。[`set_local_clock`] で起動時に決める）。
static LOCAL_CLOCK: OnceLock<fn() -> Stamp> = OnceLock::new();

/// バックアップの名前に使う現地時刻の取り方を決める（起動時に 1 回）。決めていないときは
/// 協定世界時を使う（このファイルは Windows に頼らない作りにしているため）。
pub fn set_local_clock(clock: fn() -> Stamp) {
    let _ = LOCAL_CLOCK.set(clock);
}

fn now_stamp() -> Stamp {
    match LOCAL_CLOCK.get() {
        Some(clock) => clock(),
        None => Stamp::from_unix(now_unix()),
    }
}

/// バックアップのファイル名（`settings-YYYYMMDD-HHMMSS.json`）。名前の順が日時の順になる。
fn backup_name(at: Stamp) -> String {
    format!(
        "{BACKUP_PREFIX}{:04}{:02}{:02}-{:02}{:02}{:02}{BACKUP_SUFFIX}",
        at.year, at.month, at.day, at.hour, at.minute, at.second
    )
}

/// バックアップのファイル名から日時を読む。この形でない名前（利用者が置いたファイルなど）は
/// `None`（消したり、比べたりしない）。
fn parse_backup_name(name: &str) -> Option<Stamp> {
    let body = name.strip_prefix(BACKUP_PREFIX)?.strip_suffix(BACKUP_SUFFIX)?;
    if body.len() != 15 || !body.is_char_boundary(8) || &body[8..9] != "-" {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u16> {
        let part = body.get(range)?;
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let stamp = Stamp {
        year: digits(0..4)?,
        month: digits(4..6)? as u8,
        day: digits(6..8)? as u8,
        hour: digits(9..11)? as u8,
        minute: digits(11..13)? as u8,
        second: digits(13..15)? as u8,
    };
    let valid = (1..=12).contains(&stamp.month)
        && (1..=31).contains(&stamp.day)
        && stamp.hour < 24
        && stamp.minute < 60
        && stamp.second < 60;
    valid.then_some(stamp)
}

/// いちばん新しいバックアップの名前と日時。
fn newest_backup(names: &[String]) -> Option<(&str, Stamp)> {
    names
        .iter()
        .filter_map(|n| parse_backup_name(n).map(|s| (n.as_str(), s)))
        .max_by_key(|(_, s)| *s)
}

/// `now` に新しいバックアップを作るか。いちばん新しいものから 10 分経っていなければ作らない。
/// 時計が戻った（いちばん新しいものが `now` より先の日時）ときは作る。
fn backup_due(names: &[String], now: Stamp) -> bool {
    match newest_backup(names) {
        None => true,
        Some((_, newest)) => {
            let elapsed = now.to_secs() - newest.to_secs();
            !(0..BACKUP_MIN_INTERVAL_SECS).contains(&elapsed)
        }
    }
}

/// 新しい方から `keep` 個を残すとき、消すバックアップの名前（古い順）。
fn backups_to_remove(names: &[String], keep: usize) -> Vec<String> {
    let mut backups: Vec<&String> = names.iter().filter(|n| parse_backup_name(n).is_some()).collect();
    backups.sort();
    backups.dedup();
    let excess = backups.len().saturating_sub(keep);
    backups.into_iter().take(excess).cloned().collect()
}

/// settings.json（`path`）を書き換える前の内容 `current` を、`backup` フォルダーに残す。
/// 失敗しても保存は止めず、記録に残すだけにする。
fn back_up_settings(path: &Path, current: &[u8]) {
    if let Err(e) = try_back_up_settings(path, current, now_stamp()) {
        log::warn!("設定のバックアップを作れませんでした（保存は続けます）: {e}");
    }
}

fn try_back_up_settings(path: &Path, current: &[u8], now: Stamp) -> std::io::Result<()> {
    let Some(dir) = path.parent().map(|p| p.join(BACKUP_FOLDER)) else {
        return Ok(());
    };
    let mut names: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    if !backup_due(&names, now) {
        return Ok(());
    }
    // いちばん新しいバックアップと同じ内容なら、作らない。
    if let Some((newest, _)) = newest_backup(&names) {
        if std::fs::read(dir.join(newest)).is_ok_and(|saved| saved == current) {
            return Ok(());
        }
    }
    std::fs::create_dir_all(&dir)?;
    let name = backup_name(now);
    let target = dir.join(&name);
    if target.exists() {
        return Ok(());
    }
    write_replacing(&target, current)?;
    log::info!("設定のバックアップを作りました: {name}");
    names.push(name);
    for old in backups_to_remove(&names, BACKUP_KEEP) {
        if let Err(e) = std::fs::remove_file(dir.join(&old)) {
            log::warn!("古い設定のバックアップ {old} を消せませんでした: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod backup_tests {
    use super::{backup_due, backup_name, backups_to_remove, parse_backup_name, try_back_up_settings, Stamp};

    fn stamp(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Stamp {
        Stamp { year, month, day, hour, minute, second }
    }

    #[test]
    fn name_round_trips_and_sorts_by_time() {
        let a = stamp(2026, 1, 2, 3, 4, 5);
        assert_eq!(backup_name(a), "settings-20260102-030405.json");
        assert_eq!(parse_backup_name(&backup_name(a)), Some(a));
        let b = stamp(2026, 11, 15, 23, 59, 59);
        assert!(backup_name(a) < backup_name(b));
    }

    #[test]
    fn other_names_are_ignored() {
        for name in [
            "settings.json",
            "settings-2026010-030405.json",
            "settings-20260102_030405.json",
            "settings-20261302-030405.json",
            "settings-20260102-246000.json",
            "settings-2026O102-030405.json",
            "settings-2026010あ-0304.json",
            "settings-20260102-030405.json.bak",
            "memo.txt",
        ] {
            assert_eq!(parse_backup_name(name), None, "{name}");
        }
    }

    #[test]
    fn unix_time_to_stamp() {
        assert_eq!(Stamp::from_unix(0), stamp(1970, 1, 1, 0, 0, 0));
        // 2024-02-29 12:34:56 UTC（うるう日）
        assert_eq!(Stamp::from_unix(1_709_210_096), stamp(2024, 2, 29, 12, 34, 56));
        let s = stamp(2026, 11, 15, 8, 9, 10);
        assert_eq!(Stamp::from_unix(s.to_secs() as u64), s);
    }

    #[test]
    fn first_backup_is_due() {
        assert!(backup_due(&[], stamp(2026, 1, 1, 0, 0, 0)));
        assert!(backup_due(&["memo.txt".to_string()], stamp(2026, 1, 1, 0, 0, 0)));
    }

    #[test]
    fn no_new_backup_within_ten_minutes() {
        let names = vec![
            "settings-20260101-090000.json".to_string(),
            "settings-20260101-235500.json".to_string(),
        ];
        assert!(!backup_due(&names, stamp(2026, 1, 1, 23, 55, 0)));
        assert!(!backup_due(&names, stamp(2026, 1, 2, 0, 4, 59)));
        // 日をまたいでも、経った時間で数える。
        assert!(backup_due(&names, stamp(2026, 1, 2, 0, 5, 0)));
    }

    #[test]
    fn clock_going_backwards_makes_a_backup() {
        let names = vec!["settings-20260101-120000.json".to_string()];
        assert!(backup_due(&names, stamp(2026, 1, 1, 11, 59, 0)));
    }

    #[test]
    fn keeps_newest_backups() {
        let mut names: Vec<String> = (0..12).map(|i| backup_name(stamp(2026, 1, 1 + i, 0, 0, 0))).collect();
        names.reverse();
        names.push("memo.txt".to_string());
        assert_eq!(
            backups_to_remove(&names, 10),
            vec!["settings-20260101-000000.json".to_string(), "settings-20260102-000000.json".to_string()]
        );
        assert!(backups_to_remove(&names[..5], 10).is_empty());
    }

    #[test]
    fn backs_up_into_folder() {
        let dir = std::env::temp_dir().join(format!("atai-backup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let settings = dir.join("settings.json");
        let backups = dir.join("backup");
        let count = || std::fs::read_dir(&backups).map(|d| d.count()).unwrap_or(0);
        // フォルダーが無ければ作る。
        std::fs::create_dir_all(&dir).unwrap();
        try_back_up_settings(&settings, b"{}", stamp(2026, 1, 1, 0, 0, 0)).unwrap();
        assert_eq!(count(), 1);
        // 10 分経っていても、いちばん新しいものと同じ内容なら作らない。
        try_back_up_settings(&settings, b"{}", stamp(2026, 1, 1, 1, 0, 0)).unwrap();
        assert_eq!(count(), 1);
        // 10 分経っていなければ作らない。
        try_back_up_settings(&settings, b"{\"a\":1}", stamp(2026, 1, 1, 0, 9, 0)).unwrap();
        assert_eq!(count(), 1);
        // 内容が違い、10 分経っていれば作る。数が上限を超えたら古いものから消す。
        for i in 0..12u8 {
            let data = format!("{{\"a\":{i}}}");
            try_back_up_settings(&settings, data.as_bytes(), stamp(2026, 1, 2, i, 0, 0)).unwrap();
        }
        assert_eq!(count(), 10);
        assert!(!backups.join("settings-20260101-000000.json").exists());
        assert_eq!(std::fs::read(backups.join("settings-20260102-110000.json")).unwrap(), b"{\"a\":11}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `patch` を `base` に重ねる。オブジェクト同士は項目ごとに再帰的に重ね、
/// それ以外（値・配列）は `patch` の値で置き換える。`patch` に無い項目は残す。
fn merge_json(base: &mut serde_json::Value, patch: &serde_json::Value) {
    match (base, patch) {
        (serde_json::Value::Object(base), serde_json::Value::Object(patch)) => {
            for (key, value) in patch {
                match base.get_mut(key) {
                    Some(existing) if existing.is_object() && value.is_object() => {
                        merge_json(existing, value)
                    }
                    _ => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, patch) => *base = patch.clone(),
    }
}

/// 設定ファイルのパス（設定画面の「設定ファイルの場所を開く」で使う）。
pub fn settings_file() -> Option<PathBuf> {
    settings_path()
}

/// 動作の記録（`log.txt`）のパス。設定ファイルと同じフォルダに置く。
pub fn log_file() -> Option<PathBuf> {
    data_dir().map(|d| d.join(LOG_FILE))
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

/// 実行ファイルのフォルダに作る、設定・状態・記録を置くフォルダの名前。
const DATA_FOLDER: &str = "data";

/// 以前（実行ファイルと同じフォルダに置いていたころ）のファイルで、`data` フォルダへ移すもの。
const LEGACY_FILES: [&str; 5] = [SETTINGS_FILE, STATE_FILE, CLIP_HISTORY_FILE, LOG_FILE, "log.old.txt"];

/// 以前の場所から `data` フォルダへ移したときの記録（記録の準備ができる前に移すので、
/// あとで [`take_migration_notes`] から取り出して記録に残す）。
static MIGRATION_NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// 設定・状態・記録を置くディレクトリ。
///
/// PC の引っ越しでフォルダごと持ち運べるよう、実行ファイルのフォルダの下の `data` フォルダを
/// 使う（無ければ作る）。以前の版が実行ファイルと同じフォルダに置いたファイルは、初めてここを
/// 決めたときに `data` フォルダへ移す（[`migrate_legacy_files`]）。
/// `Program Files` 配下など書き込めない場所に置かれている場合は、
/// `%LOCALAPPDATA%\Atai-paste` にフォールバックする。
fn data_dir() -> Option<PathBuf> {
    // 書き込めるかを確かめるためにファイルを作って消すので、決めるのは 1 回だけにする
    // （複数のスレッドが同時に確かめると、片方が「書き込めない」と誤ることがある）。
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(find_data_dir).clone()
}

fn find_data_dir() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let data = exe_dir.join(DATA_FOLDER);
    if std::fs::create_dir_all(&data).is_ok() && is_writable(&data) {
        migrate_legacy_files(&exe_dir, &data);
        return Some(data);
    }
    let local = std::env::var_os("LOCALAPPDATA")?;
    let dir = PathBuf::from(local).join("Atai-paste");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 以前の版が実行ファイルと同じフォルダ（`from`）に置いたファイルを、`data` フォルダ（`to`）へ移す。
/// `to` に同じ名前のファイルがすでにあれば、そちらを使い、古い方はそのまま残す。
/// 移せなかった（ほかのアプリが開いている など）ときは写しを作り、元は残す。
fn migrate_legacy_files(from: &Path, to: &Path) {
    let mut notes = MIGRATION_NOTES.lock().unwrap_or_else(|p| p.into_inner());
    for name in LEGACY_FILES {
        let old = from.join(name);
        let new = to.join(name);
        if !old.is_file() || new.exists() {
            continue;
        }
        let result = std::fs::rename(&old, &new).or_else(|_| std::fs::copy(&old, &new).map(|_| ()));
        notes.push(match result {
            Ok(()) => format!("{name} を data フォルダへ移しました"),
            Err(e) => format!("{name} を data フォルダへ移せませんでした: {e}"),
        });
    }
}

/// 以前の場所から `data` フォルダへ移したときの記録を取り出す（起動時に記録に残す）。
pub fn take_migration_notes() -> Vec<String> {
    std::mem::take(&mut *MIGRATION_NOTES.lock().unwrap_or_else(|p| p.into_inner()))
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
    use super::{
        hotkeys_from_json, hotkeys_to_json, is_due, merge_import, merge_json, parse_import,
        read_error_reason, strip_bom, HotkeyRuleSetting, Settings,
    };

    #[test]
    fn read_error_reason_explains_non_utf8() {
        // Shift_JIS で保存された「設定」。UTF-8 としては読めない。
        let dir = std::env::temp_dir().join(format!("atai-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sjis.json");
        std::fs::write(&path, [0x90u8, 0xdd, 0x92, 0xe8]).unwrap();
        let e = std::fs::read_to_string(&path).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(read_error_reason(&e).contains("UTF-8 ではありません"));
    }

    #[test]
    fn read_error_reason_keeps_other_errors() {
        let e = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let reason = read_error_reason(&e);
        assert!(!reason.contains("UTF-8"));
        assert!(reason.contains("denied"));
    }

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
    fn merge_keeps_unknown_items_and_overwrites_given_ones() {
        let mut base = serde_json::json!({
            "update": { "api_url": "https://example.invalid/", "check_on_startup": true },
            "ime_indicator": { "enabled": true, "hold_ms": 400 },
            "my_note": "利用者が書いたメモ",
        });
        merge_json(
            &mut base,
            &serde_json::json!({
                "update": { "check_on_startup": false },
                "ime_indicator": { "enabled": false },
                "remap": { "hotkey": "Ctrl+Q", "target_apps": ["EXCEL.EXE", "ET.EXE"] },
            }),
        );
        assert_eq!(
            base,
            serde_json::json!({
                "update": { "api_url": "https://example.invalid/", "check_on_startup": false },
                "ime_indicator": { "enabled": false, "hold_ms": 400 },
                "my_note": "利用者が書いたメモ",
                "remap": { "hotkey": "Ctrl+Q", "target_apps": ["EXCEL.EXE", "ET.EXE"] },
            })
        );
    }

    #[test]
    fn merge_replaces_arrays_instead_of_appending() {
        let mut base = serde_json::json!({ "remap": { "target_apps": ["A.EXE", "B.EXE"] } });
        merge_json(&mut base, &serde_json::json!({ "remap": { "target_apps": ["C.EXE"] } }));
        assert_eq!(base, serde_json::json!({ "remap": { "target_apps": ["C.EXE"] } }));
    }

    #[test]
    fn old_settings_file_gets_remap_defaults() {
        // v1.3.0 以前の settings.json には remap が無い。既定の Ctrl+B / Excel になること。
        let s: Settings = serde_json::from_str(r#"{ "update": { "check_on_startup": false } }"#).unwrap();
        assert_eq!(s.remap.hotkey, "Ctrl+B");
        assert_eq!(s.remap.target_apps, ["EXCEL.EXE"]);
        assert!(!s.update.check_on_startup);
    }

    #[test]
    fn old_settings_file_gets_fade_default() {
        // v1.4.0 以前の settings.json には fade_ms が無い。これまでどおり 0.25 秒になること。
        let s: Settings =
            serde_json::from_str(r#"{ "ime_indicator": { "hold_ms": 800 } }"#).unwrap();
        assert_eq!(s.ime_indicator.hold_ms, 800);
        assert_eq!(s.ime_indicator.fade_ms, 250);
        // v1.4.1 以前には表示の出し方と記録の設定も無い。これまでの見た目と同じになること。
        assert_eq!(s.ime_indicator.position, "center");
        assert_eq!(s.ime_indicator.theme, "dark");
        assert_eq!(s.ime_indicator.opacity, 90);
        assert!(s.ime_indicator.hide_in_fullscreen);
        assert!(!s.log.enabled);
        // キー割り当ては空
        assert!(s.hotkeys.is_empty());
    }

    #[test]
    fn hotkey_rules_read_leniently() {
        let s: Settings = serde_json::from_str(
            r#"{ "hotkeys": [
                { "hotkey": "Ctrl+Alt+V", "action": "paste_plain" },
                { "hotkey": "Win+N", "action": "run", "value": "notepad.exe", "enabled": false },
                { "hotkey": "F9", "action": "そんな動作は無い" }
            ] }"#,
        )
        .unwrap();
        assert_eq!(s.hotkeys.len(), 3);
        assert!(s.hotkeys[0].enabled); // 書かなければ使う
        assert!(s.hotkeys[0].apps.is_empty());
        assert!(!s.hotkeys[1].enabled);
        // 知らない動作でも設定ファイル全体は読める（その割り当てだけ使わない）
        assert_eq!(s.hotkeys[2].action, "そんな動作は無い");
        // 入れ方を書かなければ、まとめて貼り付ける
        assert_eq!(s.hotkeys[0].input, "paste");
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

    #[test]
    fn hotkeys_round_trip() {
        let rules = vec![HotkeyRuleSetting {
            hotkey: "Ctrl+Alt+V".into(),
            action: "paste_plain".into(),
            apps: vec!["EXCEL.EXE".into()],
            ..Default::default()
        }];
        let text = hotkeys_to_json(&rules).unwrap();
        assert_eq!(hotkeys_from_json(&text).unwrap(), rules);
        // 配列そのもの・BOM 付き・settings.json 全体も読める。
        let array = serde_json::to_string(&rules).unwrap();
        assert_eq!(hotkeys_from_json(&format!("\u{feff}{array}")).unwrap(), rules);
        let whole = format!(r#"{{"remap":{{"hotkey":"Ctrl+B"}},"hotkeys":{array}}}"#);
        assert_eq!(hotkeys_from_json(&whole).unwrap(), rules);
    }

    #[test]
    fn import_settings() {
        let text = r#"{"remap":{"hotkey":"Ctrl+Q"},"autostart":true,"exported_by_version":"1.6.0",
                       "clipboard_history":{"enabled":true,"trigger":"double_ctrl"},"mine":1}"#;
        let imported = parse_import(&format!("\u{feff}{text}")).unwrap();
        assert_eq!(imported.autostart, Some(true));
        assert_eq!(imported.settings.remap.hotkey, "Ctrl+Q");
        assert!(imported.settings.clipboard_history.enabled);
        assert_eq!(imported.settings.clipboard_history.trigger, "double_ctrl");
        // 書かれていない項目は既定値。
        assert_eq!(imported.settings.clipboard_history.max_items, 1000);
        // 書き出し用の印は settings.json に入れない。利用者の項目は残す。
        let root = imported.root.as_object().unwrap();
        assert!(!root.contains_key("autostart"));
        assert!(!root.contains_key("exported_by_version"));
        assert!(root.contains_key("mine"));

        assert!(parse_import(r#"{"hotkeys":[]}"#).unwrap().autostart.is_none());
        assert!(parse_import(r#"{"name":"別のアプリ"}"#).is_err());
        assert!(parse_import("[1]").is_err());
        assert!(parse_import("x").is_err());
    }

    #[test]
    fn import_keeps_missing_lists_and_update_source() {
        let current = Settings {
            hotkeys: vec![HotkeyRuleSetting {
                hotkey: "Ctrl+Alt+V".into(),
                action: "paste_plain".into(),
                ..Default::default()
            }],
            snippets: vec![super::Snippet {
                name: "a".into(),
                text: "b".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        // キー割り当て・定型文が無いファイル: 今のものを残す。更新の確認先は今のまま。
        let text = r#"{"remap":{"hotkey":"Ctrl+Q"},
                       "update":{"api_url":"https://api.github.com/repos/someone/else/releases/latest",
                                 "check_on_startup":false}}"#;
        let merged = merge_import(parse_import(text).unwrap(), &current).unwrap();
        assert_eq!(merged.settings.hotkeys, current.hotkeys);
        assert_eq!(merged.settings.snippets, current.snippets);
        assert_eq!(merged.settings.update.api_url, current.update.api_url);
        assert!(!merged.settings.update.check_on_startup);
        assert_eq!(merged.root["update"]["api_url"], current.update.api_url.as_str());
        // 書かれていれば、空でもそれを使う。
        let merged =
            merge_import(parse_import(r#"{"hotkeys":[],"snippets":[]}"#).unwrap(), &current).unwrap();
        assert!(merged.settings.hotkeys.is_empty());
        assert!(merged.settings.snippets.is_empty());
    }

    #[test]
    fn hotkeys_from_bad_json() {
        assert!(hotkeys_from_json("not json").is_err());
        assert!(hotkeys_from_json(r#"{"remap":{}}"#).is_err());
        assert!(hotkeys_from_json("3").is_err());
        assert!(hotkeys_from_json(r#"{"hotkeys":"x"}"#).is_err());
    }

    #[test]
    fn lenient_load_keeps_readable_parts() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{
                "remap": { "hotkey": "Ctrl+Q", "target_apps": "EXCEL.EXE" },
                "log": { "enabled": true },
                "hotkeys": [
                    { "hotkey": "F9", "action": "block" },
                    { "hotkey": "F8", "action": "block", "apps": "x" }
                ],
                "snippets": [ { "name": "a", "text": "b" } ]
            }"#,
        )
        .unwrap();
        assert!(serde_json::from_value::<Settings>(value.clone()).is_err());
        let (settings, problems) = Settings::from_value_lenient(&value);
        // 読めなかった所だけが既定値になる。
        assert_eq!(settings.remap.hotkey, Settings::default().remap.hotkey);
        assert!(settings.log.enabled);
        assert_eq!(settings.hotkeys.len(), 1);
        assert_eq!(settings.hotkeys[0].hotkey, "F9");
        assert_eq!(settings.snippets.len(), 1);
        assert_eq!(problems.len(), 2);
        assert!(problems[0].starts_with("remap"));
        assert!(problems[1].starts_with("hotkeys の 2 件目"));
    }
}
