//! クリップボードの履歴に入れる画像を、ファイルに置く・読む・描く。
//!
//! 画像は 1 枚ずつ、圧縮してから Windows のデータ保護 API（DPAPI）で暗号化し、
//! `data\clip_images\` に置く（文字の履歴と同じく、この PC にサインインしている同じユーザー
//! でなければ元に戻せない）。
//!
//! - `index.dat`: 目録（[`clip_images::Index`]）
//! - `<番号>.img`: 画像そのもの（クリップボードの DIB をそのまま）
//! - `<番号>.thm`: 一覧に出す縮小した絵
//!
//! 件数や容量の上限などの判断は [`crate::clip_images`] にある。

use std::path::PathBuf;

use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    SetBrushOrgEx, SetStretchBltMode, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HALFTONE, HDC, SRCCOPY,
};

use crate::clip_images::{self, DibInfo, Index, Thumb};
use crate::{clip_store, config};

const INDEX_FILE: &str = "index.dat";

fn dir() -> Option<PathBuf> {
    config::clip_images_dir()
}

fn image_path(id: u64) -> Option<PathBuf> {
    dir().map(|d| d.join(format!("{id}.img")))
}

fn thumb_path(id: u64) -> Option<PathBuf> {
    dir().map(|d| d.join(format!("{id}.thm")))
}

/// 目録を読む。無ければ空。読めない・元に戻せないときは `Err`。
pub fn load_index() -> Result<Index, String> {
    let Some(path) = dir().map(|d| d.join(INDEX_FILE)) else {
        return Ok(Index::default());
    };
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Index::default()),
        Err(e) => return Err(e.to_string()),
    };
    let plain = clip_store::unprotect(&data)
        .ok_or("元に戻せません（別のユーザー・PC で保存したもの）")?;
    Index::from_json(&String::from_utf8_lossy(&plain))
}

/// 目録を保存する。
pub fn save_index(index: &Index) -> Result<(), String> {
    let dir = dir().ok_or("画像の保存先を決められません")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let data = clip_store::protect(index.to_json().as_bytes()).ok_or("暗号化できませんでした")?;
    config::write_file_safely(&dir.join(INDEX_FILE), &data).map_err(|e| e.to_string())
}

/// 画像（クリップボードの DIB）を保存する。保存した大きさ（バイト数）を返す。
pub fn save_image(id: u64, dib: &[u8]) -> Result<u64, String> {
    let dir = dir().ok_or("画像の保存先を決められません")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let data = clip_store::protect(&clip_images::pack(dib)).ok_or("暗号化できませんでした")?;
    let path = dir.join(format!("{id}.img"));
    std::fs::write(&path, &data).map_err(|e| {
        let _ = std::fs::remove_file(&path);
        e.to_string()
    })?;
    Ok(data.len() as u64)
}

/// 縮小した絵を保存する（保存できなくても、一覧に絵が出ないだけ）。
pub fn save_thumb(id: u64, thumb: &Thumb) {
    let Some(path) = thumb_path(id) else {
        return;
    };
    match clip_store::protect(&clip_images::pack(&thumb.to_bytes())) {
        Some(data) => {
            if let Err(e) = std::fs::write(&path, data) {
                log::warn!("画像の履歴: 縮小した絵を保存できませんでした: {e}");
            }
        }
        None => log::warn!("画像の履歴: 縮小した絵を暗号化できませんでした"),
    }
}

/// 画像（DIB）を読む。
pub fn load_image(id: u64) -> Option<Vec<u8>> {
    let data = std::fs::read(image_path(id)?).ok()?;
    let packed = clip_store::unprotect(&data)?;
    clip_images::unpack(&packed, clip_images::MAX_IMAGE_BYTES)
}

/// 縮小した絵を読む。
pub fn load_thumb(id: u64) -> Option<Thumb> {
    let data = std::fs::read(thumb_path(id)?).ok()?;
    let packed = clip_store::unprotect(&data)?;
    Thumb::from_bytes(&clip_images::unpack(&packed, clip_images::THUMB_MAX_BYTES)?)
}

/// 画像のファイルを消す。
pub fn delete(ids: &[u64]) {
    for id in ids {
        for path in [image_path(*id), thumb_path(*id)].into_iter().flatten() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// 保存した画像をすべて消す（目録も）。
pub fn delete_all() {
    sweep(&[]);
    if let Some(dir) = dir() {
        let _ = std::fs::remove_file(dir.join(INDEX_FILE));
        // 空になっていれば、フォルダーも消す。
        let _ = std::fs::remove_dir(dir);
    }
}

/// 目録に無い画像のファイルを消す（途中で終了して残ったものなど）。
pub fn sweep(keep: &[u64]) {
    let Some(dir) = dir() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_ours = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "img" || e == "thm");
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok());
        if is_ours && id.is_some_and(|id| !keep.contains(&id)) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// 上から下へ並ぶ 32 ビットの絵の見出し。
fn top_down_header(width: u32, height: u32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// DIB から、縮小した絵を作る（色の深さや並びの違いは Windows に任せる）。
pub unsafe fn make_thumb(dib: &[u8], info: DibInfo) -> Option<Thumb> {
    let (width, height) = clip_images::thumb_size(
        info.width,
        info.height,
        clip_images::THUMB_MAX_W,
        clip_images::THUMB_MAX_H,
    );
    let screen = GetDC(HWND::default());
    let memory = CreateCompatibleDC(screen);
    ReleaseDC(HWND::default(), screen);
    if memory.is_invalid() {
        return None;
    }
    let header = top_down_header(width, height);
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let bitmap = CreateDIBSection(memory, &header, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0);
    let result = match bitmap {
        Ok(bitmap) if !bits.is_null() => {
            let old = SelectObject(memory, bitmap);
            SetStretchBltMode(memory, HALFTONE);
            let _ = SetBrushOrgEx(memory, 0, 0, None);
            let drawn = StretchDIBits(
                memory,
                0,
                0,
                width as i32,
                height as i32,
                0,
                0,
                info.width as i32,
                info.height as i32,
                Some(dib.as_ptr().add(info.bits_offset).cast()),
                dib.as_ptr().cast(),
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            let pixels = (drawn != 0).then(|| {
                std::slice::from_raw_parts(bits as *const u8, width as usize * height as usize * 4)
                    .to_vec()
            });
            SelectObject(memory, old);
            let _ = DeleteObject(bitmap);
            pixels.map(|pixels| Thumb {
                width,
                height,
                pixels,
            })
        }
        Ok(bitmap) => {
            let _ = DeleteObject(bitmap);
            None
        }
        Err(_) => None,
    };
    let _ = DeleteDC(memory);
    result
}

/// 縮小した絵を、枠 `(x, y, w, h)` の中央に、縦横の比を保って描く。
pub unsafe fn draw_thumb(hdc: HDC, thumb: &Thumb, x: i32, y: i32, w: i32, h: i32) {
    if w <= 0 || h <= 0 {
        return;
    }
    let (dx, dy, dw, dh) = clip_images::fit_rect(thumb.width, thumb.height, w, h);
    let header = top_down_header(thumb.width, thumb.height);
    let old_mode = SetStretchBltMode(hdc, HALFTONE);
    let _ = SetBrushOrgEx(hdc, 0, 0, None);
    StretchDIBits(
        hdc,
        x + dx,
        y + dy,
        dw,
        dh,
        0,
        0,
        thumb.width as i32,
        thumb.height as i32,
        Some(thumb.pixels.as_ptr().cast()),
        &header,
        DIB_RGB_COLORS,
        SRCCOPY,
    );
    SetStretchBltMode(hdc, windows::Win32::Graphics::Gdi::STRETCH_BLT_MODE(old_mode));
}
