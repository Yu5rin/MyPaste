//! 開発時ログ出力。
//!
//! 仕様: ログは開発時のみ `log.txt` へ出力し、本番ビルドでは無効化する。
//!
//! - デバッグビルド（`debug_assertions`）または `--features devlog` を付けたビルドでのみ、
//!   実行ファイルと同じフォルダに `log.txt` を作成して追記する。
//! - それ以外（本番リリースビルド）ではロガーを初期化しないため、
//!   `log::info!` などの各マクロは何も出力しない（実質無効）。

/// ロガーを初期化する。プロセス起動時に一度だけ呼び出す。
pub fn init() {
    #[cfg(any(debug_assertions, feature = "devlog"))]
    {
        use simplelog::{ColorChoice, CombinedLogger, Config, LevelFilter, TermLogger, TerminalMode, WriteLogger};
        use std::fs::OpenOptions;

        // log.txt は実行ファイルと同じディレクトリに置く。
        let path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("log.txt")))
            .unwrap_or_else(|| std::path::PathBuf::from("log.txt"));

        let mut loggers: Vec<Box<dyn simplelog::SharedLogger>> = Vec::new();

        if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
            loggers.push(WriteLogger::new(LevelFilter::Debug, Config::default(), file));
        }

        // デバッグビルドではコンソールにも出す（release+devlog ではファイルのみ）。
        #[cfg(debug_assertions)]
        loggers.push(TermLogger::new(
            LevelFilter::Debug,
            Config::default(),
            TerminalMode::Mixed,
            ColorChoice::Auto,
        ));

        let _ = CombinedLogger::init(loggers);
    }

    // 本番ビルド（release かつ devlog 無効）ではロガー未初期化 = 無出力。
}
