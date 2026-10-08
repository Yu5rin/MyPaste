//! クリップボードの履歴をファイルに残す（アプリを終了しても履歴を残すとき）。
//!
//! 履歴にはコピーした文字がそのまま入るので、Windows のデータ保護 API（DPAPI。
//! `CryptProtectData`）で暗号化して保存する。暗号化した中身は、**この PC にサインインしている
//! 同じユーザー**でなければ元に戻せない。ほかのユーザーや、ファイルを別の PC へ持ち出した場合は
//! 読めない（その場合は空の履歴から始める）。
//!
//! 中身の形（JSON の文字列の配列）は [`crate::clip_history`] にある。

use windows::core::w;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

use crate::{clip_history, config};

/// 保存しておいた履歴（新しい順）を読む。ファイルが無いときや、別のユーザー・別の PC で
/// 保存したもので元に戻せないときは空。ファイルがあるのに読めない（ほかのアプリが使っている など）
/// ときは `Err`（呼び出し側は、その間は上書きしない）。
pub fn load() -> Result<clip_history::Saved, String> {
    let Some(path) = config::clip_history_file() else {
        return Ok(clip_history::Saved::default());
    };
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(clip_history::Saved::default())
        }
        Err(e) => return Err(e.to_string()),
    };
    match unprotect(&data) {
        Some(plain) => Ok(clip_history::from_json(&String::from_utf8_lossy(&plain))),
        None => {
            // 別のユーザーや別の PC で保存したもの。読めないので使わない（次の保存で置き換わる）。
            log::warn!("クリップボードの履歴を元に戻せませんでした（別のユーザー・PC で保存したもの）");
            Ok(clip_history::Saved::default())
        }
    }
}

/// 履歴（新しい順）を暗号化して保存する。
pub fn save(saved: &clip_history::Saved) -> Result<(), String> {
    let path = config::clip_history_file().ok_or("履歴の保存先を決められません")?;
    let data = protect(clip_history::to_json(saved).as_bytes())
        .ok_or("履歴を暗号化できませんでした")?;
    config::write_file_safely(&path, &data).map_err(|e| e.to_string())
}

/// 保存した履歴を消す（履歴を残さない設定にしたとき・履歴を消したとき）。
pub fn delete() {
    if let Some(path) = config::clip_history_file() {
        match std::fs::remove_file(&path) {
            Ok(()) => log::debug!("保存していたクリップボードの履歴を消しました"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("保存していたクリップボードの履歴を消せませんでした: {e}"),
        }
    }
}

/// DPAPI で暗号化する（今のユーザーだけが元に戻せる）。
fn protect(plain: &[u8]) -> Option<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(plain.len()).ok()?,
        pbData: plain.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            w!("Atai-paste clipboard history"),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .ok()?;
        Some(take_blob(output))
    }
}

/// DPAPI で暗号化したものを元に戻す。
fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).ok()?,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .ok()?;
        Some(take_blob(output))
    }
}

/// DPAPI が返したメモリを写し取り、解放する。
unsafe fn take_blob(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if blob.pbData.is_null() {
        return Vec::new();
    }
    let bytes = std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec();
    let _ = LocalFree(HLOCAL(blob.pbData.cast()));
    bytes
}
