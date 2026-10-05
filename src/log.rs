//! 動作の記録（ログ）。
//!
//! - **トラブル調査用**: 設定画面の「動作の記録（log.txt）を残す」を ON にすると、
//!   製品版でも `log.txt` に記録する（[`set_enabled`]。再起動は不要）。既定は OFF。
//! - **開発時**: デバッグビルド（`debug_assertions`）または `--features devlog` を付けた
//!   ビルドでは、設定にかかわらず常に記録する。デバッグビルドではコンソールにも出す。
//!
//! `log.txt` は `settings.json` と同じフォルダ（実行ファイルのフォルダ、書き込めなければ
//! `%LOCALAPPDATA%\Atai-paste`）に置く。1 MB を超えたら `log.old.txt` に回して作り直すので、
//! 際限なく大きくはならない。
//!
//! 記録するのはアプリの動作（起動・設定・リマップした・入力モードを表示した、など）だけで、
//! 入力した文字は記録しない（キーボードフックが記録するのは、設定したキーの組み合わせを
//! 押したときだけ）。
//!
//! 外部のクレートを使わず、`log` クレートの [`log::Log`] を自前で実装している
//! （記録の ON/OFF を起動中に切り替えるため）。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::config;

/// これを超えたら古い記録へ回す大きさ（バイト）。
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// 開発用のビルドか（常に記録する）。
const ALWAYS_ON: bool = cfg!(any(debug_assertions, feature = "devlog"));

/// 記録するか（設定画面の ON/OFF）。
static ENABLED: AtomicBool = AtomicBool::new(ALWAYS_ON);

/// 書き込み先（開いたファイルと、これまでの大きさ）。最初の記録のときに開く。
static WRITER: Mutex<Option<(File, u64)>> = Mutex::new(None);

static LOGGER: FileLogger = FileLogger;

struct FileLogger;

/// ロガーを登録する。プロセス起動時に一度だけ呼び出す。
pub fn init() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(LevelFilter::Debug);
    }
}

/// 記録の ON/OFF を切り替える（起動時と、設定画面で保存したとき）。
/// 開発用のビルドでは常に ON のまま。
pub fn set_enabled(enabled: bool) {
    let enabled = enabled || ALWAYS_ON;
    let was = ENABLED.swap(enabled, Ordering::SeqCst);
    if was && !enabled {
        // 記録をやめたらファイルを閉じる（利用者が消したり開いたりしやすいように）。
        if let Ok(mut writer) = WRITER.lock() {
            *writer = None;
        }
    }
}

/// いま記録しているか。
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// 記録のファイルのパス（設定画面の「記録を開く」で使う）。
pub fn log_file() -> Option<PathBuf> {
    config::log_file()
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        is_enabled() && metadata.level() <= Level::Debug
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} [{}] {}\r\n",
            timestamp(),
            record.level(),
            record.args()
        );

        #[cfg(debug_assertions)]
        eprint!("{line}");

        let Ok(mut writer) = WRITER.lock() else {
            return;
        };
        if writer.is_none() {
            *writer = open_log();
        }
        let Some((file, written)) = writer.as_mut() else {
            return;
        };
        if file.write_all(line.as_bytes()).is_err() {
            // 書けなくなった（消された・別のアプリが握っている等）。次の記録で開き直す。
            *writer = None;
            return;
        }
        *written += line.len() as u64;
        if *written > MAX_LOG_BYTES {
            // 次の記録のときに、古い記録へ回して作り直す。
            *writer = None;
        }
    }

    fn flush(&self) {
        if let Ok(mut writer) = WRITER.lock() {
            if let Some((file, _)) = writer.as_mut() {
                let _ = file.flush();
            }
        }
    }
}

/// 記録のファイルを開く。大きくなっていたら `log.old.txt` に回してから新しく作る。
fn open_log() -> Option<(File, u64)> {
    let path = config::log_file()?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let size = if size > MAX_LOG_BYTES {
        let _ = std::fs::rename(&path, path.with_file_name("log.old.txt"));
        0
    } else {
        size
    };
    let mut file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
    if size == 0 {
        // 新しいファイルには UTF-8 の BOM を付ける。古いメモ帳などで開いても
        // 日本語が文字化けしないように。
        let _ = file.write_all("\u{feff}".as_bytes());
    }
    Some((file, size))
}

/// 記録に付ける日時（PC の時刻）。例: `2026-10-05 12:34:56.789`
fn timestamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}
