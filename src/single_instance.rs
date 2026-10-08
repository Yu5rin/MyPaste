//! 二重起動の防止。
//!
//! 名前付きミューテックスを持ち、すでに同じアプリが動いていれば、何も表示せずに終わる
//! （トレイアイコン・キーボードフック・入力モード表示が 2 つずつ動かないように）。
//!
//! 自動更新では、新しい実行ファイルを起動してから古い方が終わる。そのため更新後の起動
//! （[`AFTER_UPDATE_ARG`] 付き）では、古い方が終わってミューテックスを手放すまで待つ。

use windows::core::w;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};

/// 自動更新で新しい版を起動するときに付ける引数。
pub const AFTER_UPDATE_ARG: &str = "--after-update";

/// 更新後の起動で、古い版が終わるのを待つ上限（ミリ秒）。古い版は更新の入れ替えを
/// 終えてから終了するので、通常はすぐに手放される。
const AFTER_UPDATE_WAIT_MS: u32 = 30_000;

/// 起動している間持ち続けるミューテックス。プロセスが終わると自動で手放される。
pub struct Guard(HANDLE);

impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 自分だけが動くようにする。すでに動いている場合は `None`（呼び出し側はそのまま終わる）。
///
/// ミューテックスはサインインしているユーザーごと（`Local\`）。別のユーザーが同じ PC で
/// 使っていても、それぞれ起動できる。
pub fn acquire() -> Option<Guard> {
    let after_update = std::env::args().any(|a| a == AFTER_UPDATE_ARG);
    unsafe {
        let handle = match CreateMutexW(None, true, w!("Local\\AtaiPaste.SingleInstance")) {
            Ok(handle) => handle,
            Err(e) => {
                // 作れない（まず無い）場合は、防止せずに起動を続ける。
                log::warn!("二重起動の確認ができませんでした: {e}");
                return Some(Guard(HANDLE::default()));
            }
        };
        if GetLastError() != ERROR_ALREADY_EXISTS {
            return Some(Guard(handle));
        }
        if after_update {
            // 古い版が終わって手放すのを待つ（終わり方によっては「放棄」扱いになる）。
            let waited = WaitForSingleObject(handle, AFTER_UPDATE_WAIT_MS);
            if waited == WAIT_OBJECT_0 || waited == WAIT_ABANDONED {
                return Some(Guard(handle));
            }
        }
        log::info!("すでに起動しているため、終了します");
        let _ = CloseHandle(handle);
        None
    }
}
