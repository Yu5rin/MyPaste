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
//! 件数や容量の上限などの判断は [`crate::clip_images`] にある。一覧の「画像をファイルに保存」で
//! PNG に書き出すのもここ（PNG にするのは [`crate::png`]）。

use std::path::PathBuf;

use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    SetBrushOrgEx, SetStretchBltMode, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HALFTONE, HDC, SRCCOPY,
};

use crate::clip_images::{self, DibInfo, Index, Pixels, Thumb};
use crate::{clip_store, config, png};

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

/// 目録を読み残したときに、目録と画像のファイルを移しておくフォルダー（`clip_images` の中）。
const UNREADABLE_DIR: &str = "unreadable";

/// 目録を読む。無ければ空。別のユーザー・別の PC で保存したもので元に戻せないときや、中身が
/// 壊れているときは、`index.unreadable.dat` として残し、画像のファイルも `unreadable` に移して
/// （新しい画像で上書きしたり、目録に無いものとして消したりしないように）、空を返す。このときは
/// `(空, true)`（呼び出し側は、目録に無いファイルを消さない）。ファイルがあるのに読めない
/// （ほかのアプリが使っている など）ときは `Err`（呼び出し側は、その間は画像を覚えない。
/// 空の目録で上書きすると、残していた画像を失うため）。
pub fn load_index() -> Result<(Index, bool), String> {
    let Some(dir) = dir() else {
        return Ok((Index::default(), false));
    };
    let path = dir.join(INDEX_FILE);
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Index::default(), false)),
        Err(e) => return Err(e.to_string()),
    };
    let index = clip_store::unprotect(&data)
        .ok_or_else(|| "元に戻せません（別のユーザー・PC で保存したもの）".to_string())
        .and_then(|plain| Index::from_json(&String::from_utf8_lossy(&plain)));
    match index {
        Ok(index) => Ok((index, false)),
        Err(e) => {
            let backup = path.with_extension("unreadable.dat");
            let _ = std::fs::rename(&path, &backup);
            set_aside_images(&dir);
            log::warn!("画像の履歴の目録を読めなかったため、空から始めます: {e}");
            Ok((Index::default(), true))
        }
    }
}

/// 画像のファイル（`.img` / `.thm`）を、`unreadable` のフォルダーに移す。
fn set_aside_images(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let aside = dir.join(UNREADABLE_DIR);
    if std::fs::create_dir_all(&aside).is_err() {
        return;
    }
    for entry in entries.flatten() {
        let path = entry.path();
        let is_ours = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "img" || e == "thm");
        if let (true, Some(name)) = (is_ours, path.file_name()) {
            if let Err(e) = std::fs::rename(&path, aside.join(name)) {
                log::warn!("画像の履歴: 読めない目録の画像を移せませんでした: {e}");
            }
        }
    }
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
    let pixels = stretch_dib(dib, info, width, height)?;
    Some(Thumb {
        width,
        height,
        pixels,
    })
}

/// DIB を、`(width, height)` の大きさの、上から下へ並ぶ 32 ビットの絵（BGRA）に描き直す
/// （色の深さや並びの違いは Windows に任せる。4 バイト目は意味を持たない）。
unsafe fn stretch_dib(dib: &[u8], info: DibInfo, width: u32, height: u32) -> Option<Vec<u8>> {
    stretch(
        dib.as_ptr().cast(),
        dib.as_ptr().add(info.bits_offset).cast(),
        info.width,
        info.height,
        width,
        height,
    )
}

/// 見出し `header` と画素 `bits` の絵（大きさ `src_w`×`src_h`）を、`(width, height)` の、上から下へ
/// 並ぶ 32 ビットの絵（BGRA）に描き直す。
unsafe fn stretch(
    header: *const BITMAPINFO,
    bits: *const core::ffi::c_void,
    src_w: u32,
    src_h: u32,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let screen = GetDC(HWND::default());
    let memory = CreateCompatibleDC(screen);
    ReleaseDC(HWND::default(), screen);
    if memory.is_invalid() {
        return None;
    }
    let target = top_down_header(width, height);
    let mut out: *mut core::ffi::c_void = std::ptr::null_mut();
    let bitmap = CreateDIBSection(memory, &target, DIB_RGB_COLORS, &mut out, HANDLE::default(), 0);
    let result = match bitmap {
        Ok(bitmap) if !out.is_null() => {
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
                src_w as i32,
                src_h as i32,
                Some(bits),
                header,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            let pixels = (drawn != 0).then(|| {
                std::slice::from_raw_parts(out as *const u8, width as usize * height as usize * 4)
                    .to_vec()
            });
            SelectObject(memory, old);
            let _ = DeleteObject(bitmap);
            pixels
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

/// DIB の画素（上から下へ、RGBA）。24・32 ビットのものは自分で読み（不透明度を残せる）、
/// それ以外の形は Windows に 32 ビットの絵として描かせてから読む（不透明として扱う）。
pub unsafe fn image_pixels(dib: &[u8]) -> Option<Pixels> {
    if let Some(pixels) = clip_images::dib_pixels(dib) {
        return Some(pixels);
    }
    let info = clip_images::dib_info(dib)?;
    let bgra = stretch_dib(dib, info, info.width, info.height)?;
    clip_images::bgra_to_pixels(info.width, info.height, &bgra, false)
}

/// 履歴の画像を PNG のファイルに書き出す。書き出した画像の (幅, 高さ) を返す。
pub unsafe fn save_png(id: u64, path: &std::path::Path) -> Result<(u32, u32), String> {
    let dib = load_image(id).ok_or("画像を読めませんでした")?;
    let pixels = image_pixels(&dib).ok_or("画像の形式を読み取れませんでした")?;
    let png = if pixels.has_alpha {
        png::encode(pixels.width, pixels.height, png::ColorType::Rgba, &pixels.rgba)
    } else {
        png::encode(pixels.width, pixels.height, png::ColorType::Rgb, &pixels.to_rgb())
    }
    .ok_or("PNG にできませんでした")?;
    std::fs::write(path, png).map_err(|e| e.to_string())?;
    Ok((pixels.width, pixels.height))
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
