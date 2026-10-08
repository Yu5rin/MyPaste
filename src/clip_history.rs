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
pub const MAX_ITEMS_LIMIT: usize = 100;
pub const MIN_ITEMS: usize = 10;
pub const DEFAULT_ITEMS: usize = 100;
/// 1 件として覚える文字数の上限（大きな表をコピーしたときにメモリやファイルを使いすぎないように）。
pub const MAX_CHARS: usize = 50_000;
/// 2 回押しの間隔の範囲と既定値（ミリ秒）。
pub const DOUBLE_TAP_MS_MIN: u32 = 200;
pub const DOUBLE_TAP_MS_MAX: u32 = 1000;
pub const DEFAULT_DOUBLE_TAP_MS: u32 = 400;
/// 一覧の 1 行に出す文字数の上限（実際には欄の幅に合わせて「…」で切る）。
const ROW_CHARS: usize = 200;
/// 一覧の 1 ページに並べる件数。
pub const PAGE_SIZE: usize = 20;
/// 全文の吹き出しに出す行数と、1 行の文字数の上限。
const TIP_LINES: usize = 20;
const TIP_LINE_CHARS: usize = 200;

/// コピーされた文字の履歴（新しい順）。
#[derive(Debug)]
pub struct History {
    items: VecDeque<String>,
    max: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::new(DEFAULT_ITEMS)
    }
}

impl History {
    pub fn new(max: usize) -> Self {
        Self {
            items: VecDeque::new(),
            max: clamp_items(max),
        }
    }

    /// 保存しておいた履歴（新しい順）から作る。条件に合わないものと重複は除く。
    pub fn from_saved(saved: Vec<String>, max: usize) -> Self {
        let mut history = Self::new(max);
        // 古いものから順に入れると、新しいものが先頭に来て、重複は新しい方が残る。
        for text in saved.iter().rev() {
            history.push(text);
        }
        history
    }

    /// 覚えておく件数を変える（減らしたときは古いものから消す）。
    pub fn set_max(&mut self, max: usize) {
        self.max = clamp_items(max);
        self.items.truncate(self.max);
    }

    /// コピーされた文字を加える。空白だけの文字や、長すぎる文字は覚えない。
    /// すでにある文字なら、前のものを消していちばん新しい位置へ移す。
    /// 履歴が変わったら `true`。
    pub fn push(&mut self, text: &str) -> bool {
        if text.trim().is_empty() || text.chars().count() > MAX_CHARS {
            return false;
        }
        if self.items.front().is_some_and(|t| t == text) {
            return false;
        }
        if let Some(i) = self.items.iter().position(|t| t == text) {
            self.items.remove(i);
        }
        self.items.push_front(text.to_string());
        self.items.truncate(self.max);
        true
    }

    pub fn items(&self) -> impl Iterator<Item = &String> {
        self.items.iter()
    }

    /// 1 件を消す（一覧で消したとき）。消えたら `true`。
    pub fn remove(&mut self, text: &str) -> bool {
        match self.items.iter().position(|t| t == text) {
            Some(i) => {
                self.items.remove(i);
                true
            }
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

/// 件数を範囲に収める。
pub fn clamp_items(max: usize) -> usize {
    max.clamp(MIN_ITEMS, MAX_ITEMS_LIMIT)
}

/// 履歴をファイルに保存する形（JSON の文字列の配列、新しい順）にする。
pub fn to_json(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".to_string())
}

/// 保存しておいた形から読む。読めなければ空。
pub fn from_json(text: &str) -> Vec<String> {
    serde_json::from_str(text).unwrap_or_default()
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

/// 一覧を出す位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuPosition {
    /// 入力位置（キャレット）の近く。分からなければマウスの近く。
    Caret,
    /// マウスの近く。
    Mouse,
}

impl MenuPosition {
    /// (値, settings.json に書く名前, 画面に出す名前)
    pub const ALL: [(MenuPosition, &'static str, &'static str); 2] = [
        (MenuPosition::Caret, "caret", "入力位置の近く"),
        (MenuPosition::Mouse, "mouse", "マウスの近く"),
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
    /// キーが押された・離された（`time` はミリ秒の時刻。Windows のキーボードフックの値）。
    /// 2 回押しが成立したら `true`。
    pub fn on_key(&mut self, key: Modifier, vk: u32, down: bool, time: u32, interval_ms: u32) -> bool {
        if !key.matches(vk) {
            // ほかのキーが押されたら、組み合わせとみなして数え直す。
            if down {
                *self = Self::default();
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

/// ページの数（0 件でも 1 ページ）。
pub fn page_count(len: usize) -> usize {
    len.div_ceil(PAGE_SIZE).max(1)
}

/// `page` ページ目（0 から）に並べる位置の範囲。
pub fn page_range(page: usize, len: usize) -> std::ops::Range<usize> {
    let start = (page * PAGE_SIZE).min(len);
    start..(start + PAGE_SIZE).min(len)
}

/// ページの見出し（例 `1〜20 件目 / 100 件`、絞り込み中は `1〜3 件目 / 3 件（絞り込み）`）。
pub fn page_label(page: usize, len: usize, filtered: bool) -> String {
    let range = page_range(page, len);
    let tail = if filtered { "（絞り込み）" } else { "" };
    if range.is_empty() {
        return format!("0 件{tail}");
    }
    format!("{}〜{} 件目 / {len} 件{tail}", range.start + 1, range.end)
}

/// 絞り込み。`query` を空白で区切った語を**すべて**含むものの位置を返す（大文字・小文字、
/// 全角・半角の英数字は区別しない）。`query` が空ならすべて。
pub fn filter<'a>(items: impl IntoIterator<Item = &'a String>, query: &str) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(normalize).collect();
    items
        .into_iter()
        .enumerate()
        .filter(|(_, text)| {
            if words.is_empty() {
                return true;
            }
            let text = normalize(text);
            words.iter().all(|w| text.contains(w.as_str()))
        })
        .map(|(i, _)| i)
        .collect()
}

/// 絞り込み用に、全角の英数字・記号を半角に、英字を小文字にそろえる。
fn normalize(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{3000}' => ' ',
            _ => c,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut h = History::default();
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
        assert_eq!(History::new(1000).max, MAX_ITEMS_LIMIT);
    }

    #[test]
    fn saved_round_trip() {
        let mut h = History::default();
        h.push("古い");
        h.push("新しい\r\n2 行目");
        let saved = from_json(&to_json(&items(&h)));
        let restored = History::from_saved(saved, 100);
        assert_eq!(items(&restored), items(&h));
        // 重複や空のものが混じっていても、新しい方を残して読む。
        let restored =
            History::from_saved(vec!["a".into(), " ".into(), "b".into(), "a".into()], 100);
        assert_eq!(items(&restored), ["a", "b"]);
        assert!(from_json("壊れた").is_empty());
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
        assert_eq!(config.max_items, 100);
        assert!(config.keep_after_exit);
        settings.trigger = "double_ctrl".into();
        settings.max_items = 500;
        settings.double_tap_ms = 50;
        let config = Config::from_settings(&settings);
        assert_eq!(config.trigger, Trigger::DoubleCtrl);
        assert_eq!(config.hotkey, None);
        assert_eq!(config.max_items, 100);
        assert_eq!(config.double_tap_ms, DOUBLE_TAP_MS_MIN);
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
        assert_eq!(page_count(0), 1);
        assert_eq!(page_count(20), 1);
        assert_eq!(page_count(21), 2);
        assert_eq!(page_count(100), 5);
        assert_eq!(page_range(1, 25), 20..25);
        assert_eq!(page_range(3, 25), 25..25);
        assert_eq!(page_label(0, 100, false), "1〜20 件目 / 100 件");
        assert_eq!(page_label(1, 25, true), "21〜25 件目 / 25 件（絞り込み）");
        assert_eq!(page_label(0, 0, true), "0 件（絞り込み）");
    }

    #[test]
    fn filters() {
        let items: Vec<String> = ["Hello World", "ＡＢＣ商事", "見積書 2026", "hello"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(filter(&items, ""), [0, 1, 2, 3]);
        assert_eq!(filter(&items, "HELLO"), [0, 3]);
        assert_eq!(filter(&items, "abc"), [1]);
        assert_eq!(filter(&items, "見積　２０２６"), [2]);
        assert_eq!(filter(&items, "hello world"), [0]);
        assert!(filter(&items, "なし").is_empty());
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
}
