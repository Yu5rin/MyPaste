//! クリップボードの履歴に入れる画像（スクリーンショットなど）の判断ロジック。
//!
//! Win32 に依存しないので Linux でもテストできる。クリップボードから読むこと・ファイルに
//! 残すこと・縮小した絵を作ることは [`crate::clip_image_store`] にある。
//!
//! 画像は文字より大きいので、中身はメモリに置かず、1 枚ずつ圧縮して暗号化したファイルに置く。
//! ここで持つのは、その目録（[`Index`]。大きさ・コピーした日時・ピン留めなど）だけ。
//!
//! - 件数の上限は、文字の履歴の「残す件数」と同じ。
//! - 保存した大きさの合計が [`MAX_TOTAL_BYTES`] を超えたら、古いものから消す。
//! - ピン留めしたものは、どちらの上限でも消さない。

use serde::{Deserialize, Serialize};

/// 保存した画像の大きさ（圧縮後）の合計の上限。
pub const MAX_TOTAL_BYTES: u64 = 500 * 1024 * 1024;
/// 1 枚の画像（圧縮前。クリップボードの DIB）の大きさの上限。これより大きいものは覚えない。
pub const MAX_IMAGE_BYTES: usize = 128 * 1024 * 1024;
/// ピン留めできる枚数の上限。
pub const MAX_PINNED: usize = 200;
/// 縮小した絵（一覧に出すもの）の大きさの上限。
pub const THUMB_MAX_W: u32 = 400;
pub const THUMB_MAX_H: u32 = 300;
/// 圧縮の強さ（速さを優先する。スクリーンショットは、これでも十分に小さくなる）。
const PACK_LEVEL: u8 = 1;

/// 1 枚の画像の目録。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// ファイルの名前に使う番号。
    pub id: u64,
    pub width: u32,
    pub height: u32,
    /// 保存した大きさ（圧縮後のバイト数）。
    pub stored: u64,
    /// 中身のハッシュ（同じ画像をもう一度コピーしたときに、増やさないため）。
    pub hash: u64,
    /// コピーした日時（表示用。例 `11/14 15:02`）。
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub pinned: bool,
    /// いつ入ったか（大きいほど新しい通し番号。文字の履歴と同じ並びに混ぜて出すのに使う）。
    #[serde(default)]
    pub stamp: u64,
}

/// 一覧に出す 1 件（文字か画像）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Text { text: String, pinned: bool },
    Image(Entry),
}

impl Item {
    pub fn pinned(&self) -> bool {
        match self {
            Item::Text { pinned, .. } => *pinned,
            Item::Image(entry) => entry.pinned,
        }
    }
}

/// 文字と画像の履歴を、1 つの一覧にまとめる。ピン留めしたもの（文字、画像の順）を先に、続けて
/// そのほかを新しい順（通し番号の大きい順。同じなら文字が先）に並べる。
///
/// `texts` はピン留めしていない文字（新しい順）と通し番号、`images` は [`Index::entries`] の順。
pub fn merge(pinned_texts: Vec<String>, texts: Vec<(String, u64)>, images: Vec<Entry>) -> Vec<Item> {
    let mut items: Vec<Item> = pinned_texts
        .into_iter()
        .map(|text| Item::Text { text, pinned: true })
        .collect();
    let (pinned_images, images): (Vec<Entry>, Vec<Entry>) =
        images.into_iter().partition(|e| e.pinned);
    items.extend(pinned_images.into_iter().map(Item::Image));
    let mut texts = texts.into_iter().peekable();
    let mut images = images.into_iter().peekable();
    loop {
        let text_first = match (texts.peek(), images.peek()) {
            (Some((_, stamp)), Some(image)) => *stamp >= image.stamp,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if text_first {
            if let Some((text, _)) = texts.next() {
                items.push(Item::Text { text, pinned: false });
            }
        } else if let Some(image) = images.next() {
            items.push(Item::Image(image));
        }
    }
    items
}

/// 画像の目録（新しい順）。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Index {
    items: Vec<Entry>,
    next_id: u64,
}

impl Index {
    /// 次の画像に付ける番号を取る。
    pub fn take_id(&mut self) -> u64 {
        let highest = self.items.iter().map(|e| e.id).max().unwrap_or(0);
        self.next_id = self.next_id.max(highest) + 1;
        self.next_id
    }

    /// 同じ画像（大きさとハッシュが同じもの）の番号。
    pub fn find(&self, hash: u64, width: u32, height: u32) -> Option<u64> {
        self.items
            .iter()
            .find(|e| e.hash == hash && e.width == width && e.height == height)
            .map(|e| e.id)
    }

    /// 新しい画像を加え、上限を超えた分を消す。消した画像の番号を返す（ファイルを消すため）。
    pub fn push(&mut self, entry: Entry, max: usize) -> Vec<u64> {
        self.items.retain(|e| e.id != entry.id);
        self.items.insert(0, entry);
        self.trim(max)
    }

    /// いちばん新しい位置へ移し、通し番号 `stamp` を付ける（同じ画像をもう一度コピーしたとき・
    /// 貼り付けたとき）。ピン留めしたものは動かさない。変わったら `true`。
    pub fn touch(&mut self, id: u64, stamp: u64) -> bool {
        match self.items.iter().position(|e| e.id == id) {
            Some(i) if !self.items[i].pinned => {
                let mut entry = self.items.remove(i);
                let changed = self.first_unpinned() != Some(i) || entry.stamp < stamp;
                entry.stamp = entry.stamp.max(stamp);
                self.items.insert(0, entry);
                changed
            }
            _ => false,
        }
    }

    /// いちばん新しい通し番号（無ければ 0）。
    pub fn max_stamp(&self) -> u64 {
        self.items.iter().map(|e| e.stamp).max().unwrap_or(0)
    }

    fn first_unpinned(&self) -> Option<usize> {
        self.items.iter().position(|e| !e.pinned)
    }

    /// 1 枚を消す。消えたら `true`。
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|e| e.id != id);
        self.items.len() != before
    }

    /// ピン留めする・外す。変わったら `true`。
    pub fn set_pinned(&mut self, id: u64, pinned: bool) -> bool {
        let count = self.items.iter().filter(|e| e.pinned).count();
        match self.items.iter_mut().find(|e| e.id == id) {
            Some(entry) if entry.pinned != pinned && (!pinned || count < MAX_PINNED) => {
                entry.pinned = pinned;
                true
            }
            _ => false,
        }
    }

    /// 上限を超えた分を、古いものから消す（ピン留めしたものと、いちばん新しいものは残す）。
    /// 消した画像の番号を返す。
    pub fn trim(&mut self, max: usize) -> Vec<u64> {
        let mut removed = Vec::new();
        let newest = self.first_unpinned().map(|i| self.items[i].id);
        loop {
            let unpinned = self.items.iter().filter(|e| !e.pinned).count();
            let total: u64 = self.items.iter().map(|e| e.stored).sum();
            if unpinned <= max.max(1) && total <= MAX_TOTAL_BYTES {
                break;
            }
            let oldest = self
                .items
                .iter()
                .rposition(|e| !e.pinned && Some(e.id) != newest);
            match oldest {
                Some(i) => removed.push(self.items.remove(i).id),
                None => break,
            }
        }
        removed
    }

    /// ピン留めしていないものを消す（「履歴を消す」）。消した画像の番号を返す。
    pub fn clear_unpinned(&mut self) -> Vec<u64> {
        let removed = self.items.iter().filter(|e| !e.pinned).map(|e| e.id).collect();
        self.items.retain(|e| e.pinned);
        removed
    }

    /// 一覧に出す順（ピン留めしたものが先、続けて新しい順）。
    pub fn entries(&self) -> Vec<Entry> {
        let pinned = self.items.iter().filter(|e| e.pinned);
        let others = self.items.iter().filter(|e| !e.pinned);
        pinned.chain(others).cloned().collect()
    }

    /// 目録にある画像の番号（ファイルの掃除で、目録に無いものを消すため）。
    pub fn ids(&self) -> Vec<u64> {
        self.items.iter().map(|e| e.id).collect()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }


    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn from_json(text: &str) -> Result<Index, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }
}

/// 一覧の 1 行に出す文（例 `画像 1920×1080  11/14 15:02`）。
pub fn row_label(entry: &Entry) -> String {
    if entry.time.is_empty() {
        format!("画像 {}×{}", entry.width, entry.height)
    } else {
        format!("画像 {}×{}  {}", entry.width, entry.height, entry.time)
    }
}

/// 「履歴」のタブで出すものの種類（`F` キーで すべて → 文字だけ → 画像だけ と切り替える）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KindFilter {
    #[default]
    All,
    Text,
    Image,
}

impl KindFilter {
    /// 次の種類（画像だけの次は、すべてに戻る）。
    pub fn next(self) -> KindFilter {
        match self {
            KindFilter::All => KindFilter::Text,
            KindFilter::Text => KindFilter::Image,
            KindFilter::Image => KindFilter::All,
        }
    }

    /// 見出しに出す名前。
    pub fn label(self) -> &'static str {
        match self {
            KindFilter::All => "すべて",
            KindFilter::Text => "文字だけ",
            KindFilter::Image => "画像だけ",
        }
    }

    /// この種類に入るか。
    pub fn accepts(self, item: &Item) -> bool {
        matches!(
            (self, item),
            (KindFilter::All, _)
                | (KindFilter::Text, Item::Text { .. })
                | (KindFilter::Image, Item::Image(_))
        )
    }
}

/// 検索で探す文（文字は全文、画像は一覧に出す文）。
pub fn search_text(item: &Item) -> String {
    match item {
        Item::Text { text, .. } => text.clone(),
        Item::Image(entry) => row_label(entry),
    }
}

/// まとめて貼り付けるものの 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// 文字（定型文は置き換えたあとのもの）。
    Text(String),
    /// 履歴の画像（番号）。
    Image(u64),
}

/// まとめて貼り付ける順に並べたものを、貼り付ける単位にする。続く文字は改行（CRLF）でつないで
/// 1 回で貼り付け、画像はその位置で 1 枚ずつ貼り付ける。つなぐ文字の終わりの改行は除く
/// （改行で終わる文字のあとに、空の行ができないように）。最後の文字の改行は残す。
pub fn batch(parts: Vec<Part>) -> Vec<Part> {
    let mut out: Vec<Part> = Vec::new();
    for part in parts {
        match (out.last_mut(), part) {
            (Some(Part::Text(previous)), Part::Text(text)) => {
                let kept = previous.trim_end_matches(['\r', '\n']).len();
                previous.truncate(kept);
                previous.push_str("\r\n");
                previous.push_str(&text);
            }
            (_, part) => out.push(part),
        }
    }
    out
}

/// クリップボードの DIB（`CF_DIB`）の見出しから読み取ったもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DibInfo {
    pub width: u32,
    pub height: u32,
    /// 画素の並びが始まる位置（先頭からのバイト数）。
    pub bits_offset: usize,
}

/// DIB の見出しを読む。画像として扱えない（見出しが壊れている・画素が無い）ものは `None`。
pub fn dib_info(dib: &[u8]) -> Option<DibInfo> {
    let u32_at = |at: usize| -> Option<u32> {
        dib.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let header = u32_at(0)? as usize;
    // BITMAPINFOHEADER（40）以上だけを扱う（古い BITMAPCOREHEADER は扱わない）。
    if !(40..=1024).contains(&header) {
        return None;
    }
    let width = u32_at(4)? as i32;
    let height = u32_at(8)? as i32;
    let bit_count = u32_at(12)? >> 16;
    let compression = u32_at(16)?;
    let colors_used = u32_at(32)? as usize;
    if width <= 0 || height == 0 || width > 65_535 || height.unsigned_abs() > 65_535 {
        return None;
    }
    // 色の表（8 ビット以下）と、色の取り出し方（BI_BITFIELDS）の分だけ、画素の始まりが後ろへずれる。
    let palette = match bit_count {
        1 | 4 | 8 => {
            if colors_used == 0 {
                1usize << bit_count
            } else {
                colors_used.min(256)
            }
        }
        16 | 24 | 32 => colors_used.min(256),
        _ => return None,
    };
    // 圧縮していないもの（BI_RGB）と、色の取り出し方を添えたもの（BI_BITFIELDS）だけを扱う。
    // RLE や JPEG・PNG を入れたものは、画素の大きさを見出しから確かめられないので扱わない。
    if compression != 0 && compression != 3 {
        return None;
    }
    let masks = if header == 40 && compression == 3 { 12 } else { 0 };
    let bits_offset = header + masks + palette * 4;
    // 画素が、見出しの言う大きさの分だけ本当にあるか（足りないものを描くと、範囲の外を読む）。
    let stride = (width as usize * bit_count as usize).div_ceil(32) * 4;
    let needed = stride.checked_mul(height.unsigned_abs() as usize)?;
    if bits_offset.checked_add(needed)? > dib.len() {
        return None;
    }
    Some(DibInfo {
        width: width as u32,
        height: height.unsigned_abs(),
        bits_offset,
    })
}

/// 縮小した絵の大きさ（縦横の比はそのまま。元が小さければ元の大きさ）。
pub fn thumb_size(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let (w, h) = (u64::from(width.max(1)), u64::from(height.max(1)));
    let (max_w, max_h) = (u64::from(max_w.max(1)), u64::from(max_h.max(1)));
    if w <= max_w && h <= max_h {
        return (w as u32, h as u32);
    }
    // 幅に合わせるか、高さに合わせるか（どちらも上限に収まる方）。
    if w * max_h >= h * max_w {
        (max_w as u32, ((h * max_w) / w).max(1) as u32)
    } else {
        (((w * max_h) / h).max(1) as u32, max_h as u32)
    }
}

/// 中身のハッシュ（FNV-1a）。同じ画像かどうかを見分けるためだけに使う。
pub fn hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    // 8 バイトずつ混ぜる（大きな画像でも速く終わるように）。
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        let v = u64::from_le_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]);
        h = (h ^ v).wrapping_mul(0x0000_0100_0000_01b3);
    }
    for b in chunks.remainder() {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^ bytes.len() as u64
}

/// 圧縮する（可逆）。
pub fn pack(bytes: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec(bytes, PACK_LEVEL)
}

/// 圧縮したものを元に戻す。壊れている・`max` より大きいものは `None`。
pub fn unpack(packed: &[u8], max: usize) -> Option<Vec<u8>> {
    miniz_oxide::inflate::decompress_to_vec_with_limit(packed, max).ok()
}

/// 縮小した絵（上から下へ、1 画素 4 バイトの BGRA）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Thumb {
    /// ファイルに置く形（幅・高さ・画素）にする。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(8 + self.pixels.len());
        bytes.extend_from_slice(&self.width.to_le_bytes());
        bytes.extend_from_slice(&self.height.to_le_bytes());
        bytes.extend_from_slice(&self.pixels);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Thumb> {
        let width = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?);
        let height = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?);
        let pixels = bytes.get(8..)?;
        if width == 0
            || height == 0
            || width > THUMB_MAX_W
            || height > THUMB_MAX_H
            || pixels.len() != width as usize * height as usize * 4
        {
            return None;
        }
        Some(Thumb {
            width,
            height,
            pixels: pixels.to_vec(),
        })
    }
}

/// 縮小した絵をファイルに置いたときの大きさの上限（元に戻すときの上限に使う）。
pub const THUMB_MAX_BYTES: usize = 8 + (THUMB_MAX_W * THUMB_MAX_H * 4) as usize;

/// 画面の枠 `(box_w, box_h)` の中に、縦横の比を保って収めたときの位置と大きさ
/// `(x, y, w, h)`（枠の中央に置く。元より大きくはしない）。
pub fn fit_rect(width: u32, height: u32, box_w: i32, box_h: i32) -> (i32, i32, i32, i32) {
    let (w, h) = thumb_size(width, height, box_w.max(1) as u32, box_h.max(1) as u32);
    let (w, h) = (w as i32, h as i32);
    ((box_w - w) / 2, (box_h - h) / 2, w, h)
}

/// 画像の画素（上から下へ、1 画素 4 バイトの RGBA）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// 透けているところがある（無ければ、不透明度を除いて RGB で保存できる）。
    pub has_alpha: bool,
}

impl Pixels {
    /// 1 画素 3 バイトの RGB（不透明度を除く）。
    pub fn to_rgb(&self) -> Vec<u8> {
        let mut rgb = Vec::with_capacity(self.rgba.len() / 4 * 3);
        for px in self.rgba.chunks_exact(4) {
            rgb.extend_from_slice(&px[..3]);
        }
        rgb
    }

    /// 白の上に重ねた、不透明な BGRA（文字の読み取りに渡す。透けたところが黒にならないように）。
    pub fn to_opaque_bgra(&self) -> Vec<u8> {
        let mut bgra = Vec::with_capacity(self.rgba.len());
        for px in self.rgba.chunks_exact(4) {
            let a = u32::from(px[3]);
            let over_white = |c: u8| ((u32::from(c) * a + 255 * (255 - a)) / 255) as u8;
            bgra.extend_from_slice(&[over_white(px[2]), over_white(px[1]), over_white(px[0]), 255]);
        }
        bgra
    }
}

/// 上から下へ並ぶ BGRA（Windows の 32 ビットの絵）を [`Pixels`] にする。`use_alpha` が偽なら
/// 不透明として扱う（GDI で描いた絵の 4 バイト目は意味を持たない）。真でも、不透明度がすべて 0 なら
/// 不透明として扱う（スクリーンショットの多くは 4 バイト目を使わず 0 にしている）。
pub fn bgra_to_pixels(width: u32, height: u32, bgra: &[u8], use_alpha: bool) -> Option<Pixels> {
    let count = (width as usize).checked_mul(height as usize)?;
    if count == 0 || bgra.len() != count.checked_mul(4)? {
        return None;
    }
    let use_alpha = use_alpha && bgra.chunks_exact(4).any(|px| px[3] != 0);
    let mut rgba = Vec::with_capacity(bgra.len());
    for px in bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], if use_alpha { px[3] } else { 255 }]);
    }
    let has_alpha = use_alpha && rgba.chunks_exact(4).any(|px| px[3] != 255);
    Some(Pixels {
        width,
        height,
        rgba,
        has_alpha,
    })
}

/// 色の取り出し方（BI_BITFIELDS のマスク）で、1 つの色を 0〜255 にして取り出す。
fn mask_channel(value: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let v = (value & mask) >> shift;
    if bits >= 8 {
        (v >> (bits - 8)) as u8
    } else {
        let max = (1u32 << bits) - 1;
        (v * 255 / max) as u8
    }
}

/// クリップボードの DIB を、上から下へ並ぶ画素にする。24 ビット（BI_RGB）と 32 ビット（BI_RGB・
/// BI_BITFIELDS）だけを扱い、下から上へ並ぶもの・上から下へ並ぶもののどちらも読める。それ以外の
/// 形（色の表を使うもの・16 ビットなど）は `None`（呼ぶ側が Windows に描かせて変換する）。
pub fn dib_pixels(dib: &[u8]) -> Option<Pixels> {
    let info = dib_info(dib)?;
    let u32_at = |at: usize| -> Option<u32> {
        dib.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let header = u32_at(0)? as usize;
    let bottom_up = (u32_at(8)? as i32) > 0;
    let bit_count = u32_at(12)? >> 16;
    let compression = u32_at(16)?;
    let (width, height) = (info.width as usize, info.height as usize);
    let stride = (width * bit_count as usize).div_ceil(32) * 4;
    // dib_info で、画素が足りていることは確かめてある。
    let row = |y: usize| -> &[u8] {
        let source = if bottom_up { height - 1 - y } else { y };
        let start = info.bits_offset + source * stride;
        &dib[start..start + stride]
    };
    let mut bgra = Vec::with_capacity(width * height * 4);
    match (bit_count, compression) {
        (24, 0) => {
            for y in 0..height {
                for px in row(y)[..width * 3].chunks_exact(3) {
                    bgra.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
            }
            bgra_to_pixels(info.width, info.height, &bgra, false)
        }
        (32, 0) => {
            for y in 0..height {
                bgra.extend_from_slice(&row(y)[..width * 4]);
            }
            bgra_to_pixels(info.width, info.height, &bgra, true)
        }
        (32, 3) => {
            // 色の取り出し方は、見出しの直後（BITMAPINFOHEADER）か、見出しの中（V4・V5）にある。
            let (red, green, blue) = (u32_at(40)?, u32_at(44)?, u32_at(48)?);
            let alpha = if header >= 56 { u32_at(52)? } else { 0 };
            for y in 0..height {
                for px in row(y)[..width * 4].chunks_exact(4) {
                    let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                    bgra.extend_from_slice(&[
                        mask_channel(v, blue),
                        mask_channel(v, green),
                        mask_channel(v, red),
                        mask_channel(v, alpha),
                    ]);
                }
            }
            bgra_to_pixels(info.width, info.height, &bgra, alpha != 0)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, stored: u64) -> Entry {
        Entry {
            id,
            width: 10,
            height: 10,
            stored,
            hash: id * 7,
            time: String::new(),
            pinned: false,
            stamp: id,
        }
    }

    #[test]
    fn push_keeps_newest_first_and_trims_by_count() {
        let mut index = Index::default();
        for id in 1..=5 {
            let removed = index.push(entry(id, 10), 3);
            if id <= 3 {
                assert!(removed.is_empty());
            } else {
                assert_eq!(removed, vec![id - 3]);
            }
        }
        let ids: Vec<u64> = index.entries().iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![5, 4, 3]);
    }

    #[test]
    fn trims_by_total_size_but_keeps_newest_and_pinned() {
        let mut index = Index::default();
        index.push(entry(1, MAX_TOTAL_BYTES / 2), 100);
        index.push(entry(2, MAX_TOTAL_BYTES / 2), 100);
        assert!(index.set_pinned(1, true));
        // 合計が上限を超える: ピン留めした 1 は残り、古い 2 が消える。
        let removed = index.push(entry(3, MAX_TOTAL_BYTES / 2), 100);
        assert_eq!(removed, vec![2]);
        // いちばん新しいものは、それだけで上限を超えても残す。
        let removed = index.push(entry(4, MAX_TOTAL_BYTES * 2), 100);
        assert_eq!(removed, vec![3]);
        let ids: Vec<u64> = index.entries().iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![1, 4]);
    }

    #[test]
    fn pinned_come_first_and_are_not_counted() {
        let mut index = Index::default();
        for id in 1..=4 {
            index.push(entry(id, 1), 10);
        }
        assert!(index.set_pinned(2, true));
        assert!(!index.set_pinned(2, true));
        let ids: Vec<u64> = index.entries().iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![2, 4, 3, 1]);
        assert_eq!(index.trim(2), vec![1]);
        assert_eq!(index.clear_unpinned(), vec![4, 3]);
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn touch_moves_to_top_and_find_matches_same_image() {
        let mut index = Index::default();
        for id in 1..=3 {
            index.push(entry(id, 1), 10);
        }
        assert_eq!(index.find(7, 10, 10), Some(1));
        assert_eq!(index.find(7, 11, 10), None);
        assert!(index.touch(1, 10));
        assert!(!index.touch(1, 10));
        assert_eq!(index.max_stamp(), 10);
        let ids: Vec<u64> = index.entries().iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![1, 3, 2]);
        assert!(index.remove(3));
        assert!(!index.remove(3));
    }

    #[test]
    fn merge_orders_by_stamp_with_pinned_first() {
        let mut index = Index::default();
        for (id, stamp) in [(1, 2), (2, 5), (3, 9)] {
            let mut e = entry(id, 1);
            e.stamp = stamp;
            index.push(e, 10);
        }
        index.set_pinned(1, true);
        let texts = vec![("c".to_string(), 7), ("b".to_string(), 5), ("a".to_string(), 1)];
        let merged = merge(vec!["p".to_string()], texts, index.entries());
        let names: Vec<String> = merged
            .iter()
            .map(|item| match item {
                Item::Text { text, .. } => text.clone(),
                Item::Image(e) => format!("#{}", e.id),
            })
            .collect();
        // ピン留め（文字、画像）→ 新しい順。同じ通し番号なら文字が先。
        assert_eq!(names, vec!["p", "#1", "#3", "c", "b", "#2", "a"]);
        assert!(merged[0].pinned() && merged[1].pinned() && !merged[2].pinned());
        assert!(merge(Vec::new(), Vec::new(), Vec::new()).is_empty());
    }

    #[test]
    fn ids_do_not_repeat_after_reload() {
        let mut index = Index::default();
        let a = index.take_id();
        index.push(entry(a, 1), 10);
        let b = index.take_id();
        index.push(entry(b, 1), 10);
        index.remove(b);
        let mut loaded = Index::from_json(&index.to_json()).unwrap();
        assert_eq!(loaded, index);
        assert!(loaded.take_id() > b);
        assert!(Index::from_json("[1,2]").is_err());
    }

    fn dib(width: i32, height: i32, bits: u16, compression: u32, extra: usize) -> Vec<u8> {
        let mut d = vec![0u8; 40];
        d[0..4].copy_from_slice(&40u32.to_le_bytes());
        d[4..8].copy_from_slice(&width.to_le_bytes());
        d[8..12].copy_from_slice(&height.to_le_bytes());
        d[12..14].copy_from_slice(&1u16.to_le_bytes());
        d[14..16].copy_from_slice(&bits.to_le_bytes());
        d[16..20].copy_from_slice(&compression.to_le_bytes());
        d.extend(std::iter::repeat_n(0, extra));
        d
    }

    #[test]
    fn reads_dib_header() {
        // 画素が足りないもの・圧縮したものは扱わない。
        assert!(dib_info(&dib(4, 2, 32, 0, 31)).is_none());
        assert!(dib_info(&dib(4000, 3000, 32, 0, 1024)).is_none());
        assert!(dib_info(&dib(4, 2, 8, 1, 1024 + 8)).is_none());
        let info = dib_info(&dib(4, -2, 32, 0, 32)).unwrap();
        assert_eq!((info.width, info.height, info.bits_offset), (4, 2, 40));
        // BI_BITFIELDS は、見出しのあとに色の取り出し方が 12 バイト続く。
        assert_eq!(dib_info(&dib(4, 2, 32, 3, 44)).unwrap().bits_offset, 52);
        // 8 ビットは 256 色の表が続く。
        assert_eq!(dib_info(&dib(4, 2, 8, 0, 1024 + 8)).unwrap().bits_offset, 40 + 1024);
        assert!(dib_info(&dib(0, 2, 32, 0, 32)).is_none());
        assert!(dib_info(&dib(4, 2, 32, 0, 0)).is_none());
        assert!(dib_info(&[0u8; 10]).is_none());
    }

    #[test]
    fn thumb_size_keeps_ratio() {
        assert_eq!(thumb_size(1920, 1080, 400, 300), (400, 225));
        assert_eq!(thumb_size(1080, 1920, 400, 300), (168, 300));
        assert_eq!(thumb_size(100, 50, 400, 300), (100, 50));
        assert_eq!(thumb_size(10_000, 1, 400, 300), (400, 1));
        assert_eq!(fit_rect(400, 200, 200, 200), (0, 50, 200, 100));
    }

    #[test]
    fn pack_round_trip_and_hash() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 7) as u8).collect();
        let packed = pack(&data);
        assert!(packed.len() < data.len());
        assert_eq!(unpack(&packed, data.len()).unwrap(), data);
        assert!(unpack(&packed, 100).is_none());
        assert!(unpack(&[1, 2, 3], 100).is_none());
        let mut other = data.clone();
        other[5000] ^= 1;
        assert_ne!(hash(&data), hash(&other));
        assert_eq!(hash(&data), hash(&data.clone()));
    }

    #[test]
    fn thumb_bytes_round_trip() {
        let thumb = Thumb {
            width: 2,
            height: 1,
            pixels: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        assert_eq!(Thumb::from_bytes(&thumb.to_bytes()).unwrap(), thumb);
        assert!(Thumb::from_bytes(&[0u8; 8]).is_none());
        assert!(Thumb::from_bytes(&[1, 0, 0, 0, 1, 0, 0, 0, 9]).is_none());
    }

    #[test]
    fn row_label_shows_size_and_time() {
        let mut e = entry(1, 1);
        e.width = 1920;
        e.height = 1080;
        assert_eq!(row_label(&e), "画像 1920×1080");
        e.time = "11/14 15:02".into();
        assert_eq!(row_label(&e), "画像 1920×1080  11/14 15:02");
        assert_eq!(search_text(&Item::Image(e)), "画像 1920×1080  11/14 15:02");
        let text = Item::Text {
            text: "abc".into(),
            pinned: false,
        };
        assert_eq!(search_text(&text), "abc");
    }

    /// 横 2 画素の DIB（`rows` はファイルに置く順の画素の行）。
    fn dib_with(bits: u16, compression: u32, height: i32, extra: &[u8], rows: &[&[u8]]) -> Vec<u8> {
        let mut d = dib(2, height, bits, compression, 0);
        d.extend_from_slice(extra);
        for r in rows {
            d.extend_from_slice(r);
        }
        d
    }

    #[test]
    fn reads_24_bit_bottom_up() {
        // 1 行 6 バイト + 埋め 2 バイト。下の行が先。
        let bottom: &[u8] = &[1, 2, 3, 4, 5, 6, 0, 0];
        let top: &[u8] = &[7, 8, 9, 10, 11, 12, 0, 0];
        let p = dib_pixels(&dib_with(24, 0, 2, &[], &[bottom, top])).unwrap();
        assert_eq!((p.width, p.height, p.has_alpha), (2, 2, false));
        assert_eq!(p.rgba, vec![9, 8, 7, 255, 12, 11, 10, 255, 3, 2, 1, 255, 6, 5, 4, 255]);
        assert_eq!(p.to_rgb(), vec![9, 8, 7, 12, 11, 10, 3, 2, 1, 6, 5, 4]);
    }

    #[test]
    fn reads_32_bit_top_down_and_zero_alpha_is_opaque() {
        let first: &[u8] = &[1, 2, 3, 0, 4, 5, 6, 0];
        let second: &[u8] = &[7, 8, 9, 0, 10, 11, 12, 0];
        let p = dib_pixels(&dib_with(32, 0, -2, &[], &[first, second])).unwrap();
        assert!(!p.has_alpha);
        assert_eq!(&p.rgba[..8], &[3, 2, 1, 255, 6, 5, 4, 255]);
        // 不透明度を使っているときは、そのまま残す。
        let first: &[u8] = &[1, 2, 3, 128, 4, 5, 6, 255];
        let p = dib_pixels(&dib_with(32, 0, -2, &[], &[first, second])).unwrap();
        assert!(p.has_alpha);
        assert_eq!(&p.rgba[..8], &[3, 2, 1, 128, 6, 5, 4, 255]);
        assert_eq!(p.rgba[11], 0);
        // 白の上に重ねる: 不透明度 128 の (3, 2, 1) は白に寄り、透明は白になる。
        let bgra = p.to_opaque_bgra();
        assert_eq!(&bgra[..4], &[127, 128, 128, 255]);
        assert_eq!(&bgra[4..8], &[4, 5, 6, 255]);
        assert_eq!(&bgra[8..12], &[255, 255, 255, 255]);
    }

    #[test]
    fn reads_32_bit_bitfields() {
        let masks: Vec<u8> = [0x00FF_0000u32, 0x0000_FF00, 0x0000_00FF]
            .iter()
            .flat_map(|m| m.to_le_bytes())
            .collect();
        let row: &[u8] = &[1, 2, 3, 0, 4, 5, 6, 0];
        let p = dib_pixels(&dib_with(32, 3, 2, &masks, &[row, row])).unwrap();
        assert!(!p.has_alpha);
        assert_eq!(&p.rgba[..8], &[3, 2, 1, 255, 6, 5, 4, 255]);
        // 赤と青を入れ替えたマスク。
        let swapped: Vec<u8> = [0x0000_00FFu32, 0x0000_FF00, 0x00FF_0000]
            .iter()
            .flat_map(|m| m.to_le_bytes())
            .collect();
        let p = dib_pixels(&dib_with(32, 3, 2, &swapped, &[row, row])).unwrap();
        assert_eq!(&p.rgba[..4], &[1, 2, 3, 255]);
    }

    #[test]
    fn other_formats_need_gdi() {
        let row: &[u8] = &[0, 0, 0, 0];
        assert!(dib_pixels(&dib_with(16, 0, 2, &[], &[row, row])).is_none());
        assert!(dib_pixels(&[0u8; 8]).is_none());
        assert_eq!(mask_channel(0b11111, 0b11111), 255);
        assert_eq!(mask_channel(0, 0), 0);
        assert!(bgra_to_pixels(2, 2, &[0; 15], true).is_none());
    }

    #[test]
    fn batch_joins_texts_between_images() {
        let parts = vec![
            Part::Text("a\r\n".into()),
            Part::Text("b".into()),
            Part::Image(3),
            Part::Image(4),
            Part::Text("c".into()),
            Part::Text("d\r\n".into()),
        ];
        assert_eq!(
            batch(parts),
            vec![
                Part::Text("a\r\nb".into()),
                Part::Image(3),
                Part::Image(4),
                Part::Text("c\r\nd\r\n".into()),
            ]
        );
        assert_eq!(batch(vec![Part::Text("x".into())]), vec![Part::Text("x".into())]);
        assert!(batch(Vec::new()).is_empty());
    }

    #[test]
    fn kind_filter_cycles_and_accepts() {
        let image = Item::Image(entry(1, 1));
        let text = Item::Text {
            text: "a".into(),
            pinned: false,
        };
        assert_eq!(KindFilter::default().next(), KindFilter::Text);
        assert_eq!(KindFilter::Text.next(), KindFilter::Image);
        assert_eq!(KindFilter::Image.next(), KindFilter::All);
        assert!(KindFilter::All.accepts(&image) && KindFilter::All.accepts(&text));
        assert!(KindFilter::Text.accepts(&text) && !KindFilter::Text.accepts(&image));
        assert!(KindFilter::Image.accepts(&image) && !KindFilter::Image.accepts(&text));
        assert_eq!(KindFilter::Image.label(), "画像だけ");
    }
}
