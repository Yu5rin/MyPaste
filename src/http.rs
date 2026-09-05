//! HTTPS 通信（Windows 標準の WinHTTP を使用）。
//!
//! 更新確認とダウンロードのためだけに使う、最小限の GET クライアント。
//!
//! WinHTTP を選んだ理由:
//! - **証明書の検証を OS に任せられる**（自前で証明書ストアを抱えなくてよい）
//! - 追加のクレートが不要で、C やアセンブリのビルドを伴わない
//! - システムのプロキシ設定に自動で従う
//!
//! ハンドルは [`Handle`] で RAII 管理し、途中で失敗しても確実に閉じる。

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_OPEN_REQUEST_FLAGS,
};

/// User-Agent（GitHub API は User-Agent が無いと 403 を返す）。
const USER_AGENT: &str = "Atai-paste-updater";

/// ヘッダ問い合わせ: ステータスコード。
const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
/// ヘッダ問い合わせ: Content-Length。
const WINHTTP_QUERY_CONTENT_LENGTH: u32 = 5;
/// ヘッダ問い合わせ結果を数値として受け取るフラグ。
const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

/// ダウンロードするファイルの上限（100MB）。
/// 想定外に巨大な応答でメモリを食い潰さないための保険。
const MAX_BODY_BYTES: u64 = 100 * 1024 * 1024;

/// WinHTTP ハンドルの RAII ラッパ。
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// URL を分解した結果。
struct CrackedUrl {
    host: Vec<u16>,
    /// パスとクエリ文字列を結合したもの。
    path: Vec<u16>,
    port: u16,
    secure: bool,
}

/// 文字列を UTF-16 のヌル終端バッファへ変換する。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// URL を分解する。
fn crack_url(url: &str) -> Result<CrackedUrl, String> {
    // WinHttpCrackUrl はヌル終端を含めない長さを期待するため、終端なしで渡す。
    let url_w: Vec<u16> = url.encode_utf16().collect();

    let mut comp = URL_COMPONENTS {
        dwStructSize: std::mem::size_of::<URL_COMPONENTS>() as u32,
        ..Default::default()
    };
    // 長さに -1 を入れると、各要素が元の文字列内を指すポインタとして返る。
    comp.dwSchemeLength = u32::MAX;
    comp.dwHostNameLength = u32::MAX;
    comp.dwUrlPathLength = u32::MAX;
    comp.dwExtraInfoLength = u32::MAX;

    unsafe {
        WinHttpCrackUrl(&url_w, 0, &mut comp).map_err(|e| format!("URL の解析に失敗: {e}"))?;
    }

    if comp.lpszHostName.is_null() || comp.dwHostNameLength == 0 {
        return Err("URL にホスト名がありません".to_string());
    }

    // 返されたポインタは url_w の内部を指すので、ここでコピーしておく。
    let host_slice =
        unsafe { std::slice::from_raw_parts(comp.lpszHostName.0, comp.dwHostNameLength as usize) };
    let host: Vec<u16> = host_slice.iter().copied().chain(std::iter::once(0)).collect();

    // パスとクエリ（extra info）を結合する。
    let mut path: Vec<u16> = Vec::new();
    if !comp.lpszUrlPath.is_null() && comp.dwUrlPathLength > 0 {
        let s = unsafe {
            std::slice::from_raw_parts(comp.lpszUrlPath.0, comp.dwUrlPathLength as usize)
        };
        path.extend_from_slice(s);
    }
    if !comp.lpszExtraInfo.is_null() && comp.dwExtraInfoLength > 0 {
        let s = unsafe {
            std::slice::from_raw_parts(comp.lpszExtraInfo.0, comp.dwExtraInfoLength as usize)
        };
        path.extend_from_slice(s);
    }
    if path.is_empty() {
        path.push(u16::from(b'/'));
    }
    path.push(0);

    // nScheme が INTERNET_SCHEME_HTTPS(2) なら TLS。ポート 443 も同様に扱う。
    let secure = comp.nScheme.0 == 2 || comp.nPort == 443;
    if !secure {
        return Err("HTTPS 以外の URL は許可していません".to_string());
    }

    Ok(CrackedUrl {
        host,
        path,
        port: comp.nPort,
        secure,
    })
}

/// GET リクエストを送り、本文をすべて読み取る。
///
/// - `accept`: `Accept` ヘッダに入れる値（GitHub API では
///   `application/vnd.github+json` を指定する）
/// - `progress`: 受信バイト数と全体サイズ（判れば）を受け取るコールバック
pub fn get(
    url: &str,
    accept: Option<&str>,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<Vec<u8>, String> {
    let parts = crack_url(url)?;

    unsafe {
        // --- セッション ---
        let session = Handle(WinHttpOpen(
            PCWSTR(wide(USER_AGENT).as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err("WinHttpOpen に失敗しました".to_string());
        }

        // --- 接続 ---
        let connect = Handle(WinHttpConnect(
            session.0,
            PCWSTR(parts.host.as_ptr()),
            parts.port,
            0,
        ));
        if connect.0.is_null() {
            return Err("接続に失敗しました".to_string());
        }

        // --- リクエスト ---
        let flags = if parts.secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            PCWSTR(wide("GET").as_ptr()),
            PCWSTR(parts.path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        ));
        if request.0.is_null() {
            return Err("リクエストの作成に失敗しました".to_string());
        }

        // --- 送信 ---
        // GitHub API 向けに Accept ヘッダを付ける。
        let headers: Option<Vec<u16>> =
            accept.map(|a| format!("Accept: {a}\r\n").encode_utf16().collect());
        let header_slice = headers.as_deref();

        WinHttpSendRequest(request.0, header_slice, None, 0, 0, 0)
            .map_err(|e| format!("送信に失敗: {e}"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())
            .map_err(|e| format!("応答の受信に失敗: {e}"))?;

        // --- ステータスコード ---
        let mut status: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(|e| format!("ステータスコードの取得に失敗: {e}"))?;

        if status != 200 {
            return Err(format!("サーバーが HTTP {status} を返しました"));
        }

        // --- Content-Length（進捗表示に使う。無くてもよい） ---
        let mut content_length: u32 = 0;
        let mut cl_size = std::mem::size_of::<u32>() as u32;
        let total = WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut content_length as *mut u32 as *mut c_void),
            &mut cl_size,
            std::ptr::null_mut(),
        )
        .ok()
        .map(|()| u64::from(content_length));

        if let Some(t) = total {
            if t > MAX_BODY_BYTES {
                return Err(format!("応答が大きすぎます（{t} バイト）"));
            }
        }

        // --- 本文の読み取り ---
        let mut body: Vec<u8> = Vec::with_capacity(total.unwrap_or(64 * 1024) as usize);
        loop {
            let mut available: u32 = 0;
            WinHttpQueryDataAvailable(request.0, &mut available)
                .map_err(|e| format!("受信サイズの取得に失敗: {e}"))?;
            if available == 0 {
                break;
            }

            let mut chunk = vec![0u8; available as usize];
            let mut read: u32 = 0;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr() as *mut c_void,
                available,
                &mut read,
            )
            .map_err(|e| format!("受信に失敗: {e}"))?;
            if read == 0 {
                break;
            }
            chunk.truncate(read as usize);
            body.extend_from_slice(&chunk);

            if body.len() as u64 > MAX_BODY_BYTES {
                return Err("応答が大きすぎます".to_string());
            }
            progress(body.len() as u64, total);
        }

        // Content-Length が判っている場合、受信し終えたバイト数と一致するか確かめる。
        // 応答が途中で切れても WinHttpQueryDataAvailable が 0 を返して
        // ループを抜けてしまうため、ここで突き合わせないと切断を見逃す。
        if let Some(t) = total {
            let received = body.len() as u64;
            if received != t {
                return Err(format!(
                    "応答が途中で切断されました（期待 {t} バイト / 受信 {received} バイト）"
                ));
            }
        }

        Ok(body)
    }
}
