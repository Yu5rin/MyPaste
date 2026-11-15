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

    pub fn get(&self, id: u64) -> Option<&Entry> {
        self.items.iter().find(|e| e.id == id)
    }

    /// 新しい画像を加え、上限を超えた分を消す。消した画像の番号を返す（ファイルを消すため）。
    pub fn push(&mut self, entry: Entry, max: usize) -> Vec<u64> {
        self.items.retain(|e| e.id != entry.id);
        self.items.insert(0, entry);
        self.trim(max)
    }

    /// いちばん新しい位置へ移す（同じ画像をもう一度コピーしたとき・貼り付けたとき）。
    /// ピン留めしたものは動かさない。変わったら `true`。
    pub fn touch(&mut self, id: u64) -> bool {
        match self.items.iter().position(|e| e.id == id) {
            Some(i) if !self.items[i].pinned && self.first_unpinned() != Some(i) => {
                let entry = self.items.remove(i);
                self.items.insert(0, entry);
                true
            }
            _ => false,
        }
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

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
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
    let masks = if header == 40 && (compression == 3 || compression == 6) {
        if compression == 3 { 12 } else { 16 }
    } else {
        0
    };
    let bits_offset = header + masks + palette * 4;
    if bits_offset >= dib.len() {
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
        assert!(index.touch(1));
        assert!(!index.touch(1));
        let ids: Vec<u64> = index.entries().iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![1, 3, 2]);
        assert!(index.remove(3));
        assert!(!index.remove(3));
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
        d.extend(std::iter::repeat(0).take(extra));
        d
    }

    #[test]
    fn reads_dib_header() {
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
    }
}
