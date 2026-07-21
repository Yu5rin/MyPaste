//! 自動起動設定。
//!
//! Windows のスタートアップフォルダに本アプリのショートカット（.lnk）を
//! 作成／削除することで、ログオン時の自動起動を切り替える。
//! ショートカットの作成には COM の `IShellLinkW` / `IPersistFile` を使う。

use std::path::{Path, PathBuf};

use windows::core::{Interface, Result, HRESULT, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IPersistFile,
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Startup, IShellLinkW, SHGetKnownFolderPath, ShellLink, KF_FLAG_DEFAULT,
};

/// スタートアップフォルダに置くショートカットのファイル名。
const LNK_NAME: &str = "アタイの貼り付け.lnk";

/// スタートアップフォルダのパスを取得する。
fn startup_dir() -> Result<PathBuf> {
    unsafe {
        let pwstr = SHGetKnownFolderPath(&FOLDERID_Startup, KF_FLAG_DEFAULT, None)?;
        let dir = pwstr.to_string().unwrap_or_default();
        // SHGetKnownFolderPath が確保したメモリを解放する。
        CoTaskMemFree(Some(pwstr.0 as *const core::ffi::c_void));
        Ok(PathBuf::from(dir))
    }
}

/// ショートカットのフルパス。
fn lnk_path() -> Result<PathBuf> {
    Ok(startup_dir()?.join(LNK_NAME))
}

/// 自動起動が有効（ショートカットが存在する）か。
pub fn is_enabled() -> bool {
    lnk_path().map(|p| p.exists()).unwrap_or(false)
}

/// 自動起動を有効にする（ショートカットを作成）。
pub fn enable() -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| windows::core::Error::new(HRESULT(-1), format!("current_exe: {e}")))?;
    let target = lnk_path()?;
    create_shortcut(&exe, &target)
}

/// 自動起動を無効にする（ショートカットを削除）。
pub fn disable() -> Result<()> {
    if let Ok(path) = lnk_path() {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// 自動起動を反転し、反転後の状態（true = 有効）を返す。
pub fn toggle() -> Result<bool> {
    if is_enabled() {
        disable()?;
        Ok(false)
    } else {
        enable()?;
        Ok(true)
    }
}

/// COM を使って .lnk ショートカットを作成する。
fn create_shortcut(target: &Path, lnk: &Path) -> Result<()> {
    unsafe {
        // このスレッド用に COM を初期化（既に初期化済みでも害はない）。
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let result = (|| -> Result<()> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;

            let target_w = to_hstring(target);
            link.SetPath(PCWSTR(target_w.as_ptr()))?;

            if let Some(parent) = target.parent() {
                let dir_w = to_hstring(parent);
                link.SetWorkingDirectory(PCWSTR(dir_w.as_ptr()))?;
            }

            let desc_w = wide("Excel で Ctrl+B を Ctrl+Shift+V にリマップ");
            link.SetDescription(PCWSTR(desc_w.as_ptr()))?;

            let persist: IPersistFile = link.cast()?;
            let lnk_w = to_hstring(lnk);
            persist.Save(PCWSTR(lnk_w.as_ptr()), true)?;
            Ok(())
        })();

        CoUninitialize();
        result
    }
}

/// パスを UTF-16 のヌル終端バッファへ変換する。
fn to_hstring(path: &Path) -> Vec<u16> {
    wide(&path.to_string_lossy())
}

/// 文字列を UTF-16 のヌル終端バッファへ変換する。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
