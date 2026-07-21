//! Excel 判定。
//!
//! フォアグラウンドウィンドウのプロセスイメージ名が `EXCEL.EXE` の場合にのみ
//! `true` を返す。判定は以下の Win32 API を使う:
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

/// 比較対象のイメージ名（大文字小文字は無視して比較する）。
const EXCEL_IMAGE_NAME: &str = "EXCEL.EXE";

/// 最前面のウィンドウが Excel（EXCEL.EXE）かどうかを返す。
pub fn is_excel_foreground() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return false;
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return false;
        }

        // 名前取得に必要な最小限の権限だけを要求する。
        let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            Err(_) => return false,
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
            return false;
        }

        let full_path = String::from_utf16_lossy(&buf[..len as usize]);
        // パス区切りで分割して末尾のファイル名だけを取り出す。
        let file_name = full_path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(full_path.as_str());

        file_name.eq_ignore_ascii_case(EXCEL_IMAGE_NAME)
    }
}
