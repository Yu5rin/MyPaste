//! 対象アプリの判定（既定は Excel）。
//!
//! フォアグラウンドウィンドウのプロセスイメージ名が、設定した対象アプリ
//! （既定は `EXCEL.EXE`）のいずれかの場合にのみ `true` を返す。判定は以下の
//! Win32 API を使う:
//!
//! - `GetForegroundWindow` … 最前面ウィンドウのハンドル
//! - `GetWindowThreadProcessId` … そのウィンドウのプロセス ID
//! - `OpenProcess` + `QueryFullProcessImageNameW` … 実行ファイルのフルパス

use windows::core::PWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use crate::remap_logic;

/// 最前面のウィンドウが対象アプリのいずれかかを返す（大文字小文字は区別しない）。
pub fn is_target_foreground(apps: &[String]) -> bool {
    if apps.is_empty() {
        return false;
    }
    foreground_image_name().is_some_and(|name| remap_logic::is_target_app(&name, apps))
}

/// 最前面のウィンドウのプロセスのファイル名（例 `EXCEL.EXE`）。取れなければ `None`。
fn foreground_image_name() -> Option<String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }

        // 名前取得に必要な最小限の権限だけを要求する。
        let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            Err(_) => return None,
        };

        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);

        if result.is_err() {
            return None;
        }

        let full_path = String::from_utf16_lossy(&buf[..len as usize]);
        // パス区切りで分割して末尾のファイル名だけを取り出す。
        let file_name = full_path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(full_path.as_str());

        Some(file_name.to_string())
    }
}
