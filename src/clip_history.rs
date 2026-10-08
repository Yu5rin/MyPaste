//! クリップボードの履歴（キー割り当て「クリップボードの履歴から貼り付け」用）。
//!
//! コピーされた文字を新しい順に覚えておく。覚えるのはアプリが動いている間だけ（メモリのみ）で、
//! ファイルには書かない。記録するかどうか・いつ記録するかは [`crate::actions`] が決める。
//! Win32 API に触れないので、Linux 上でもテストできる。

use std::collections::VecDeque;

/// 覚えておく件数。
pub const MAX_ITEMS: usize = 20;
/// 1 件として覚える文字数の上限（大きな表をコピーしたときにメモリを使いすぎないように）。
pub const MAX_CHARS: usize = 100_000;
/// 一覧に出すときの文字数。
const LABEL_CHARS: usize = 40;

/// コピーされた文字の履歴（新しい順）。
#[derive(Debug, Default)]
pub struct History {
    items: VecDeque<String>,
}

impl History {
    /// コピーされた文字を加える。空白だけの文字や、長すぎる文字は覚えない。
    /// すでにある文字なら、いちばん新しい位置へ移す。
    pub fn push(&mut self, text: &str) {
        if text.trim().is_empty() || text.chars().count() > MAX_CHARS {
            return;
        }
        if let Some(i) = self.items.iter().position(|t| t == text) {
            self.items.remove(i);
        }
        self.items.push_front(text.to_string());
        self.items.truncate(MAX_ITEMS);
    }

    pub fn items(&self) -> impl Iterator<Item = &String> {
        self.items.iter()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

/// 一覧に出す 1 行。先頭に数字キーで選べる印（`&1`〜`&9`、10 件目は `&0`）を付け、
/// 最初の行を短くして出す。複数行なら行数も添える。
pub fn menu_label(index: usize, text: &str) -> String {
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    // メニューの & は印として扱われるので、文字の & は && にする。タブは見づらいので空白に。
    let mut short: String = first.chars().take(LABEL_CHARS).collect::<String>().replace('&', "&&");
    short = short.replace('\t', " ");
    if first.chars().count() > LABEL_CHARS {
        short.push('…');
    }
    let lines = text.trim_end_matches(['\r', '\n']).lines().count();
    let more = if lines > 1 {
        format!("（{lines} 行）")
    } else {
        String::new()
    };
    let key = match index {
        0..=8 => format!("&{} ", index + 1),
        9 => "&0 ".to_string(),
        _ => "   ".to_string(),
    };
    format!("{key}{short}{more}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_and_no_duplicates() {
        let mut h = History::default();
        h.push("a");
        h.push("b");
        h.push("a");
        assert_eq!(h.items().cloned().collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn ignores_blank_and_huge() {
        let mut h = History::default();
        h.push("   \r\n");
        h.push(&"x".repeat(MAX_CHARS + 1));
        assert_eq!(h.items().count(), 0);
    }

    #[test]
    fn keeps_at_most_max_items() {
        let mut h = History::default();
        for i in 0..30 {
            h.push(&i.to_string());
        }
        assert_eq!(h.items().count(), MAX_ITEMS);
        assert_eq!(h.items().next().unwrap(), "29");
    }

    #[test]
    fn labels() {
        assert_eq!(menu_label(0, "hello"), "&1 hello");
        assert_eq!(menu_label(9, "x"), "&0 x");
        assert_eq!(menu_label(12, "x"), "   x");
        assert_eq!(menu_label(1, "\r\n  A&B\r\n2行目\r\n"), "&2 A&&B（3 行）");
        let long = "あ".repeat(50);
        assert_eq!(menu_label(2, &long), format!("&3 {}…", "あ".repeat(40)));
    }
}
