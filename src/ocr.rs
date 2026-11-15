//! 画像の文字の読み取り（Windows に入っている OCR。`Windows.Media.Ocr`）。
//!
//! 履歴の一覧の「画像の文字を読み取って貼り付け」で使う。読み取る言語は、Windows の設定の
//! 「言語」に入っているもの（日本語なら、日本語の OCR が入っていること）。読み取れる言語が
//! 1 つも無いときは [`Failure::NoLanguage`]。
//!
//! 読み取りは時間がかかることがあるので、呼ぶ側は別のスレッドで呼ぶ（[`recognize`] は、
//! そのスレッドで COM を初期化する）。

use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use crate::{clip_image_store, text_transform};

/// 読み取れなかった理由。
#[derive(Debug)]
pub enum Failure {
    /// 読み取れる言語が入っていない。
    NoLanguage,
    /// 画像を読めない・読み取りに失敗した（理由）。
    Failed(String),
}

/// 縦横の上限を教えてもらえなかったときの値（Windows 10 / 11 の値）。
const DEFAULT_MAX_SIDE: u32 = 2600;

/// 画像（クリップボードの DIB）の文字を読み取る。行は改行（CRLF）で区切る。何も読み取れなければ
/// 空の文字を返す。
pub fn recognize(dib: &[u8]) -> Result<String, Failure> {
    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let result = recognize_inner(dib);
        if com {
            CoUninitialize();
        }
        result
    }
}

unsafe fn recognize_inner(dib: &[u8]) -> Result<String, Failure> {
    let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| Failure::NoLanguage)?;
    let max_side = OcrEngine::MaxImageDimension()
        .unwrap_or(DEFAULT_MAX_SIDE)
        .max(1);
    let (width, height, bgra) = clip_image_store::ocr_pixels(dib, max_side)
        .ok_or_else(|| Failure::Failed("画像の形式を読み取れませんでした".into()))?;
    let failed = |what: &str, e: windows::core::Error| Failure::Failed(format!("{what}: {e}"));

    let writer = DataWriter::new().map_err(|e| failed("バッファーを作れません", e))?;
    writer
        .WriteBytes(&bgra)
        .map_err(|e| failed("バッファーに書けません", e))?;
    let buffer = writer
        .DetachBuffer()
        .map_err(|e| failed("バッファーを取り出せません", e))?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        width as i32,
        height as i32,
    )
    .map_err(|e| failed("画像を渡せません", e))?;
    let result = engine
        .RecognizeAsync(&bitmap)
        .and_then(|operation| operation.get())
        .map_err(|e| failed("読み取りに失敗しました", e))?;

    let lines = result.Lines().map_err(|e| failed("結果を読めません", e))?;
    let mut text = Vec::new();
    for i in 0..lines.Size().unwrap_or(0) {
        let Ok(line) = lines.GetAt(i) else {
            continue;
        };
        let mut words = Vec::new();
        if let Ok(list) = line.Words() {
            for j in 0..list.Size().unwrap_or(0) {
                if let Ok(word) = list.GetAt(j).and_then(|w| w.Text()) {
                    words.push(word.to_string_lossy());
                }
            }
        }
        let joined = text_transform::join_ocr_words(&words);
        if !joined.is_empty() {
            text.push(joined);
        }
    }
    let _ = bitmap.Close();
    Ok(text.join("\r\n"))
}
