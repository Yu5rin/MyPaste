//! 「文字を整えて貼り付け」の整え方。
//!
//! クリップボードの文字に、選んだ整え方を決まった順に当ててから貼り付ける
//! （[`crate::actions`]）。Win32 API に触れないので、Linux 上でもテストできる。
//!
//! 整え方は `settings.json` では `"trim,zen_to_han"` のようにカンマ区切りの名前で書く。

/// 整え方の種類。並び順がそのまま当てる順になる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    /// 全角の英数字・記号・空白を半角にする
    ZenToHan,
    /// 半角カタカナを全角にする（濁点・半濁点はまとめる）
    HanKanaToZen,
    /// ▲ △ や (1,234) の負数の書き方を -1234 のようにマイナスにする
    NegativeMarks,
    /// 数字の桁区切り（1,234 の ,）を除く
    RemoveThousandsSeparators,
    /// 各行の前後の空白を除く
    TrimLines,
    /// 空の行を除く
    RemoveBlankLines,
    /// 連続する空白を 1 つにする
    CollapseSpaces,
    /// 空白をすべて除く
    RemoveAllSpaces,
    /// 改行を除いて 1 行にする
    JoinLines,
    /// 全体の前後の空白・改行を除く
    Trim,
}

/// 整え方の一覧（設定値の名前, 画面に出す名前）。並び順は当てる順で、画面の並びにも使う。
pub const TRANSFORMS: [(Transform, &str, &str); 10] = [
    (Transform::ZenToHan, "zen_to_han", "全角英数・記号を半角に"),
    (Transform::HanKanaToZen, "han_kana_to_zen", "半角カタカナを全角に"),
    (Transform::NegativeMarks, "negative_marks", "▲ △ (123) をマイナスに"),
    (Transform::RemoveThousandsSeparators, "remove_commas", "桁区切りの , を除く"),
    (Transform::TrimLines, "trim_lines", "各行の前後の空白を除く"),
    (Transform::RemoveBlankLines, "remove_blank_lines", "空の行を除く"),
    (Transform::CollapseSpaces, "collapse_spaces", "続く空白を 1 つに"),
    (Transform::RemoveAllSpaces, "remove_spaces", "空白をすべて除く"),
    (Transform::JoinLines, "join_lines", "改行を除いて 1 行に"),
    (Transform::Trim, "trim", "全体の前後の空白を除く"),
];

impl Transform {
    pub fn key(self) -> &'static str {
        TRANSFORMS.iter().find(|(t, _, _)| *t == self).map(|(_, k, _)| *k).unwrap_or("")
    }
}

/// `"trim,zen_to_han"` を読む。知らない名前があれば `Err`（その名前）。並びは当てる順にそろえ、
/// 重複は除く。
pub fn parse_list(text: &str) -> Result<Vec<Transform>, String> {
    let mut found = Vec::new();
    for name in text.split([',', ' ', '\n', '\r']).map(str::trim).filter(|n| !n.is_empty()) {
        let transform = TRANSFORMS
            .iter()
            .find(|(_, key, _)| key.eq_ignore_ascii_case(name))
            .map(|(t, _, _)| *t)
            .ok_or_else(|| format!("整え方「{name}」には対応していません"))?;
        if !found.contains(&transform) {
            found.push(transform);
        }
    }
    Ok(TRANSFORMS
        .iter()
        .map(|(t, _, _)| *t)
        .filter(|t| found.contains(t))
        .collect())
}

/// 整え方の一覧を `"trim,zen_to_han"` の形にする。
pub fn format_list(transforms: &[Transform]) -> String {
    transforms.iter().map(|t| t.key()).collect::<Vec<_>>().join(",")
}

/// 文字を整える。
pub fn apply(text: &str, transforms: &[Transform]) -> String {
    // 改行は \n にそろえてから処理し、最後に \r\n に戻す（Windows の貼り付け先の多くは \r\n）。
    let mut text = text.replace("\r\n", "\n").replace('\r', "\n");
    for transform in TRANSFORMS.iter().map(|(t, _, _)| *t) {
        if !transforms.contains(&transform) {
            continue;
        }
        text = match transform {
            Transform::ZenToHan => zen_to_han(&text),
            Transform::HanKanaToZen => han_kana_to_zen(&text),
            Transform::NegativeMarks => negative_marks(&text),
            Transform::RemoveThousandsSeparators => remove_thousands_separators(&text),
            Transform::TrimLines => map_lines(&text, |l| l.trim_matches(is_space).to_string()),
            Transform::RemoveBlankLines => {
                // 最後の改行は残す（コピーした範囲の終わりの改行まで消さない）。
                let ends_with_newline = text.ends_with('\n');
                let mut joined = text
                    .split('\n')
                    .filter(|l| !l.chars().all(is_space))
                    .collect::<Vec<_>>()
                    .join("\n");
                if ends_with_newline && !joined.is_empty() {
                    joined.push('\n');
                }
                joined
            }
            Transform::CollapseSpaces => map_lines(&text, collapse_spaces),
            Transform::RemoveAllSpaces => text.chars().filter(|c| !is_space(*c)).collect(),
            Transform::JoinLines => text.split('\n').collect::<String>(),
            Transform::Trim => text.trim_matches(|c: char| is_space(c) || c == '\n').to_string(),
        };
    }
    text.replace('\n', "\r\n")
}

/// 空白とみなす文字（半角・全角の空白とタブ）。
fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\u{3000}' | '\t')
}

fn map_lines(text: &str, f: impl Fn(&str) -> String) -> String {
    text.split('\n').map(f).collect::<Vec<_>>().join("\n")
}

fn collapse_spaces(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut previous_space = false;
    for c in line.chars() {
        if is_space(c) {
            if !previous_space {
                out.push(' ');
            }
            previous_space = true;
        } else {
            out.push(c);
            previous_space = false;
        }
    }
    out
}

/// 全角の英数字・記号（！〜～。全角のマイナス「－」を含む）と全角空白を半角にする。
/// 円記号（￥）と長音記号（ー）はそのまま残す。
fn zen_to_han(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            // ！(FF01)〜～(FF5E) は ASCII の !〜~ に対応する
            '\u{FF01}'..='\u{FF5E}' if c != '￥' => {
                char::from_u32(c as u32 - 0xFF01 + 0x21).unwrap_or(c)
            }
            c => c,
        })
        .collect()
}

/// 半角カタカナ（ｱ など）を全角にする。濁点（ﾞ）・半濁点（ﾟ）は前の文字とまとめる。
fn han_kana_to_zen(text: &str) -> String {
    const TABLE: &str = "。「」、・ヲァィゥェォャュョッーアイウエオカキクケコサシスセソタチツテトナニヌネノハヒフヘホマミムメモヤユヨラリルレロワン";
    let table: Vec<char> = TABLE.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let code = c as u32;
        if !(0xFF61..=0xFF9D).contains(&code) {
            match c {
                'ﾞ' => out.push('゛'),
                'ﾟ' => out.push('゜'),
                c => out.push(c),
            }
            continue;
        }
        let base = table[(code - 0xFF61) as usize];
        match chars.peek() {
            Some('ﾞ') => {
                if let Some(voiced) = add_mark(base, 1) {
                    out.push(voiced);
                    chars.next();
                    continue;
                }
            }
            Some('ﾟ') => {
                if let Some(semi) = add_mark(base, 2) {
                    out.push(semi);
                    chars.next();
                    continue;
                }
            }
            _ => {}
        }
        out.push(base);
    }
    out
}

/// カタカナに濁点（1）・半濁点（2）を付けた文字。付けられなければ `None`。
fn add_mark(base: char, mark: u32) -> Option<char> {
    if base == 'ウ' && mark == 1 {
        return Some('ヴ');
    }
    let code = base as u32;
    let voiced = matches!(base, 'カ'..='ト' | 'ハ'..='ホ');
    if mark == 1 && voiced && "カキクケコサシスセソタチツテトハヒフヘホ".contains(base) {
        return char::from_u32(code + 1);
    }
    if mark == 2 && "ハヒフヘホ".contains(base) {
        return char::from_u32(code + 2);
    }
    None
}

/// 会計でよく使う負数の書き方を、マイナス記号に直す。
/// - `▲1,234` / `△1,234` → `-1,234`
/// - `(1,234)` / `（1,234）` → `-1,234`（中身が数字だけのとき）
fn negative_marks(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let is_digit = |c: char| c.is_ascii_digit() || ('０'..='９').contains(&c);
    let is_word = |c: char| c.is_alphanumeric();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // ▲・△ は、すぐあと（空白をはさんでも）に数字があるときだけ（見出しの記号などは残す）。
        if c == '▲' || c == '△' {
            let next = chars[i + 1..].iter().find(|c| !matches!(c, ' ' | '\u{3000}'));
            if next.is_some_and(|&n| is_digit(n)) {
                out.push('-');
                i += 1;
                continue;
            }
        }
        // (123) は、前が文字や数字でなく（f(2) などを除く）、後ろが行末・タブ・空白のあとの
        // 数字や行末・「円」のとき（「(1) 項目」のような箇条書きの番号を除く）。
        if (c == '(' || c == '（') && !(i > 0 && is_word(chars[i - 1])) {
            let close = if c == '(' { ')' } else { '）' };
            // 閉じ括弧は近くだけを探す（長い文で何度も最後まで探さないように）。
            let window = &chars[i + 1..chars.len().min(i + 1 + 32)];
            if let Some(len) = window.iter().position(|&x| x == close) {
                let inner = &chars[i + 1..i + 1 + len];
                let looks_numeric = inner.iter().any(|&c| is_digit(c))
                    && inner.iter().all(|&c| is_digit(c) || ",.，．".contains(c));
                let after = &chars[i + 2 + len..];
                let ends_cell = match after.first() {
                    None | Some('\t' | '\r' | '\n' | '円') => true,
                    Some(' ' | '\u{3000}') => {
                        let rest = after.iter().find(|c| !matches!(c, ' ' | '\u{3000}'));
                        rest.is_none_or(|&r| is_digit(r) || matches!(r, '\t' | '\r' | '\n' | '(' | '（' | '▲' | '△' | '-'))
                    }
                    _ => false,
                };
                if looks_numeric && ends_cell {
                    out.push('-');
                    out.extend(inner);
                    i += len + 2;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// 数字にはさまれた桁区切りのカンマ（1,234）を除く。文中の読点のようなカンマ（a, b）は残す。
fn remove_thousands_separators(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let digit = |c: char| c.is_ascii_digit() || ('０'..='９').contains(&c);
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        if (c == ',' || c == '，')
            && i > 0
            && digit(chars[i - 1])
            && chars.get(i + 1..i + 4).is_some_and(|next| next.iter().all(|&d| digit(d)))
            && !chars.get(i + 4).is_some_and(|&d| digit(d))
        {
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, keys: &str) -> String {
        apply(text, &parse_list(keys).unwrap())
    }

    #[test]
    fn parses_and_orders() {
        assert_eq!(
            format_list(&parse_list("trim, zen_to_han,trim").unwrap()),
            "zen_to_han,trim"
        );
        assert!(parse_list("fly").is_err());
        assert!(parse_list("").unwrap().is_empty());
    }

    #[test]
    fn zen_to_han_converts_alnum_and_space() {
        assert_eq!(run("ＡＢＣ　１２３！＠", "zen_to_han"), "ABC 123!@");
        // かな・漢字・長音・円記号はそのまま。全角のマイナスは半角に
        assert_eq!(run("テスト－ー漢字￥", "zen_to_han"), "テスト-ー漢字￥");
    }

    #[test]
    fn han_kana_to_zen_merges_marks() {
        assert_eq!(run("ｶﾞｷﾞｸﾞ ﾊﾟﾋﾟ ｳﾞｧ ｱｲｳ｡", "han_kana_to_zen"), "ガギグ パピ ヴァ アイウ。");
        assert_eq!(run("ｱﾞ", "han_kana_to_zen"), "ア゛");
    }

    #[test]
    fn negative_marks_become_minus() {
        assert_eq!(run("▲1,234 △5", "negative_marks"), "-1,234 -5");
        assert_eq!(run("(1,234) （５６）", "negative_marks"), "-1,234 -５６");
        // 数字でない括弧はそのまま
        assert_eq!(run("(注) (a1)", "negative_marks"), "(注) (a1)");
        // 箇条書きの番号・関数の引数・数字の無い ▲ はそのまま
        assert_eq!(run("(1) 項目", "negative_marks"), "(1) 項目");
        assert_eq!(run("f(2)", "negative_marks"), "f(2)");
        assert_eq!(run("▲ページ先頭", "negative_marks"), "▲ページ先頭");
        assert_eq!(run("▲ 500\t(300)円", "negative_marks"), "- 500\t-300円");
    }

    #[test]
    fn removes_only_thousands_commas() {
        assert_eq!(run("1,234,567円", "remove_commas"), "1234567円");
        assert_eq!(run("a, b, 1,2", "remove_commas"), "a, b, 1,2");
        assert_eq!(run("12,3456", "remove_commas"), "12,3456");
    }

    #[test]
    fn line_and_space_cleanups() {
        assert_eq!(run("  a  \r\n\r\n　b　\r\n", "trim_lines,remove_blank_lines"), "a\r\nb\r\n");
        assert_eq!(run("a   b\t\tc", "collapse_spaces"), "a b c");
        assert_eq!(run("1 2　3\t4", "remove_spaces"), "1234");
        assert_eq!(run("あいう\r\nえお\n", "join_lines"), "あいうえお");
        assert_eq!(run("\r\n  abc  \r\n", "trim"), "abc");
    }

    #[test]
    fn combined_for_excel_numbers() {
        assert_eq!(
            run("　▲１，２３４　\r\n", "zen_to_han,negative_marks,remove_commas,trim"),
            "-1234"
        );
    }
}
