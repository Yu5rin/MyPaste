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
//! 1. `Atai-paste.exe` → `Atai-paste.exe.old` にリネーム（実行中でも可能）
//! 2. 新しい `Atai-paste.exe` を配置
//! 3. 新しい EXE を起動して、自分は終了する
//! 4. 次回起動時に `.old` を削除する（[`cleanup_old`]）
//!
//! 2 で失敗した場合は 1 を巻き戻す（[`install`] のロールバック）。アプリが壊れた状態で
//! 残らないようにするため、この巻き戻しは省略しない。

use std::path::{Path, PathBuf};
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

    if let Some(expected) = &info.sha256 {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual = hex(&hasher.finalize());
        if &actual != expected {
            return Err(format!(
                "ダウンロードしたファイルの検証に失敗しました。\n期待: {expected}\n実際: {actual}"
            ));
        }
        log::info!("SHA256 検証に成功しました");
    } else {
        log::warn!("リリースに SHA256 が無いため検証を省略しました");
    }

    // 一時フォルダへ書き出す。
    let path = std::env::temp_dir().join(format!("Atai-paste-{}.exe", info.version));
    std::fs::write(&path, &bytes).map_err(|e| format!("一時ファイルの書き出しに失敗: {e}"))?;
    Ok(path)
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

/// ダウンロード済みの実行ファイルを、現在の実行ファイルと入れ替える。
///
/// 失敗した場合はリネームを巻き戻すため、アプリが壊れた状態にはならない。
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
    // 前回の残骸があれば先に消しておく（残っていてもリネームは失敗する）。
    let _ = std::fs::remove_file(&old);

    // 1) 実行中の EXE をリネーム（実行中でも可能）
    std::fs::rename(&current, &old).map_err(|e| format!("現在の実行ファイルを退避できません: {e}"))?;

    // 2) 新しい EXE を配置。失敗したらリネームを巻き戻す。
    if let Err(e) = std::fs::copy(new_exe, &current) {
        let _ = std::fs::rename(&old, &current);
        return Err(format!("新しい実行ファイルを配置できません（元に戻しました）: {e}"));
    }

    log::info!("更新を配置しました: {}", current.display());
    Ok(current)
}

/// 前回の更新で残った `.old` を削除する（起動時に呼ぶ）。
pub fn cleanup_old() {
    let Ok(current) = std::env::current_exe() else {
        return;
    };
    let old = old_path(&current);
    if old.exists() {
        match std::fs::remove_file(&old) {
            Ok(()) => log::info!("前回の更新の残骸を削除しました"),
            // まだロックされている場合もある。次回の起動で消えるので無視してよい。
            Err(e) => log::warn!("残骸の削除に失敗しました（次回起動時に再試行）: {e}"),
        }
    }
}

/// 更新を確認し、必要ならインストールまで行う。別スレッドから呼ぶこと。
///
/// - `manual` が `true`（メニューからの手動確認）のときは、最新である場合や
///   失敗した場合にもダイアログで結果を知らせる。
/// - `manual` が `false`（起動時の自動確認）のときは、更新が見つかったときだけ
///   利用者に尋ね、それ以外は黙ってログに残す。
pub fn run(settings: Settings, tx: Sender<TrayMessage>, manual: bool) {
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
                message_box(
                    &format!("更新を確認できませんでした。\n\n{e}"),
                    MB_OK | MB_ICONERROR,
                );
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
            message_box(
                &format!("更新の取得に失敗しました。\n\n{e}"),
                MB_OK | MB_ICONERROR,
            );
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
            message_box(
                &format!("更新を適用できませんでした。\n\n{e}\n\nリリースページを開きます。"),
                MB_OK | MB_ICONERROR,
            );
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
            message_box(
                &format!(
                    "更新は完了しましたが、自動で再起動できませんでした。\n\
                     お手数ですが手動で起動し直してください。\n\n{e}"
                ),
                MB_OK | MB_ICONERROR,
            );
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

/// メッセージボックスを表示し、押されたボタンの ID を返す。
fn message_box(text: &str, style: MESSAGEBOX_STYLE) -> i32 {
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
    use super::Version;

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
}
