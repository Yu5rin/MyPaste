//! PNG の書き出し（履歴の画像を「画像をファイルに保存」するとき）。
//!
//! 上から下へ並ぶ RGB か RGBA の画素を、PNG のファイルの中身にする。行ごとの前処理（フィルター）は
//! 使わず（種類 0）、まとめて DEFLATE で圧縮する（[`miniz_oxide`]）。Win32 に依存しないので
//! Linux でもテストできる。

/// PNG のファイルの先頭に置く 8 バイト。
pub const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
/// 圧縮の強さ（0〜10。保存は一度きりなので、ふつうの強さにする）。
const LEVEL: u8 = 6;

/// 画素の並び方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorType {
    /// 1 画素 3 バイト（赤・緑・青）。
    Rgb,
    /// 1 画素 4 バイト（赤・緑・青・不透明度）。
    Rgba,
}

impl ColorType {
    /// 1 画素のバイト数。
    pub fn channels(self) -> usize {
        match self {
            ColorType::Rgb => 3,
            ColorType::Rgba => 4,
        }
    }

    /// IHDR に書く色の種類。
    fn code(self) -> u8 {
        match self {
            ColorType::Rgb => 2,
            ColorType::Rgba => 6,
        }
    }
}

/// 画素（上から下へ、行の間にすき間なし）を PNG にする。大きさと画素の数が合わないときは `None`。
pub fn encode(width: u32, height: u32, color: ColorType, pixels: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return None;
    }
    let stride = (width as usize).checked_mul(color.channels())?;
    if stride.checked_mul(height as usize)? != pixels.len() {
        return None;
    }
    // 行ごとに、前処理の種類（0 = なし）の 1 バイトを前に付ける。
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in pixels.chunks_exact(stride) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&raw, LEVEL);

    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    // 1 色 8 ビット・色の種類・圧縮の方式 0・前処理の方式 0・飛び越しなし。
    header.extend_from_slice(&[8, color.code(), 0, 0, 0]);

    let mut out = Vec::with_capacity(compressed.len() + 64);
    out.extend_from_slice(&SIGNATURE);
    write_chunk(&mut out, b"IHDR", &header);
    write_chunk(&mut out, b"IDAT", &compressed);
    write_chunk(&mut out, b"IEND", &[]);
    Some(out)
}

/// 1 つのかたまり（長さ・種類・中身・CRC）を書く。
fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32_update(crc32_update(0xFFFF_FFFF, kind), data) ^ 0xFFFF_FFFF;
    out.extend_from_slice(&crc.to_be_bytes());
}

/// CRC32 の表（PNG と zip で使う多項式 0xEDB88320）。
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for b in data {
        crc = CRC_TABLE[((crc ^ u32::from(*b)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

/// CRC32 を計算する（テストで、書いた CRC を確かめるのに使う）。
#[cfg(test)]
fn crc32(data: &[u8]) -> u32 {
    crc32_update(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PNG を、かたまり（種類, 中身）の並びに分ける。CRC が合わなければ失敗にする。
    fn chunks(png: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(&png[..8], &SIGNATURE);
        let mut at = 8;
        let mut found = Vec::new();
        while at < png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = png[at + 4..at + 8].try_into().unwrap();
            let data = png[at + 8..at + 8 + len].to_vec();
            let crc = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            let mut checked = kind.to_vec();
            checked.extend_from_slice(&data);
            assert_eq!(crc, crc32(&checked));
            found.push((kind, data));
            at += 12 + len;
        }
        found
    }

    #[test]
    fn crc32_matches_known_values() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        // IEND のかたまりの CRC はいつも同じ。
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    #[test]
    fn writes_header_and_pixels() {
        let pixels: Vec<u8> = (0..2 * 3 * 4).map(|i| i as u8).collect();
        let png = encode(2, 3, ColorType::Rgba, &pixels).unwrap();
        let found = chunks(&png);
        let kinds: Vec<&[u8; 4]> = found.iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, vec![b"IHDR", b"IDAT", b"IEND"]);
        let header = &found[0].1;
        assert_eq!(header.len(), 13);
        assert_eq!(&header[0..4], &2u32.to_be_bytes());
        assert_eq!(&header[4..8], &3u32.to_be_bytes());
        assert_eq!(&header[8..13], &[8, 6, 0, 0, 0]);
        assert!(found[2].1.is_empty());
        // 圧縮を戻すと、行ごとに 0 を前に付けた画素になる。
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&found[1].1).unwrap();
        let mut expected = Vec::new();
        for row in pixels.chunks(8) {
            expected.push(0);
            expected.extend_from_slice(row);
        }
        assert_eq!(raw, expected);
    }

    #[test]
    fn rgb_uses_color_type_2() {
        let png = encode(1, 1, ColorType::Rgb, &[1, 2, 3]).unwrap();
        assert_eq!(chunks(&png)[0].1[9], 2);
    }

    #[test]
    fn rejects_wrong_sizes() {
        assert!(encode(0, 1, ColorType::Rgb, &[]).is_none());
        assert!(encode(2, 2, ColorType::Rgb, &[0; 11]).is_none());
        assert!(encode(2, 2, ColorType::Rgba, &[0; 12]).is_none());
    }
}
