//! 更新の確認・ダウンロード・インストール。
//!
//! GitHub の Releases API から最新版を調べ、現在より新しければ利用者に確認したうえで
//! 新しい実行ファイルへ置き換える。
//!
//! ## 実行中の EXE を置き換える方法
//!
//! 実行中の EXE は Windows がロックしているため上書きできないが、**リネームはできる**。
//! そこで次の順で入れ替える（Chrome などと同じ考え方）。
//!
//! 1. 新しい実行ファイルを、同じフォルダに `Atai-paste.exe.new` としてコピーで書き出す
//! 2. `Atai-paste.exe` → `Atai-paste.exe.old` にリネーム（実行中でも可能）
//! 3. `Atai-paste.exe.new` → `Atai-paste.exe` にリネーム
//! 4. 新しい EXE を起動して、自分は終了する
//! 5. 次回起動時に `.old` を削除する（[`cleanup_old`]）
//!
//! 時間のかかる「コピー」を先に済ませておき、実際の入れ替えはリネーム 2 回
//! （2→3）だけで行う。リネームは同一ボリューム上ならほぼ一瞬で終わるため、
//! 実行ファイルが存在しない瞬間の窓は大幅に狭い。
//!
//! 3 が失敗した場合は 2 を巻き戻す（[`install`] のロールバック）。ただし、
//! **電源断や強制終了など OS ごと落ちるケースでは、この巻き戻しコード自体が
//! 実行されない**ため保証の対象外である。そのために [`cleanup_old`] が
//! 次回起動時、`.exe` が無く `.old` だけが残っている場合に `.old` を
//! `.exe` へ戻す（復元を試みる）。それでも、複製元の `.old` 自体が壊れて
//! いた場合などは救えない。詳細は README の「実行中の EXE を入れ替える仕組み」
//! を参照。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use windows::core::PCWSTR;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO,
    MESSAGEBOX_STYLE, SW_SHOWNORMAL,
};

use crate::config::{self, Settings};
use crate::{http, TrayMessage};

/// ダイアログのタイトル。
const DIALOG_TITLE: &str = "アタイの貼り付け";
/// GitHub API に指定する Accept ヘッダ。
const GITHUB_ACCEPT: &str = "application/vnd.github+json";
/// 一時フォルダへ書き出す更新ファイルの名前の接頭辞。
const TEMP_PREFIX: &str = "Atai-paste-";
/// 一時ファイルを残骸とみなすまでの時間（秒）。
/// 更新処理は数秒で終わるため、これより古いものは中断された残骸と判断する。
const TEMP_STALE_SECS: u64 = 60 * 60;

/// 更新処理（[`run`]）が現在実行中かどうか。多重実行の防止に使う。
///
/// 起動時の自動確認とメニューからの手動確認が同時に走ると、確認ダイアログが
/// 二重に出たり、同じ一時ファイルへ同時に書き込んだり、EXE を二重に入れ替えたり
/// する恐れがある。[`RunningGuard`] で `run()` の実行区間を排他制御する。
static UPDATE_RUNNING: AtomicBool = AtomicBool::new(false);

/// [`UPDATE_RUNNING`] の実行権を表すガード。
///
/// 生存している間だけフラグが `true` になり、Drop で確実に `false` へ戻す。
/// `run()` は早期 return が多いため、戻し忘れを防ぐのに Drop での解放を使う。
struct RunningGuard;

impl RunningGuard {
    /// 実行権の取得を試みる。既に実行中であれば `None` を返す。
    fn acquire() -> Option<Self> {
        try_acquire(&UPDATE_RUNNING).then_some(Self)
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        release(&UPDATE_RUNNING);
    }
}

/// `false` → `true` への切替を試みる（純粋なロジック本体。テストのため分離）。
fn try_acquire(flag: &AtomicBool) -> bool {
    flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

/// フラグを `false` に戻す。
fn release(flag: &AtomicBool) {
    flag.store(false, Ordering::SeqCst);
}

/// 更新処理が現在実行中かどうか。
///
/// `main.rs` が終了処理（`Quit`）で、実行中の入れ替えを中断させないよう
/// 完了を待つために使う。
pub fn is_running() -> bool {
    UPDATE_RUNNING.load(Ordering::SeqCst)
}

/// 三つ組みのバージョン番号。
///
/// 比較は必ず**数値として**行う。文字列比較では `1.0.10` < `1.0.9` と
/// 誤判定してしまうため。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Version {
    /// `v1.2.3` / `1.2.3` / `1.2` などを解析する。
    /// `1.2.3-beta` のような接尾辞は無視して数値部分だけを見る。
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim();
        let t = t.strip_prefix('v').or_else(|| t.strip_prefix('V')).unwrap_or(t);
        // ハイフン以降（プレリリース識別子）とビルドメタデータは切り捨てる。
        let t = t.split(['-', '+']).next()?;

        let mut parts = t.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().ok()?;
        let patch = parts.next().unwrap_or("0").parse().ok()?;
        Some(Self {
            major,
            minor,
            patch,
        })
    }

    /// このビルドのバージョン。
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self {
            major: 0,
            minor: 0,
            patch: 0,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// GitHub Releases API のレスポンス（必要な項目のみ）。
#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// リリースに添付されたファイル。
#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    /// `sha256:...` 形式。GitHub が付与しない場合もあるため任意。
    #[serde(default)]
    digest: Option<String>,
}

/// 見つかった更新の情報。
#[derive(Debug, Clone)]
pub struct Available {
    pub version: Version,
    pub download_url: String,
    pub sha256: Option<String>,
    pub page_url: String,
}

/// 最新リリースを問い合わせ、現在より新しければ返す。
pub fn check(settings: &Settings) -> Result<Option<Available>, String> {
    let body = http::get(&settings.update.api_url, Some(GITHUB_ACCEPT), |_, _| {})?;
    let release: Release =
        serde_json::from_slice(&body).map_err(|e| format!("応答の解析に失敗: {e}"))?;

    let latest = Version::parse(&release.tag_name)
        .ok_or_else(|| format!("バージョンを解釈できません: {}", release.tag_name))?;
    let current = Version::current();

    log::info!("更新確認: 現在 {current} / 最新 {latest}");
    if latest <= current {
        return Ok(None);
    }

    // 目的の実行ファイルが添付されているか探す。
    let asset = release
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(&settings.update.asset_name));
    let Some(asset) = asset else {
        return Err(format!(
            "リリース {} に {} が添付されていません",
            release.tag_name, settings.update.asset_name
        ));
    };

    let sha256 = asset.digest.as_ref().and_then(|d| {
        d.strip_prefix("sha256:")
            .map(|h| h.trim().to_ascii_lowercase())
    });

    Ok(Some(Available {
        version: latest,
        download_url: asset.browser_download_url.clone(),
        sha256,
        page_url: if release.html_url.is_empty() {
            settings.update.releases_page.clone()
        } else {
            release.html_url.clone()
        },
    }))
}

/// 更新ファイルをダウンロードし、SHA256 が判っていれば検証する。
///
/// 検証を通ったものだけを一時ファイルとして書き出し、そのパスを返す。
fn download_and_verify(
    info: &Available,
    tx: &Sender<TrayMessage>,
) -> Result<PathBuf, String> {
    let mut last_percent = u64::MAX;
    let bytes = http::get(&info.download_url, None, |done, total| {
        // 進捗はトレイのツールチップに出す。更新のたびに送ると煩いので
        // パーセントが変わったときだけ通知する。
        let percent = match total {
            Some(t) if t > 0 => done * 100 / t,
            _ => 0,
        };
        if percent != last_percent {
            last_percent = percent;
            let _ = tx.send(TrayMessage::UpdateProgress(percent));
        }
    })?;

    // 最低限の妥当性検査: PE 実行ファイルの署名（先頭 2 バイトが `MZ`）を確認する。
    // GitHub がリリース資産に `digest`（SHA256）を付与していない場合でも、
    // 明らかに壊れた・別種のファイルを実行ファイルとして書き出さないようにする。
    if !has_mz_signature(&bytes) {
        return Err(
            "ダウンロードしたファイルが実行ファイルの形式ではありません。\
             もう一度お試しください。"
                .to_string(),
        );
    }

    if let Some(expected) = &info.sha256 {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual = hex(&hasher.finalize());
        if &actual != expected {
            // 64 桁の hex を利用者に見せても意味がなく不安を与えるだけなので、
            // ダイアログには出さずログにのみ残す。
            log::error!("SHA256 検証に失敗しました（期待: {expected} / 実際: {actual}）");
            return Err(
                "ダウンロードしたファイルが壊れている可能性があります。\
                 もう一度お試しください。"
                    .to_string(),
            );
        }
        log::info!("SHA256 検証に成功しました");
    } else {
        log::warn!("リリースに SHA256 が無いため検証を省略しました");
    }

    // 一時フォルダへ書き出す。途中で失敗した場合は中途半端なファイルを残さない。
    let path = std::env::temp_dir().join(format!("{TEMP_PREFIX}{}.exe", info.version));
    if let Err(e) = std::fs::write(&path, &bytes) {
        let _ = std::fs::remove_file(&path);
        return Err(format!("一時ファイルの書き出しに失敗: {e}"));
    }
    Ok(path)
}

/// 先頭 2 バイトが `MZ`（PE 実行ファイルの署名）であるかを確認する。
///
/// GitHub の `digest` が無い場合でも、明らかに実行ファイルでないもの
/// （HTML のエラーページなど）を弾ける最低限の妥当性検査になる。
fn has_mz_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(b"MZ")
}

/// バイト列を 16 進文字列にする。
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// `.old` を付けたパスを返す。
fn old_path(exe: &Path) -> PathBuf {
    let mut s = exe.to_path_buf().into_os_string();
    s.push(".old");
    PathBuf::from(s)
}

/// `.new` を付けたパスを返す（入れ替え前に新しい EXE を暫定的に置く場所）。
fn staged_path(exe: &Path) -> PathBuf {
    let mut s = exe.to_path_buf().into_os_string();
    s.push(".new");
    PathBuf::from(s)
}

/// ダウンロード済みの実行ファイルを、現在の実行ファイルと入れ替える。
///
/// 時間のかかる「コピー」を先に同じフォルダへ済ませておき、実際の入れ替えは
/// リネーム 2 回（`exe → .old`、`.new → exe`）だけで行う。リネームはほぼ一瞬で
/// 終わるため、実行ファイルが存在しない瞬間の窓を最小限にできる。
///
/// 失敗を検知できた場合はリネームを巻き戻す。ただし電源断や強制終了など
/// OS ごと落ちるケースはこの関数のコード自体が実行されないため対象外であり、
/// その場合の復元は次回起動時の [`cleanup_old`] に委ねる。
fn install(new_exe: &Path) -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|e| format!("自身のパスを取得できません: {e}"))?;
    let dir = current
        .parent()
        .ok_or_else(|| "実行ファイルの場所を特定できません".to_string())?;

    // 書き込めない場所（Program Files など）では自動更新できない。
    if !config::is_writable(dir) {
        return Err(format!(
            "このフォルダには書き込めないため自動更新できません。\n{}\n\n\
             お手数ですが、リリースページから手動で置き換えてください。",
            dir.display()
        ));
    }

    let old = old_path(&current);
    let staged = staged_path(&current);
    // 前回の残骸があれば先に消しておく（残っていてもリネームは失敗する）。
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::remove_file(&staged);

    // 1) 新しい EXE を同じフォルダへ `.new` としてコピーで配置しておく。
    //    ここで失敗しても現在の EXE には一切手を付けていないので安全。
    std::fs::copy(new_exe, &staged)
        .map_err(|e| format!("新しい実行ファイルの配置に失敗しました: {e}"))?;

    // 2) 実行中の EXE をリネーム（実行中でも可能）。
    if let Err(e) = std::fs::rename(&current, &old) {
        let _ = std::fs::remove_file(&staged);
        return Err(format!("現在の実行ファイルを退避できません: {e}"));
    }

    // 3) `.new` を本来の場所へリネーム。失敗したら 2 を巻き戻す。
    if let Err(e) = std::fs::rename(&staged, &current) {
        let _ = std::fs::rename(&old, &current);
        return Err(format!("新しい実行ファイルを配置できません（元に戻しました）: {e}"));
    }

    log::info!("更新を配置しました: {}", current.display());
    Ok(current)
}

/// 実行ファイル自体の復元が必要かどうかの判定（純粋なロジック本体。テストのため分離）。
///
/// `.exe` が無く `.old` だけが残っている状態は、[`install`] の 2 と 3 の間で
/// 処理が中断されたことを示す。この場合は `.old` を `.exe` へ戻さないと
/// 次回起動できなくなる。
fn needs_exe_restore(exe_exists: bool, old_exists: bool) -> bool {
    !exe_exists && old_exists
}

/// 前回の更新で残った残骸を削除する（起動時に呼ぶ）。
///
/// 対象は次の 3 つ。
/// - `.exe` が無く `.old` だけが残っている場合: 中断された入れ替えとみなし、
///   `.old` を `.exe` へ復元する（[`needs_exe_restore`]）。
/// - 通常どおり入れ替えが完了して残った `Atai-paste.exe.old`: 削除する。
/// - 入れ替え直前まで使っていた `Atai-paste.exe.new`: 残っていれば削除する
///   （中断時に配置済みのまま残ることがある）。
/// - 一時フォルダに残った更新ファイル（書き出しの失敗や強制終了で残ることがある）
pub fn cleanup_old() {
    if let Ok(current) = std::env::current_exe() {
        let old = old_path(&current);
        let staged = staged_path(&current);

        if needs_exe_restore(current.exists(), old.exists()) {
            match std::fs::rename(&old, &current) {
                Ok(()) => log::warn!(
                    "中断された更新を検知したため、以前の実行ファイルを復元しました"
                ),
                Err(e) => log::error!("実行ファイルの復元に失敗しました: {e}"),
            }
        } else if old.exists() {
            match std::fs::remove_file(&old) {
                Ok(()) => log::info!("前回の更新の残骸を削除しました"),
                // まだロックされている場合もある。次回の起動で消えるので無視してよい。
                Err(e) => log::warn!("残骸の削除に失敗しました（次回起動時に再試行）: {e}"),
            }
        }

        if staged.exists() {
            match std::fs::remove_file(&staged) {
                Ok(()) => log::info!("入れ替え前の一時ファイル（.new）を削除しました"),
                Err(e) => log::warn!(".new ファイルの削除に失敗しました: {e}"),
            }
        }
    }
    cleanup_temp_files();
}

/// 一時フォルダに残った更新ファイルを削除する。
///
/// 更新中の別インスタンスが使っているファイルを消さないよう、
/// 十分に古いもの（[`TEMP_STALE_SECS`] 以上前）だけを対象にする。
fn cleanup_temp_files() {
    let dir = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let now = std::time::SystemTime::now();

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !is_temp_update_file(name) {
            continue;
        }

        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age.as_secs() >= TEMP_STALE_SECS);
        if !stale {
            continue;
        }

        match std::fs::remove_file(entry.path()) {
            Ok(()) => log::info!("一時フォルダの更新ファイルを削除しました: {name}"),
            Err(e) => log::warn!("一時ファイルの削除に失敗しました: {name}: {e}"),
        }
    }
}

/// 自分が書き出した更新用の一時ファイルかどうか。
///
/// 無関係なファイルを削除しないよう、名前を厳密に判定する。
fn is_temp_update_file(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(TEMP_PREFIX) else {
        return false;
    };
    let Some(version) = rest.strip_suffix(".exe") else {
        return false;
    };
    // 接頭辞と拡張子の間はバージョン番号（数字とドットのみ）であること。
    !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// 更新を確認し、必要ならインストールまで行う。別スレッドから呼ぶこと。
///
/// - `manual` が `true`（メニューからの手動確認）のときは、最新である場合や
///   失敗した場合にもダイアログで結果を知らせる。
/// - `manual` が `false`（起動時の自動確認）のときは、更新が見つかったときだけ
///   利用者に尋ね、それ以外は黙ってログに残す。
pub fn run(settings: Settings, tx: Sender<TrayMessage>, manual: bool) {
    // 起動時の自動確認と手動確認が同時に走らないようにする。既に実行中なら、
    // 手動操作のときだけその旨を知らせて何もしない（自動確認は黙って諦める）。
    let Some(_guard) = RunningGuard::acquire() else {
        log::info!("更新処理は既に実行中のため、今回の要求は無視します");
        if manual {
            message_box(
                "現在、更新を確認しています。完了までしばらくお待ちください。",
                MB_OK | MB_ICONINFORMATION,
            );
        }
        return;
    };

    if !manual {
        // 起動時チェック。既定では毎回確認する（設定で間隔を空けられる）。
        let interval = settings.update.check_interval_hours;
        if !config::should_check_now(interval) {
            log::info!("前回の確認から {interval} 時間経っていないため、更新確認を省略しました");
            return;
        }
        config::mark_checked();
    }

    let found = match check(&settings) {
        Ok(v) => v,
        Err(e) => {
            // 通信の失敗は起動を妨げない。手動確認のときだけ知らせる。
            log::warn!("更新の確認に失敗しました: {e}");
            if manual {
                let summary = format!(
                    "更新を確認できませんでした。インターネットに接続できないか、\
                     GitHub が混み合っている可能性があります。\n\n\
                     このページから最新版を確認することもできます。\n{}",
                    settings.update.releases_page
                );
                message_box(&with_detail(&summary, &e), MB_OK | MB_ICONERROR);
            }
            return;
        }
    };

    let Some(info) = found else {
        log::info!("最新版を使用しています");
        if manual {
            message_box(
                &format!(
                    "お使いのバージョン {} が最新です。",
                    Version::current()
                ),
                MB_OK | MB_ICONINFORMATION,
            );
        }
        return;
    };

    // 更新が見つかった。インストールしてよいか尋ねる。
    let answer = message_box(
        &format!(
            "新しいバージョン {} があります（現在 {}）。\n\n\
             今すぐ更新しますか？\n\
             更新後、アプリは自動で再起動します。",
            info.version,
            Version::current()
        ),
        MB_YESNO | MB_ICONQUESTION,
    );
    if answer != IDYES.0 {
        log::info!("利用者が更新を見送りました");
        return;
    }

    // ダウンロードと検証。
    let downloaded = match download_and_verify(&info, &tx) {
        Ok(p) => p,
        Err(e) => {
            log::error!("更新の取得に失敗しました: {e}");
            let summary = format!(
                "更新ファイルの取得に失敗しました。インターネットの接続状況をご確認のうえ、\
                 もう一度お試しください。\n\n\
                 このページから手動でダウンロードすることもできます。\n{}",
                info.page_url
            );
            message_box(&with_detail(&summary, &e), MB_OK | MB_ICONERROR);
            let _ = tx.send(TrayMessage::UpdateFinished);
            return;
        }
    };

    // 入れ替え。
    let installed = match install(&downloaded) {
        Ok(p) => p,
        Err(e) => {
            log::error!("更新の適用に失敗しました: {e}");
            let _ = std::fs::remove_file(&downloaded);
            // 自動で置き換えられない場合（書き込み権限が無いなど）は、
            // 手動で入れ替えられるようリリースページを開く。
            let summary = "更新を適用できませんでした。リリースページを開きますので、\
                            お手数ですが手動で置き換えてください。";
            message_box(&with_detail(summary, &e), MB_OK | MB_ICONERROR);
            open_in_browser(&info.page_url);
            let _ = tx.send(TrayMessage::UpdateFinished);
            return;
        }
    };
    let _ = std::fs::remove_file(&downloaded);

    // 新しい実行ファイルを起動して、自分は終了する。
    match std::process::Command::new(&installed).spawn() {
        Ok(_) => {
            log::info!("新しいバージョンを起動しました。終了します");
            let _ = tx.send(TrayMessage::Quit);
        }
        Err(e) => {
            log::error!("新しいバージョンの起動に失敗しました: {e}");
            let summary = "更新は完了しましたが、自動で再起動できませんでした。\n\
                            お手数ですが手動で起動し直してください。";
            message_box(&with_detail(summary, &e.to_string()), MB_OK | MB_ICONERROR);
            let _ = tx.send(TrayMessage::UpdateFinished);
        }
    }
}

/// 既定のブラウザで URL を開く。
fn open_in_browser(url: &str) {
    let url_w: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        // 戻り値は 32 以下がエラーだが、開けなくても致命的ではないので記録だけする。
        let result = ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(url_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if result.0 as isize <= 32 {
            log::warn!("ブラウザを開けませんでした: {url}");
        }
    }
}

/// 平易な一文（`summary`）の下に、元のエラーメッセージ（`detail`）を添えた
/// ダイアログ本文を作る。
///
/// serde や WinHTTP が返す英語まじりのメッセージをそのまま出すと利用者には
/// 意味が伝わらないため、まず日本語で状況を説明し、詳細はその下に残す
/// （原因の切り分けに役立つため、詳細自体を消しはしない）。
fn with_detail(summary: &str, detail: &str) -> String {
    format!("{summary}\n\n詳細: {detail}")
}

/// メッセージボックスを表示し、押されたボタンの ID を返す。
///
/// `main.rs` からも（初期化失敗時の通知に）呼べるよう `pub(crate)` にしている。
pub(crate) fn message_box(text: &str, style: MESSAGEBOX_STYLE) -> i32 {
    let text_w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let title_w: Vec<u16> = DIALOG_TITLE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            style,
        )
        .0
    }
}

#[cfg(test)]
mod tests {
    use super::{
        has_mz_signature, is_temp_update_file, needs_exe_restore, release, try_acquire,
        with_detail, Version,
    };
    use std::sync::atomic::AtomicBool;

    #[test]
    fn recognizes_own_temp_files() {
        assert!(is_temp_update_file("Atai-paste-1.2.1.exe"));
        assert!(is_temp_update_file("Atai-paste-10.0.0.exe"));
    }

    #[test]
    fn never_deletes_unrelated_files() {
        // 無関係なファイルを消してしまわないこと。特に本体の実行ファイル。
        assert!(!is_temp_update_file("Atai-paste.exe"));
        assert!(!is_temp_update_file("Atai-paste.exe.old"));
        assert!(!is_temp_update_file("important.exe"));
        assert!(!is_temp_update_file("Atai-paste-1.2.1.txt"));
        assert!(!is_temp_update_file("Atai-paste-.exe"));
        assert!(!is_temp_update_file("Atai-paste-メモ.exe"));
        assert!(!is_temp_update_file("XAtai-paste-1.0.0.exe"));
        assert!(!is_temp_update_file(""));
    }

    #[test]
    fn parses_common_forms() {
        assert_eq!(Version::parse("v1.2.3"), Version::parse("1.2.3"));
        assert!(Version::parse("1.2").is_some());
        assert!(Version::parse("1.2.3-beta").is_some());
        assert!(Version::parse("なし").is_none());
    }

    #[test]
    fn compares_numerically_not_lexically() {
        // 文字列比較では "1.0.10" < "1.0.9" と誤判定してしまう組み合わせ。
        let a = Version::parse("1.0.10").unwrap();
        let b = Version::parse("1.0.9").unwrap();
        assert!(a > b);

        let c = Version::parse("1.10.0").unwrap();
        let d = Version::parse("1.9.0").unwrap();
        assert!(c > d);
    }

    #[test]
    fn equal_versions_are_not_newer() {
        let a = Version::parse("v1.1.0").unwrap();
        let b = Version::parse("1.1.0").unwrap();
        assert!(a <= b);
    }

    #[test]
    fn try_acquire_blocks_concurrent_run() {
        // 実際の UPDATE_RUNNING は他のテストと共有される static なので、
        // ロジックの検証にはテスト専用のフラグを使う。
        let flag = AtomicBool::new(false);
        assert!(try_acquire(&flag), "1 回目は取得できる");
        assert!(!try_acquire(&flag), "実行中は 2 回目を取得できない");
        release(&flag);
        assert!(try_acquire(&flag), "release 後は再取得できる");
    }

    #[test]
    fn needs_exe_restore_only_when_exe_missing_and_old_present() {
        // install() の 2 と 3 の間で中断された状態だけを復元対象とする。
        assert!(needs_exe_restore(false, true));
        assert!(!needs_exe_restore(true, true));
        assert!(!needs_exe_restore(true, false));
        assert!(!needs_exe_restore(false, false));
    }

    #[test]
    fn mz_signature_detects_pe_executables() {
        assert!(has_mz_signature(b"MZ\x90\x00\x03"));
        assert!(!has_mz_signature(b"PK\x03\x04")); // ZIP など別形式
        assert!(!has_mz_signature(b"M")); // 1 バイトしかない
        assert!(!has_mz_signature(b""));
    }

    #[test]
    fn with_detail_keeps_both_summary_and_detail() {
        let text = with_detail("要約です。", "detail message");
        assert!(text.contains("要約です。"));
        assert!(text.contains("詳細: detail message"));
    }
}
