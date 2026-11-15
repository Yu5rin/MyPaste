//! クリップボードの履歴の判断ロジック。
//!
//! コピーされた文字を新しい順に覚えておく（[`History`]）。同じ文字がもう一度コピーされたら、
//! 前のものを消していちばん新しい位置へ移すので、履歴に同じものは 2 つ並ばない。
//! 一覧を出す操作（キーの組み合わせ、Ctrl などを 2 回押す）の読み取りと、2 回押しの判定
//! （[`DoubleTap`]）もここにある。
//!
//! 記録するかどうか・いつ記録するかは [`crate::actions`]、ファイルへの保存は
//! [`crate::clip_store`] が受け持つ。Win32 API に触れないので、Linux 上でもテストできる。

use std::collections::VecDeque;

use crate::config::ClipboardHistorySettings;
use crate::remap_logic::Hotkey;

/// 覚えておく件数の上限と下限、既定値。
pub const MAX_ITEMS_LIMIT: usize = 10_000;
pub const MIN_ITEMS: usize = 10;
pub const DEFAULT_ITEMS: usize = 1000;
/// 履歴全体の大きさの上限（UTF-8 のバイト数）。件数が多くても、大きなものばかりを覚えて
/// メモリや保存するファイルが大きくなりすぎないよう、超えたら古いものから消す。
pub const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;
/// 1 件として覚える文字数の上限（大きな表をコピーしたときにメモリやファイルを使いすぎないように）。
pub const MAX_CHARS: usize = 50_000;
/// 2 回押しの間隔の範囲と既定値（ミリ秒）。
pub const DOUBLE_TAP_MS_MIN: u32 = 200;
pub const DOUBLE_TAP_MS_MAX: u32 = 1000;
pub const DEFAULT_DOUBLE_TAP_MS: u32 = 400;
/// 一覧の 1 行に出す文字数の上限（実際には欄の幅に合わせて「…」で切る）。
const ROW_CHARS: usize = 200;
/// 一覧の 1 ページに並べる件数の範囲と既定値。
pub const MIN_PAGE_SIZE: usize = 10;
pub const MAX_PAGE_SIZE: usize = 40;
pub const DEFAULT_PAGE_SIZE: usize = 20;
/// 一覧の画面の幅（96 DPI 基準のピクセル）の範囲と既定値。
pub const MIN_WIDTH: u32 = 200;
pub const MAX_WIDTH: u32 = 600;
pub const DEFAULT_WIDTH: u32 = 260;
/// 一覧の画面の不透明度（%）の範囲と既定値。
pub const MIN_OPACITY: u32 = 30;
pub const MAX_OPACITY: u32 = 100;
pub const DEFAULT_OPACITY: u32 = 90;
/// 全文の吹き出しに出す行数と、1 行の文字数の上限。
const TIP_LINES: usize = 20;
const TIP_LINE_CHARS: usize = 200;

/// ピン留めできる件数の上限。
pub const MAX_PINNED: usize = 1000;

/// コピーされた文字の履歴。
///
/// ピン留めしたもの（[`History::pin`]）は別に持ち、覚えておく件数や大きさの上限では消さない。
/// 一覧には、ピン留めしたものを先に、続けてそのほかを新しい順に並べる。
#[derive(Debug)]
pub struct History {
    /// ピン留めしたもの（新しく留めたものが先）。
    pinned: Vec<String>,
    /// そのほか（新しい順）。
    items: VecDeque<String>,
    /// `items` の 1 件ずつの「いつ入ったか」（大きいほど新しい通し番号。画像の履歴と同じ並びに
    /// 混ぜて出すのに使う）。`items` と同じ順・同じ数。
    stamps: VecDeque<u64>,
    max: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::new(DEFAULT_ITEMS)
    }
}

/// ファイルに保存する形。
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Saved {
    pub pinned: Vec<String>,
    /// ピン留めしていないもの（新しい順）。
    pub items: Vec<String>,
    /// `items` の 1 件ずつの通し番号（無い・数が合わないときは、並び順から付け直す）。
    pub stamps: Vec<u64>,
}

impl History {
    pub fn new(max: usize) -> Self {
        Self {
            pinned: Vec::new(),
            items: VecDeque::new(),
            stamps: VecDeque::new(),
            max: clamp_items(max),
        }
    }

    /// 保存しておいた履歴から作る。条件に合わないものと重複は除く。
    pub fn from_saved(saved: Saved, max: usize) -> Self {
        let mut history = Self::new(max);
        for text in saved.pinned {
            if is_storable(&text)
                && !history.pinned.contains(&text)
                && history.pinned.len() < MAX_PINNED
            {
                history.pinned.push(text);
            }
        }
        // 古いものから順に入れると、新しいものが先頭に来て、重複は新しい方が残る。
        let stamped = saved.stamps.len() == saved.items.len();
        for (i, text) in saved.items.iter().enumerate().rev() {
            if stamped {
                history.push_at(text, saved.stamps[i]);
            } else {
                history.push(text);
            }
        }
        history
    }

    /// 保存する形にする。
    pub fn to_saved(&self) -> Saved {
        Saved {
            pinned: self.pinned.clone(),
            items: self.items.iter().cloned().collect(),
            stamps: self.stamps.iter().copied().collect(),
        }
    }

    /// いちばん新しい通し番号（無ければ 0）。
    pub fn max_stamp(&self) -> u64 {
        self.stamps.iter().copied().max().unwrap_or(0)
    }

    /// ピン留めしたもの（新しく留めたものが先）。
    pub fn pinned(&self) -> Vec<String> {
        self.pinned.clone()
    }

    /// ピン留めしていないもの（新しい順）と、その通し番号。
    pub fn unpinned(&self) -> Vec<(String, u64)> {
        self.items.iter().cloned().zip(self.stamps.iter().copied()).collect()
    }

    fn truncate(&mut self) {
        self.items.truncate(self.max);
        self.stamps.truncate(self.max);
    }

    fn remove_at(&mut self, index: usize) -> Option<String> {
        self.stamps.remove(index);
        self.items.remove(index)
    }

    fn push_front(&mut self, text: String, stamp: u64) {
        self.items.push_front(text);
        self.stamps.push_front(stamp);
    }

    /// 覚えておく件数を変える（減らしたときは古いものから消す。ピン留めしたものは消さない）。
    pub fn set_max(&mut self, max: usize) {
        self.max = clamp_items(max);
        self.truncate();
    }

    /// 全体の大きさが上限を超えていたら、古いものから消す（いちばん新しいものと、
    /// ピン留めしたものは残す）。
    fn trim_to_total(&mut self) {
        let mut total: usize = self.items.iter().map(String::len).sum();
        while total > MAX_TOTAL_BYTES && self.items.len() > 1 {
            self.stamps.pop_back();
            if let Some(old) = self.items.pop_back() {
                total -= old.len();
            }
        }
    }

    /// コピーされた文字を加える。空白だけの文字や、長すぎる文字は覚えない。
    /// すでにある文字なら、前のものを消していちばん新しい位置へ移す（ピン留めしたものと
    /// 同じなら、何もしない）。履歴が変わったら `true`。
    pub fn push(&mut self, text: &str) -> bool {
        self.push_at(text, self.max_stamp() + 1)
    }

    /// [`History::push`] と同じ。通し番号 `stamp` を付ける（画像の履歴と通しの番号）。
    pub fn push_at(&mut self, text: &str, stamp: u64) -> bool {
        if !is_storable(text) || self.pinned.iter().any(|t| t == text) {
            return false;
        }
        if self.items.front().is_some_and(|t| t == text) {
            // 並びは変わらないが、画像より新しくコピーし直したことは覚える。
            if let Some(front) = self.stamps.front_mut() {
                *front = (*front).max(stamp);
            }
            return false;
        }
        if let Some(i) = self.items.iter().position(|t| t == text) {
            self.remove_at(i);
        }
        self.push_front(text.to_string(), stamp);
        self.truncate();
        self.trim_to_total();
        true
    }

    /// 一覧に出す順（ピン留めしたものが先）。
    #[cfg(test)]
    pub fn items(&self) -> impl Iterator<Item = &String> {
        self.pinned.iter().chain(self.items.iter())
    }

    /// 一覧に出す順に、(文字, ピン留めしているか)。
    pub fn entries(&self) -> Vec<(String, bool)> {
        self.pinned
            .iter()
            .map(|t| (t.clone(), true))
            .chain(self.items.iter().map(|t| (t.clone(), false)))
            .collect()
    }

    /// ピン留めする（ピン留めしたものの先頭へ）。変わったら `true`。
    pub fn pin(&mut self, text: &str) -> bool {
        if self.pinned.iter().any(|t| t == text) || self.pinned.len() >= MAX_PINNED {
            return false;
        }
        let Some(i) = self.items.iter().position(|t| t == text) else {
            return false;
        };
        self.remove_at(i);
        self.pinned.insert(0, text.to_string());
        true
    }

    /// ピン留めを外す（ピン留めしていないものの先頭へ戻す）。変わったら `true`。
    #[cfg(test)]
    pub fn unpin(&mut self, text: &str) -> bool {
        self.unpin_at(text, self.max_stamp() + 1)
    }

    /// [`History::unpin`] と同じ。通し番号 `stamp` を付ける。
    pub fn unpin_at(&mut self, text: &str, stamp: u64) -> bool {
        let Some(i) = self.pinned.iter().position(|t| t == text) else {
            return false;
        };
        let text = self.pinned.remove(i);
        self.push_front(text, stamp);
        self.truncate();
        self.trim_to_total();
        true
    }

    /// 貼り付けたものを、いちばん新しい位置へ移す（ピン留めしたものは動かさない）。
    /// 変わったら `true`。
    #[cfg(test)]
    pub fn move_to_top(&mut self, text: &str) -> bool {
        if self.items.front().is_some_and(|t| t == text) {
            return false;
        }
        self.move_to_top_at(text, self.max_stamp() + 1)
    }

    /// [`History::move_to_top`] と同じ。通し番号 `stamp` を付ける（画像より上に出すため、
    /// すでに文字の中でいちばん上のときも、通し番号は付け直す）。
    pub fn move_to_top_at(&mut self, text: &str, stamp: u64) -> bool {
        match self.items.iter().position(|t| t == text) {
            None => false,
            Some(0) => {
                let changed = self.stamps.front().is_some_and(|s| *s < stamp);
                if let Some(front) = self.stamps.front_mut() {
                    *front = (*front).max(stamp);
                }
                changed
            }
            Some(i) => {
                if let Some(item) = self.remove_at(i) {
                    self.push_front(item, stamp);
                }
                true
            }
        }
    }

    /// 1 件を消す（一覧で消したとき。ピン留めしたものも消せる）。消えたら `true`。
    pub fn remove(&mut self, text: &str) -> bool {
        if let Some(i) = self.pinned.iter().position(|t| t == text) {
            self.pinned.remove(i);
            return true;
        }
        match self.items.iter().position(|t| t == text) {
            Some(i) => {
                self.remove_at(i);
                true
            }
            None => false,
        }
    }

    /// 全部の件数（ピン留めしたものを含む）。
    pub fn len(&self) -> usize {
        self.pinned.len() + self.items.len()
    }

    /// ピン留めしていないものを消す（「履歴を消す」）。
    pub fn clear_unpinned(&mut self) {
        self.items.clear();
        self.stamps.clear();
    }

    /// ピン留めしたものも含めて、すべて消す（記録をやめたとき）。
    pub fn clear(&mut self) {
        self.pinned.clear();
        self.items.clear();
        self.stamps.clear();
    }
}

/// 覚えてよい文字か（空白だけ・長すぎるものは覚えない）。
fn is_storable(text: &str) -> bool {
    !text.trim().is_empty() && text.chars().count() <= MAX_CHARS
}

/// 件数を範囲に収める。
pub fn clamp_items(max: usize) -> usize {
    max.clamp(MIN_ITEMS, MAX_ITEMS_LIMIT)
}

/// 履歴をファイルに保存する形（JSON）にする。
pub fn to_json(saved: &Saved) -> String {
    serde_json::to_string(saved).unwrap_or_else(|_| "{}".to_string())
}

/// 保存しておいた形から読む。前の版の形（文字列の配列、新しい順）も読む。読めなければ空。
pub fn from_json(text: &str) -> Result<Saved, String> {
    if let Ok(saved) = serde_json::from_str::<Saved>(text) {
        return Ok(saved);
    }
    // 以前の形（文字の配列だけ）。
    serde_json::from_str(text)
        .map(|items| Saved {
            pinned: Vec::new(),
            items,
            stamps: Vec::new(),
        })
        .map_err(|e| e.to_string())
}

/// 一覧を出す操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// キーの組み合わせ（例 Ctrl+Alt+H）。
    Hotkey,
    /// Ctrl を素早く 2 回押す。
    DoubleCtrl,
    /// Shift を素早く 2 回押す。
    DoubleShift,
    /// Alt を素早く 2 回押す。
    DoubleAlt,
}

impl Trigger {
    /// (値, settings.json に書く名前, 画面に出す名前)
    pub const ALL: [(Trigger, &'static str, &'static str); 4] = [
        (Trigger::Hotkey, "hotkey", "キーの組み合わせ"),
        (Trigger::DoubleCtrl, "double_ctrl", "Ctrl を素早く 2 回押す"),
        (Trigger::DoubleShift, "double_shift", "Shift を素早く 2 回押す"),
        (Trigger::DoubleAlt, "double_alt", "Alt を素早く 2 回押す"),
    ];

    /// settings.json の値から読む。知らない値はキーの組み合わせ。
    pub fn from_setting(text: &str) -> Trigger {
        Self::ALL
            .iter()
            .find(|(_, key, _)| key.eq_ignore_ascii_case(text.trim()))
            .map_or(Trigger::Hotkey, |(t, _, _)| *t)
    }

    pub fn as_setting(self) -> &'static str {
        Self::ALL.iter().find(|(t, _, _)| *t == self).map_or("hotkey", |(_, k, _)| *k)
    }

    /// 2 回押しで使う修飾キー（[`Modifier`]）。キーの組み合わせなら `None`。
    pub fn double_tap_key(self) -> Option<Modifier> {
        match self {
            Trigger::Hotkey => None,
            Trigger::DoubleCtrl => Some(Modifier::Ctrl),
            Trigger::DoubleShift => Some(Modifier::Shift),
            Trigger::DoubleAlt => Some(Modifier::Alt),
        }
    }
}

/// 2 回押しに使える修飾キー。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
}

impl Modifier {
    /// 仮想キーコードがこの修飾キー（左右どちらでも）か。
    pub fn matches(self, vk: u32) -> bool {
        match self {
            Modifier::Ctrl => matches!(vk, 0x11 | 0xA2 | 0xA3),
            Modifier::Shift => matches!(vk, 0x10 | 0xA0 | 0xA1),
            Modifier::Alt => matches!(vk, 0x12 | 0xA4 | 0xA5),
        }
    }
}

/// 一覧の配色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    /// Windows の色（ハイコントラストなどの設定にも従う）。
    System,
    Light,
    Dark,
    Blue,
    Green,
}

impl Theme {
    /// (値, settings.json に書く名前, 画面に出す名前)
    pub const ALL: [(Theme, &'static str, &'static str); 5] = [
        (Theme::System, "system", "Windows の色"),
        (Theme::Light, "light", "ライト"),
        (Theme::Dark, "dark", "ダーク"),
        (Theme::Blue, "blue", "ブルー"),
        (Theme::Green, "green", "グリーン"),
    ];

    pub fn from_setting(text: &str) -> Theme {
        Self::ALL
            .iter()
            .find(|(_, key, _)| key.eq_ignore_ascii_case(text.trim()))
            .map_or(Theme::System, |(t, _, _)| *t)
    }

    pub fn as_setting(self) -> &'static str {
        Self::ALL.iter().find(|(t, _, _)| *t == self).map_or("system", |(_, k, _)| *k)
    }

    /// 配色。Windows の色を使うときは `None`（画面の側で Windows の色を読む）。
    pub fn palette(self) -> Option<Palette> {
        let p = match self {
            Theme::System => return None,
            Theme::Light => Palette {
                back: rgb(255, 255, 255),
                alt: rgb(244, 246, 249),
                text: rgb(28, 28, 28),
                sub: rgb(140, 140, 140),
                sel_back: rgb(204, 228, 252),
                sel_text: rgb(0, 0, 0),
                header_back: rgb(236, 236, 236),
                header_text: rgb(90, 90, 90),
                border: rgb(160, 160, 160),
                tip_back: rgb(255, 255, 238),
                tip_text: rgb(28, 28, 28),
            },
            Theme::Dark => Palette {
                back: rgb(32, 32, 32),
                alt: rgb(42, 42, 42),
                text: rgb(232, 232, 232),
                sub: rgb(140, 140, 140),
                sel_back: rgb(0, 95, 184),
                sel_text: rgb(255, 255, 255),
                header_back: rgb(20, 20, 20),
                header_text: rgb(200, 200, 200),
                border: rgb(90, 90, 90),
                tip_back: rgb(48, 48, 48),
                tip_text: rgb(236, 236, 236),
            },
            Theme::Blue => Palette {
                back: rgb(244, 248, 255),
                alt: rgb(228, 238, 252),
                text: rgb(16, 34, 68),
                sub: rgb(110, 128, 160),
                sel_back: rgb(38, 98, 196),
                sel_text: rgb(255, 255, 255),
                header_back: rgb(38, 78, 150),
                header_text: rgb(255, 255, 255),
                border: rgb(38, 78, 150),
                tip_back: rgb(250, 252, 255),
                tip_text: rgb(16, 34, 68),
            },
            Theme::Green => Palette {
                back: rgb(246, 251, 246),
                alt: rgb(232, 244, 232),
                text: rgb(22, 52, 24),
                sub: rgb(112, 140, 112),
                sel_back: rgb(46, 125, 50),
                sel_text: rgb(255, 255, 255),
                header_back: rgb(46, 105, 50),
                header_text: rgb(255, 255, 255),
                border: rgb(46, 105, 50),
                tip_back: rgb(250, 255, 250),
                tip_text: rgb(22, 52, 24),
            },
        };
        Some(p)
    }
}

/// 一覧の画面の色（Windows の COLORREF と同じ 0x00BBGGRR の形）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// 行の背景と、1 行おきの背景。
    pub back: u32,
    pub alt: u32,
    /// 文字と、番号・行数などの控えめな文字。
    pub text: u32,
    pub sub: u32,
    /// 選んでいる行。
    pub sel_back: u32,
    pub sel_text: u32,
    /// 見出し（タブとページ）。
    pub header_back: u32,
    pub header_text: u32,
    /// 外枠。
    pub border: u32,
    /// 全文の吹き出し。
    pub tip_back: u32,
    pub tip_text: u32,
}

/// 赤・緑・青から、COLORREF の形（0x00BBGGRR）を作る。
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

/// 一覧を出す位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuPosition {
    /// 入力位置（キャレット）の近く。分からなければマウスの近く。
    Caret,
    /// マウスの近く。
    Mouse,
    /// 画面の右端に、上から下までの縦のパネル。
    Side,
    /// 画面の下に、横長の帯（カードを横に並べる）。
    Bottom,
}

impl MenuPosition {
    /// (値, settings.json に書く名前, 画面に出す名前)
    pub const ALL: [(MenuPosition, &'static str, &'static str); 4] = [
        (MenuPosition::Caret, "caret", "入力位置の近く"),
        (MenuPosition::Mouse, "mouse", "マウスの近く"),
        (MenuPosition::Side, "side", "画面の右端に縦のパネル"),
        (MenuPosition::Bottom, "bottom", "画面の下に横長の帯"),
    ];

    pub fn from_setting(text: &str) -> MenuPosition {
        Self::ALL
            .iter()
            .find(|(_, key, _)| key.eq_ignore_ascii_case(text.trim()))
            .map_or(MenuPosition::Caret, |(p, _, _)| *p)
    }

    pub fn as_setting(self) -> &'static str {
        Self::ALL.iter().find(|(p, _, _)| *p == self).map_or("caret", |(_, k, _)| *k)
    }
}

/// 設定（[`ClipboardHistorySettings`]）を読み取ったもの。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub enabled: bool,
    pub trigger: Trigger,
    /// 一覧を出すキーの組み合わせ（`trigger` がキーの組み合わせで、読めたときだけ）。
    pub hotkey: Option<Hotkey>,
    pub double_tap_ms: u32,
    pub max_items: usize,
    pub keep_after_exit: bool,
    pub position: MenuPosition,
    /// 一覧の画面の幅（96 DPI 基準）・不透明度（%）・1 ページの件数・配色。
    pub width: u32,
    pub opacity: u32,
    pub page_size: usize,
    pub theme: Theme,
    /// 一覧から貼り付けたものを、いちばん上に移すか。
    pub move_to_top: bool,
    /// 記録しないアプリ（大文字のプロセス名）。
    pub exclude_apps: Vec<String>,
    /// 画像も記録するか。
    pub record_images: bool,
    /// 貼り付けても一覧を出したままにするか。
    pub keep_open: bool,
    /// 使えない値があったときの理由（記録用）。
    pub problem: Option<String>,
}

impl Config {
    pub fn from_settings(settings: &ClipboardHistorySettings) -> Config {
        let trigger = Trigger::from_setting(&settings.trigger);
        let mut problem = None;
        let hotkey = if trigger == Trigger::Hotkey {
            match parse_hotkey(&settings.hotkey) {
                Ok(hotkey) => Some(hotkey),
                Err(e) => {
                    problem = Some(format!("一覧を出すキー「{}」は使えません: {e}", settings.hotkey));
                    None
                }
            }
        } else {
            None
        };
        Config {
            enabled: settings.enabled,
            trigger,
            hotkey,
            double_tap_ms: settings
                .double_tap_ms
                .clamp(DOUBLE_TAP_MS_MIN, DOUBLE_TAP_MS_MAX),
            max_items: clamp_items(settings.max_items),
            keep_after_exit: settings.keep_after_exit,
            position: MenuPosition::from_setting(&settings.position),
            width: settings.width.clamp(MIN_WIDTH, MAX_WIDTH),
            opacity: settings.opacity.clamp(MIN_OPACITY, MAX_OPACITY),
            page_size: settings.page_size.clamp(MIN_PAGE_SIZE, MAX_PAGE_SIZE),
            theme: Theme::from_setting(&settings.theme),
            move_to_top: settings.move_to_top,
            exclude_apps: crate::remap_logic::normalize_app_list(&settings.exclude_apps),
            record_images: settings.record_images,
            keep_open: settings.keep_open,
            problem,
        }
    }
}

/// 一覧を出すキーの組み合わせを読み、使えるか確かめる。
pub fn parse_hotkey(text: &str) -> Result<Hotkey, String> {
    let hotkey = Hotkey::parse(text)?;
    hotkey.validate_trigger()?;
    Ok(hotkey)
}

/// 修飾キーの「素早い 2 回押し」を見分ける。
///
/// そのキーだけを短く押して離す（ほかのキーと組み合わせない）ことを 2 回、決めた間隔の中で
/// 続けたら、2 回目を離したときに成立する。Ctrl+C のように組み合わせて使った押し方や、
/// 長押しは数えない。
#[derive(Debug, Default)]
pub struct DoubleTap {
    /// 押した時刻（離すまで）。
    down_at: Option<u32>,
    /// 押している間に、ほかのキーが押されなかったか。
    clean: bool,
    /// 1 回目を離した時刻。
    first_up_at: Option<u32>,
}

impl DoubleTap {
    /// 数え直す（マウスのボタンを押したときなど。Ctrl+クリックを 2 回押しと数えないように）。
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// キーが押された・離された（`time` はミリ秒の時刻。Windows のキーボードフックの値）。
    /// 2 回押しが成立したら `true`。
    pub fn on_key(&mut self, key: Modifier, vk: u32, down: bool, time: u32, interval_ms: u32) -> bool {
        if !key.matches(vk) {
            if down {
                // ほかのキーが押されたら、組み合わせとみなして数え直す。
                self.reset();
            } else if self.down_at.is_some() {
                // 押している間にほかのキーが離された（AltGr は Ctrl+Alt として届く など）。
                self.clean = false;
            }
            return false;
        }
        if down {
            // 押しっぱなし（オートリピート）では押した時刻を変えない。
            if self.down_at.is_none() {
                self.down_at = Some(time);
                self.clean = true;
            }
            return false;
        }
        let Some(down_at) = self.down_at.take() else {
            return false;
        };
        let short = time.wrapping_sub(down_at) <= interval_ms;
        if !self.clean || !short {
            self.first_up_at = None;
            return false;
        }
        match self.first_up_at {
            Some(first) if time.wrapping_sub(first) <= interval_ms => {
                self.first_up_at = None;
                true
            }
            _ => {
                self.first_up_at = Some(time);
                false
            }
        }
    }
}

/// 履歴の一覧を出している間（と、その前後）のキーの行き先（[`KeyGate::on_key`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRoute {
    /// そのまま通す（元のアプリに届く）。
    Pass,
    /// 握りつぶす（どこにも渡さない）。
    Swallow,
    /// 握りつぶして、出している一覧に渡す。
    Forward,
    /// 握りつぶして、次に出す一覧に渡すために取っておく（出し直すまでの間）。
    Queue,
}

/// キーが届いたときの一覧の様子。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GateState {
    /// 一覧を出している。
    pub shown: bool,
    /// 貼り付けのあとで、一覧を出し直すところ（その間に押されたキーは、出し直した一覧に渡す）。
    pub reopening: bool,
    /// 右クリックのメニューを出している（その間は、キーをメニューに渡す）。
    pub menu_open: bool,
    /// Alt か Win を押している（Alt+Tab などは、そのまま通す）。
    pub alt_or_win: bool,
}

/// 一覧に渡すキーと、握りつぶしたキーを覚えておく（キーボードフックで使う）。
///
/// 押したときに握りつぶしたキーは、離したときも握りつぶす（一覧が閉じたあとでも）。押したままの
/// キー（オートリピート）は、受け取った一覧にだけ渡し、閉じたあと・出し直した一覧には渡さない
/// （Enter を押したままにして、何度も貼り付けないように）。離したことを見逃したキーがいつまでも
/// 効かなくならないよう、しばらく何も届かなかったキーは、新しく押されたものとして扱う。
#[derive(Debug, Default)]
pub struct KeyGate {
    /// 握りつぶして、まだ離していないキー。
    held: Vec<HeldKey>,
    /// 一覧を出し直すまでの間に押されたキー。
    queued: Vec<u32>,
    /// 最後に出した一覧の番号（出すたびに増やす。0 はまだ出していない）。
    generation: u32,
}

/// 握りつぶして、まだ離していないキー。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeldKey {
    vk: u32,
    /// 受け取った一覧の番号（[`KeyGate::QUEUED`] は、次に出す一覧に渡すもの）。
    generation: u32,
    /// 最後に届いた時刻（ミリ秒）。
    time: u32,
}

impl KeyGate {
    /// 覚えておく、握りつぶしたキーの数と、取っておくキーの数の上限。
    pub const MAX_HELD: usize = 8;
    pub const MAX_QUEUED: usize = 16;
    /// これだけ何も届かなかったキーは、離したことを見逃したとみなす（押したままなら、
    /// オートリピートが届き続ける）。
    pub const STALE_MS: u32 = 1500;
    /// 次に出す一覧に渡すキーの印。
    const QUEUED: u32 = u32::MAX;

    pub const fn new() -> Self {
        Self {
            held: Vec::new(),
            queued: Vec::new(),
            generation: 0,
        }
    }

    /// キーが押された・離された（修飾キーそのものは呼ぶ側で除く。`now` はミリ秒の時刻）。
    pub fn on_key(&mut self, vk: u32, down: bool, now: u32, state: GateState) -> KeyRoute {
        let found = self.held.iter().position(|k| k.vk == vk);
        if !down {
            // 押したときに握りつぶしたキーなら、離したことも握りつぶす。
            return match found {
                Some(i) => {
                    self.held.remove(i);
                    KeyRoute::Swallow
                }
                None => KeyRoute::Pass,
            };
        }
        if let Some(i) = found {
            let key = &mut self.held[i];
            if now.wrapping_sub(key.time) <= Self::STALE_MS {
                // 押したまま（オートリピート）。受け取った一覧を出している間だけ、一覧に渡す。
                key.time = now;
                let same = state.shown && key.generation == self.generation;
                return if same && !state.menu_open {
                    KeyRoute::Forward
                } else {
                    KeyRoute::Swallow
                };
            }
            // 離したことを見逃していた。新しく押されたものとして扱う。
            self.held.remove(i);
        }
        if state.alt_or_win || (state.shown && state.menu_open) {
            return KeyRoute::Pass;
        }
        if state.shown {
            self.hold(vk, self.generation, now);
            return KeyRoute::Forward;
        }
        if state.reopening {
            self.hold(vk, Self::QUEUED, now);
            if self.queued.len() < Self::MAX_QUEUED {
                self.queued.push(vk);
                return KeyRoute::Queue;
            }
            return KeyRoute::Swallow;
        }
        KeyRoute::Pass
    }

    fn hold(&mut self, vk: u32, generation: u32, time: u32) {
        if self.held.len() < Self::MAX_HELD {
            self.held.push(HeldKey { vk, generation, time });
        }
    }

    /// 一覧を出した。取っておいたキー（この一覧に渡すもの）を返す。
    pub fn shown(&mut self) -> Vec<u32> {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 || self.generation == Self::QUEUED {
            self.generation = 1;
        }
        let generation = self.generation;
        for key in self.held.iter_mut().filter(|k| k.generation == Self::QUEUED) {
            key.generation = generation;
        }
        std::mem::take(&mut self.queued)
    }

    /// 一覧を出し終えた（出し直さない）。取っておいたキーは捨てる（離したときは握りつぶす）。
    pub fn session_ended(&mut self) {
        self.queued.clear();
        for key in self.held.iter_mut().filter(|k| k.generation == Self::QUEUED) {
            key.generation = 0;
        }
    }

    /// しばらく何も届かなかったキーを忘れる（一覧を出し始めるとき）。
    pub fn prune(&mut self, now: u32) {
        self.held.retain(|k| now.wrapping_sub(k.time) <= Self::STALE_MS);
    }
}

/// 一覧の 1 行に出す文字（最初の空でない行。タブは空白に）と、全体の行数。
pub fn row_text(text: &str) -> (String, usize) {
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let row: String = first.chars().take(ROW_CHARS).collect::<String>().replace('\t', " ");
    let lines = text.trim_end_matches(['\r', '\n']).lines().count().max(1);
    (row, lines)
}

/// 全文の吹き出しに出す文（長いものは途中まで。改行は \r\n にそろえる）。
pub fn tip_text(text: &str) -> String {
    let text = text.trim_end_matches(['\r', '\n']);
    let lines: Vec<&str> = text.lines().collect();
    let mut shown: Vec<String> = lines
        .iter()
        .take(TIP_LINES)
        .map(|l| {
            let mut line: String = l.chars().take(TIP_LINE_CHARS).collect();
            if l.chars().count() > TIP_LINE_CHARS {
                line.push('…');
            }
            line
        })
        .collect();
    if lines.len() > TIP_LINES {
        shown.push(format!("…（ほか {} 行）", lines.len() - TIP_LINES));
    }
    shown.join("\r\n")
}

/// ページの数（0 件でも 1 ページ）。`size` は 1 ページの件数。
pub fn page_count(len: usize, size: usize) -> usize {
    len.div_ceil(size.max(1)).max(1)
}

/// `page` ページ目（0 から）に並べる位置の範囲。
pub fn page_range(page: usize, len: usize, size: usize) -> std::ops::Range<usize> {
    let size = size.max(1);
    let start = (page * size).min(len);
    start..(start + size).min(len)
}

/// 一覧の上に出すページ（例 `1〜20 / 100`）。
pub fn page_label(page: usize, len: usize, size: usize) -> String {
    let range = page_range(page, len, size);
    if range.is_empty() {
        return "0 件".to_string();
    }
    format!("{}〜{} / {len}", range.start + 1, range.end)
}

/// 絞り込んでいるときの、一覧の上に出すページ（例 `1〜5 / 該当 5`）。
pub fn filtered_page_label(page: usize, len: usize, size: usize) -> String {
    let range = page_range(page, len, size);
    if range.is_empty() {
        return "該当なし".to_string();
    }
    format!("{}〜{} / 該当 {len}", range.start + 1, range.end)
}

/// 検索のために、大文字と小文字の違いをなくす。
pub fn fold_case(text: &str) -> String {
    text.to_lowercase()
}

/// 検索の言葉（空白で区切ったもの。全角の空白でも区切る）。大文字と小文字の違いはなくしておく。
pub fn query_terms(query: &str) -> Vec<String> {
    query
        .split(|c: char| c.is_whitespace() || c == '\u{3000}')
        .filter(|t| !t.is_empty())
        .map(fold_case)
        .collect()
}

/// 文（[`fold_case`] したもの）が、検索の言葉をすべて含むか（言葉が無ければ、いつも `true`）。
pub fn matches_terms(folded: &str, terms: &[String]) -> bool {
    terms.iter().all(|t| folded.contains(t.as_str()))
}

/// 絞り込んだ一覧（元の位置の並び）で、元の位置 `wanted` のもの、無ければその次（後ろに無ければ
/// 最後）を出している位置。空なら `None`。
pub fn nearest_position(visible: &[usize], wanted: usize) -> Option<usize> {
    if visible.is_empty() {
        return None;
    }
    Some(visible.iter().position(|v| *v >= wanted).unwrap_or(visible.len() - 1))
}

/// まとめて貼り付けるために印を付けたもの（一覧の番号, 元の位置）を、付けた順に覚える。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    items: Vec<(usize, usize)>,
}

impl Marks {
    /// 印を付ける・外す。付けたら `true`。
    pub fn toggle(&mut self, list: usize, index: usize) -> bool {
        match self.items.iter().position(|m| *m == (list, index)) {
            Some(i) => {
                self.items.remove(i);
                false
            }
            None => {
                self.items.push((list, index));
                true
            }
        }
    }

    /// 印を付けた順番（1 から）。付けていなければ `None`。
    pub fn order(&self, list: usize, index: usize) -> Option<usize> {
        self.items.iter().position(|m| *m == (list, index)).map(|i| i + 1)
    }

    /// 一覧から 1 件を消した（その印を外し、後ろのものの位置を 1 つ前へずらす）。
    pub fn removed(&mut self, list: usize, index: usize) {
        self.items.retain(|m| *m != (list, index));
        for m in self.items.iter_mut().filter(|m| m.0 == list && m.1 > index) {
            m.1 -= 1;
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// 付けた順の (一覧の番号, 元の位置)。
    pub fn iter(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.items.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_terms_match_case_insensitively() {
        let terms = query_terms("  Hello　ＷＯＲＬＤ ");
        assert_eq!(terms, vec!["hello".to_string(), "ｗｏｒｌｄ".to_string()]);
        assert!(matches_terms(&fold_case("say HELLO to the ＷＯＲＬＤ"), &terms));
        assert!(!matches_terms(&fold_case("hello"), &terms));
        assert!(matches_terms("anything", &query_terms("")));
        assert!(matches_terms(&fold_case("請求書の件"), &query_terms("請求")));
    }

    #[test]
    fn nearest_position_keeps_or_moves_forward() {
        let visible = [1, 4, 7];
        assert_eq!(nearest_position(&visible, 4), Some(1));
        assert_eq!(nearest_position(&visible, 5), Some(2));
        assert_eq!(nearest_position(&visible, 0), Some(0));
        assert_eq!(nearest_position(&visible, 9), Some(2));
        assert_eq!(nearest_position(&[], 0), None);
    }

    #[test]
    fn marks_keep_order_and_follow_removal() {
        let mut marks = Marks::default();
        assert!(marks.toggle(0, 5));
        assert!(marks.toggle(1, 2));
        assert!(marks.toggle(0, 1));
        assert_eq!(marks.order(0, 5), Some(1));
        assert_eq!(marks.order(0, 1), Some(3));
        assert_eq!(marks.order(0, 2), None);
        assert!(!marks.toggle(1, 2));
        assert_eq!(marks.order(0, 1), Some(2));
        // 元の位置 3 を消すと、5 は 4 になり、1 はそのまま。
        marks.removed(0, 3);
        assert_eq!(marks.iter().collect::<Vec<_>>(), vec![(0, 4), (0, 1)]);
        marks.removed(0, 4);
        assert_eq!(marks.iter().collect::<Vec<_>>(), vec![(0, 1)]);
        assert_eq!(marks.len(), 1);
        marks.clear();
        assert!(marks.is_empty());
    }

    #[test]
    fn filtered_label_shows_matches() {
        assert_eq!(filtered_page_label(0, 5, 20), "1〜5 / 該当 5");
        assert_eq!(filtered_page_label(1, 25, 20), "21〜25 / 該当 25");
        assert_eq!(filtered_page_label(0, 0, 20), "該当なし");
    }

    fn items(h: &History) -> Vec<String> {
        h.items().cloned().collect()
    }

    #[test]
    fn newest_first_and_no_duplicates() {
        let mut h = History::default();
        assert!(h.push("a"));
        assert!(h.push("b"));
        assert!(h.push("a"));
        assert_eq!(items(&h), ["a", "b"]);
        // いちばん新しいものと同じなら変わらない。
        assert!(!h.push("a"));
    }

    #[test]
    fn ignores_blank_and_huge() {
        let mut h = History::default();
        assert!(!h.push("   \r\n"));
        assert!(!h.push(&"x".repeat(MAX_CHARS + 1)));
        assert_eq!(h.len(), 0);
    }

    #[test]
    fn keeps_at_most_max_items() {
        let mut h = History::new(100);
        for i in 0..150 {
            h.push(&i.to_string());
        }
        assert_eq!(h.len(), 100);
        assert_eq!(h.items().next().unwrap(), "149");
        h.set_max(20);
        assert_eq!(h.len(), 20);
        assert_eq!(h.items().last().unwrap(), "130");
        // 範囲の外は収める。
        assert_eq!(History::new(5).max, MIN_ITEMS);
        assert_eq!(History::new(100_000).max, MAX_ITEMS_LIMIT);
        assert_eq!(History::default().max, 1000);
    }

    #[test]
    fn keeps_total_size_bounded() {
        let mut h = History::new(MAX_ITEMS_LIMIT);
        let big = "x".repeat(MAX_CHARS - 10);
        let count = MAX_TOTAL_BYTES / (MAX_CHARS - 10) + 10;
        for i in 0..count {
            h.push(&format!("{i}{big}"));
        }
        let total: usize = h.items().map(String::len).sum();
        assert!(total <= MAX_TOTAL_BYTES);
        assert!(h.items().next().unwrap().starts_with(&(count - 1).to_string()));
    }

    #[test]
    fn themes() {
        assert_eq!(Theme::from_setting("DARK"), Theme::Dark);
        assert_eq!(Theme::from_setting("?"), Theme::System);
        assert_eq!(Theme::Blue.as_setting(), "blue");
        assert!(Theme::System.palette().is_none());
        assert_eq!(Theme::Light.palette().unwrap().back, 0x00FF_FFFF);
        assert_eq!(rgb(1, 2, 3), 0x0003_0201);
    }

    #[test]
    fn saved_round_trip() {
        let mut h = History::default();
        h.push("古い");
        h.push("新しい\r\n2 行目");
        h.push("留める");
        h.pin("留める");
        let saved = from_json(&to_json(&h.to_saved())).unwrap();
        let restored = History::from_saved(saved, 100);
        assert_eq!(restored.entries(), h.entries());
        // 重複や空のものが混じっていても、新しい方を残して読む。
        let restored = History::from_saved(
            Saved {
                pinned: vec![],
                items: vec!["a".into(), " ".into(), "b".into(), "a".into()],
                stamps: vec![],
            },
            100,
        );
        assert_eq!(items(&restored), ["a", "b"]);
        // 通し番号は、保存して読み直しても同じ。付け直すとき（前の版の形）は、新しいほど大きい。
        let mut h = History::new(100);
        h.push_at("古い", 3);
        h.push_at("新しい", 8);
        assert!(!h.push_at("新しい", 12));
        assert_eq!(h.max_stamp(), 12);
        let again = History::from_saved(from_json(&to_json(&h.to_saved())).unwrap(), 100);
        assert_eq!(again.unpinned(), vec![("新しい".to_string(), 12), ("古い".to_string(), 3)]);
        assert!(h.move_to_top_at("古い", 20));
        assert_eq!(h.unpinned()[0], ("古い".to_string(), 20));
        let stamps: Vec<u64> = restored.unpinned().iter().map(|(_, s)| *s).collect();
        assert!(stamps[0] > stamps[1]);
        // 前の版の形（文字列の配列）も読める。
        assert_eq!(from_json(r#"["x","y"]"#).unwrap().items, ["x", "y"]);
        assert!(from_json("壊れた").is_err());
    }

    #[test]
    fn pins_survive_limits_and_stay_first() {
        let mut h = History::new(10);
        h.push("大事");
        assert!(h.pin("大事"));
        assert!(!h.pin("大事"));
        for i in 0..30 {
            h.push(&i.to_string());
        }
        // ピン留めは件数の上限で消えず、先頭に出る。
        assert_eq!(h.items().next().unwrap(), "大事");
        assert_eq!(h.len(), 11);
        // ピン留めと同じ文字をコピーしても増えない。
        assert!(!h.push("大事"));
        // 外すと、ふつうの履歴の先頭に戻る。
        assert!(h.unpin("大事"));
        assert_eq!(h.entries()[0], ("大事".to_string(), false));
        assert_eq!(h.len(), 10);
        // 「履歴を消す」はピン留めを残す。
        h.pin("大事");
        h.clear_unpinned();
        assert_eq!(items(&h), ["大事"]);
    }

    #[test]
    fn moves_pasted_to_top() {
        let mut h = History::default();
        h.push("a");
        h.push("b");
        h.push("c");
        assert!(h.move_to_top("a"));
        assert_eq!(items(&h), ["a", "c", "b"]);
        assert!(!h.move_to_top("a"));
        assert!(!h.move_to_top("なし"));
    }

    #[test]
    fn reads_settings() {
        assert_eq!(Trigger::from_setting("double_ctrl"), Trigger::DoubleCtrl);
        assert_eq!(Trigger::from_setting("?"), Trigger::Hotkey);
        assert_eq!(Trigger::DoubleAlt.as_setting(), "double_alt");
        assert_eq!(Trigger::DoubleShift.double_tap_key(), Some(Modifier::Shift));
        assert_eq!(MenuPosition::from_setting("MOUSE"), MenuPosition::Mouse);
        assert_eq!(MenuPosition::from_setting(""), MenuPosition::Caret);
    }

    #[test]
    fn reads_config() {
        let mut settings = ClipboardHistorySettings::default();
        let config = Config::from_settings(&settings);
        assert!(!config.enabled);
        assert_eq!(config.hotkey, Some(Hotkey::parse("Ctrl+Alt+H").unwrap()));
        assert_eq!(config.max_items, 1000);
        assert_eq!(config.theme, Theme::System);
        assert!(config.move_to_top);
        assert!(config.exclude_apps.is_empty());
        let config = Config::from_settings(&ClipboardHistorySettings {
            exclude_apps: vec!["keepass".into(), "1Password.exe".into()],
            ..ClipboardHistorySettings::default()
        });
        assert_eq!(config.exclude_apps, ["KEEPASS.EXE", "1PASSWORD.EXE"]);
        assert!(config.keep_after_exit);
        assert_eq!((config.width, config.opacity, config.page_size), (260, 90, 20));
        settings.trigger = "double_ctrl".into();
        settings.max_items = 50_000;
        settings.theme = "green".into();
        settings.double_tap_ms = 50;
        settings.width = 5000;
        settings.opacity = 0;
        settings.page_size = 1;
        let config = Config::from_settings(&settings);
        assert_eq!(config.trigger, Trigger::DoubleCtrl);
        assert_eq!(config.hotkey, None);
        assert_eq!(config.max_items, MAX_ITEMS_LIMIT);
        assert_eq!(config.theme, Theme::Green);
        assert_eq!(config.double_tap_ms, DOUBLE_TAP_MS_MIN);
        assert_eq!((config.width, config.opacity, config.page_size), (600, 30, 10));
        settings.trigger = "hotkey".into();
        settings.hotkey = "H".into();
        let config = Config::from_settings(&settings);
        assert_eq!(config.hotkey, None);
        assert!(config.problem.is_some());
    }

    #[test]
    fn double_tap() {
        let ctrl = 0xA2;
        let mut d = DoubleTap::default();
        let k = Modifier::Ctrl;
        assert!(!d.on_key(k, ctrl, true, 0, 400));
        assert!(!d.on_key(k, ctrl, false, 50, 400));
        assert!(!d.on_key(k, ctrl, true, 150, 400));
        assert!(!d.on_key(k, ctrl, true, 180, 400)); // 押しっぱなし
        assert!(d.on_key(k, ctrl, false, 200, 400));
        // 成立したあとは数え直す。
        assert!(!d.on_key(k, ctrl, true, 250, 400));
        assert!(!d.on_key(k, ctrl, false, 300, 400));
    }

    #[test]
    fn double_tap_rejects_combinations_and_slow_taps() {
        let k = Modifier::Ctrl;
        // Ctrl+C のあとの Ctrl 1 回では成立しない。
        let mut d = DoubleTap::default();
        d.on_key(k, 0x11, true, 0, 400);
        d.on_key(k, 0x43, true, 10, 400);
        d.on_key(k, 0x43, false, 20, 400);
        d.on_key(k, 0x11, false, 30, 400);
        d.on_key(k, 0x11, true, 100, 400);
        assert!(!d.on_key(k, 0x11, false, 120, 400));
        // 間が空きすぎたら成立しない（そこからが 1 回目になる）。
        let mut d = DoubleTap::default();
        d.on_key(k, 0x11, true, 0, 400);
        d.on_key(k, 0x11, false, 50, 400);
        d.on_key(k, 0x11, true, 900, 400);
        assert!(!d.on_key(k, 0x11, false, 950, 400));
        d.on_key(k, 0x11, true, 1000, 400);
        assert!(d.on_key(k, 0x11, false, 1050, 400));
        // AltGr（左 Ctrl と右 Alt が一緒に届く）を Alt の 2 回押しと数えない。
        let alt = Modifier::Alt;
        let mut d = DoubleTap::default();
        for t in [0u32, 200] {
            d.on_key(alt, 0xA2, true, t, 400);
            d.on_key(alt, 0xA5, true, t + 1, 400);
            d.on_key(alt, 0xA2, false, t + 30, 400);
            assert!(!d.on_key(alt, 0xA5, false, t + 31, 400));
        }
        // マウスのボタンで数え直す（Ctrl+クリックを 2 回）。
        let mut d = DoubleTap::default();
        d.on_key(k, 0x11, true, 0, 400);
        d.reset();
        d.on_key(k, 0x11, false, 50, 400);
        d.on_key(k, 0x11, true, 100, 400);
        d.reset();
        assert!(!d.on_key(k, 0x11, false, 150, 400));
        // 長押しは数えない。
        let mut d = DoubleTap::default();
        d.on_key(k, 0x11, true, 0, 400);
        d.on_key(k, 0x11, false, 50, 400);
        d.on_key(k, 0x11, true, 60, 400);
        assert!(!d.on_key(k, 0x11, false, 800, 400));
        // 時刻が一周しても数えられる。
        let mut d = DoubleTap::default();
        d.on_key(k, 0x11, true, u32::MAX - 100, 400);
        d.on_key(k, 0x11, false, u32::MAX - 50, 400);
        d.on_key(k, 0x11, true, 20, 400);
        assert!(d.on_key(k, 0x11, false, 60, 400));
    }

    #[test]
    fn rows_and_preview() {
        assert_eq!(row_text("hello"), ("hello".to_string(), 1));
        assert_eq!(row_text("\r\n  A\tB\r\n2行目\r\n"), ("A B".to_string(), 3));
        assert_eq!(row_text(&"あ".repeat(300)).0.chars().count(), 200);
        assert_eq!(tip_text("a\nb\r\nc\r\n"), "a\r\nb\r\nc");
        let many = (1..=25).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        assert!(tip_text(&many).ends_with("20\r\n…（ほか 5 行）"));
        assert!(tip_text(&"x".repeat(300)).ends_with("x…"));
    }

    #[test]
    fn pages() {
        assert_eq!(page_count(0, 20), 1);
        assert_eq!(page_count(20, 20), 1);
        assert_eq!(page_count(21, 20), 2);
        assert_eq!(page_count(100, 30), 4);
        assert_eq!(page_range(1, 25, 20), 20..25);
        assert_eq!(page_range(3, 25, 20), 25..25);
        assert_eq!(page_label(0, 100, 20), "1〜20 / 100");
        assert_eq!(page_label(1, 25, 20), "21〜25 / 25");
        assert_eq!(page_label(0, 0, 20), "0 件");
    }

    #[test]
    fn removes() {
        let mut h = History::default();
        h.push("a");
        h.push("b");
        assert!(h.remove("a"));
        assert!(!h.remove("a"));
        assert_eq!(items(&h), ["b"]);
    }

    const SHOWN: GateState = GateState {
        shown: true,
        reopening: false,
        menu_open: false,
        alt_or_win: false,
    };
    const CLOSED: GateState = GateState {
        shown: false,
        reopening: false,
        menu_open: false,
        alt_or_win: false,
    };
    const REOPENING: GateState = GateState {
        shown: false,
        reopening: true,
        menu_open: false,
        alt_or_win: false,
    };
    const ENTER: u32 = 0x0D;
    const DOWN: u32 = 0x28;

    #[test]
    fn gate_forwards_keys_and_swallows_their_release() {
        let mut gate = KeyGate::new();
        assert!(gate.shown().is_empty());
        assert_eq!(gate.on_key(DOWN, true, 0, SHOWN), KeyRoute::Forward);
        // 押したまま（オートリピート）は、同じ一覧には渡す。
        assert_eq!(gate.on_key(DOWN, true, 30, SHOWN), KeyRoute::Forward);
        assert_eq!(gate.on_key(DOWN, false, 60, SHOWN), KeyRoute::Swallow);
        // 一覧を出す前から押していたキーを離したときは通す。
        assert_eq!(gate.on_key(ENTER, false, 70, SHOWN), KeyRoute::Pass);
        // 一覧を出していなければ通す。
        assert_eq!(gate.on_key(ENTER, true, 80, CLOSED), KeyRoute::Pass);
        // Alt+Tab などは通す。
        let alt = GateState { alt_or_win: true, ..SHOWN };
        assert_eq!(gate.on_key(0x09, true, 90, alt), KeyRoute::Pass);
    }

    #[test]
    fn gate_swallows_held_key_after_close() {
        let mut gate = KeyGate::new();
        gate.shown();
        assert_eq!(gate.on_key(ENTER, true, 0, SHOWN), KeyRoute::Forward);
        // 一覧が閉じたあとのオートリピートと離したときは、元のアプリに届けない。
        assert_eq!(gate.on_key(ENTER, true, 500, CLOSED), KeyRoute::Swallow);
        gate.session_ended();
        assert_eq!(gate.on_key(ENTER, true, 530, CLOSED), KeyRoute::Swallow);
        assert_eq!(gate.on_key(ENTER, false, 560, CLOSED), KeyRoute::Swallow);
        assert_eq!(gate.on_key(ENTER, true, 600, CLOSED), KeyRoute::Pass);
    }

    #[test]
    fn gate_does_not_repeat_into_reopened_list() {
        let mut gate = KeyGate::new();
        gate.shown();
        assert_eq!(gate.on_key(ENTER, true, 0, SHOWN), KeyRoute::Forward);
        assert_eq!(gate.on_key(ENTER, true, 400, REOPENING), KeyRoute::Swallow);
        assert!(gate.shown().is_empty());
        // 出し直した一覧には、前の一覧で押した Enter のオートリピートを渡さない。
        assert_eq!(gate.on_key(ENTER, true, 600, SHOWN), KeyRoute::Swallow);
        assert_eq!(gate.on_key(ENTER, false, 650, SHOWN), KeyRoute::Swallow);
        assert_eq!(gate.on_key(ENTER, true, 700, SHOWN), KeyRoute::Forward);
    }

    #[test]
    fn gate_queues_keys_while_reopening() {
        let mut gate = KeyGate::new();
        gate.shown();
        assert_eq!(gate.on_key(DOWN, true, 0, REOPENING), KeyRoute::Queue);
        assert_eq!(gate.on_key(DOWN, false, 20, REOPENING), KeyRoute::Swallow);
        assert_eq!(gate.on_key(ENTER, true, 40, REOPENING), KeyRoute::Queue);
        assert_eq!(gate.shown(), vec![DOWN, ENTER]);
        // 出し直す前に押して、まだ押している Enter は、出し直した一覧のものとして扱う。
        assert_eq!(gate.on_key(ENTER, true, 70, SHOWN), KeyRoute::Forward);
        assert_eq!(gate.on_key(ENTER, false, 90, SHOWN), KeyRoute::Swallow);
        // 出し直さずに終わったら、取っておいたキーは捨てる。
        assert_eq!(gate.on_key(DOWN, true, 100, REOPENING), KeyRoute::Queue);
        gate.session_ended();
        assert!(gate.shown().is_empty());
        assert_eq!(gate.on_key(DOWN, true, 130, SHOWN), KeyRoute::Swallow);
        assert_eq!(gate.on_key(DOWN, false, 140, SHOWN), KeyRoute::Swallow);
        // 取っておける数には上限がある（超えた分は捨てる）。
        for i in 0..(KeyGate::MAX_QUEUED as u32 + 4) {
            gate.on_key(0x41 + i, true, 200, REOPENING);
        }
        assert_eq!(gate.shown().len(), KeyGate::MAX_QUEUED);
    }

    #[test]
    fn gate_menu_gets_keys() {
        let mut gate = KeyGate::new();
        gate.shown();
        let menu = GateState { menu_open: true, ..SHOWN };
        assert_eq!(gate.on_key(0x5D, true, 0, SHOWN), KeyRoute::Forward);
        // メニューを出している間は、新しいキーはメニューへ。メニューを出したキーの
        // オートリピートと、離したときは握りつぶす。
        assert_eq!(gate.on_key(DOWN, true, 10, menu), KeyRoute::Pass);
        assert_eq!(gate.on_key(0x5D, true, 30, menu), KeyRoute::Swallow);
        assert_eq!(gate.on_key(0x5D, false, 40, menu), KeyRoute::Swallow);
        assert_eq!(gate.on_key(DOWN, false, 50, menu), KeyRoute::Pass);
    }

    #[test]
    fn gate_forgets_missed_releases() {
        let mut gate = KeyGate::new();
        gate.shown();
        assert_eq!(gate.on_key(ENTER, true, 0, SHOWN), KeyRoute::Forward);
        // 離したことを見逃した（UAC の画面の上で離した など）。しばらくしたら、また使える。
        let later = KeyGate::STALE_MS + 1;
        assert_eq!(gate.on_key(ENTER, true, later, CLOSED), KeyRoute::Pass);
        assert_eq!(gate.on_key(ENTER, false, later + 10, CLOSED), KeyRoute::Pass);
        // 出し始めるときにも、古いものを忘れる。
        gate.shown();
        assert_eq!(gate.on_key(DOWN, true, 0, SHOWN), KeyRoute::Forward);
        gate.prune(later);
        assert_eq!(gate.on_key(DOWN, false, later, CLOSED), KeyRoute::Pass);
        // 覚えておく数には上限がある（いつまでも増えない）。
        for i in 0..(KeyGate::MAX_HELD as u32 + 4) {
            assert_eq!(gate.on_key(0x41 + i, true, 0, SHOWN), KeyRoute::Forward);
        }
        assert_eq!(gate.on_key(0x41 + KeyGate::MAX_HELD as u32, false, 0, SHOWN), KeyRoute::Pass);
    }
}
