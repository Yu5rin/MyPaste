//! 定型文の判断ロジック。
//!
//! 定型文は、よく使う文（あいさつ・住所・口座番号など）を登録しておき、クリップボードの履歴の
//! 一覧の「定型文」タブから選んで貼り付けるもの。`settings.json` の `snippets` に保存する
//! （設定の書き出し・読み込みにも含まれる）。編集する画面は [`crate::snippet_window`]。
//!
//! ここでは名前の決め方・検証・CSV での読み書きを受け持つ。Win32 API に触れないので、
//! Linux 上でもテストできる。

use crate::clip_history;
use crate::config::Snippet;

/// 登録できる定型文の数の上限。
pub const MAX_SNIPPETS: usize = 1000;
/// 1 つの定型文の本文の文字数の上限。
pub const MAX_TEXT_CHARS: usize = 20_000;
/// 名前の文字数の上限。
pub const MAX_NAME_CHARS: usize = 100;
/// CSV の見出しの行。
const CSV_HEADER: [&str; 2] = ["名前", "本文"];

/// 一覧に出す名前（名前が空なら、本文の最初の行）。
pub fn display_name(snippet: &Snippet) -> String {
    let name = snippet.name.trim();
    if name.is_empty() {
        clip_history::row_text(&snippet.text).0
    } else {
        name.to_string()
    }
}

/// 名前と本文を検証して定型文にする。本文の改行は `\n` にそろえる。
pub fn make(name: &str, text: &str) -> Result<Snippet, String> {
    let name = name.trim();
    let text = text.replace("\r\n", "\n");
    if text.trim().is_empty() {
        return Err("本文を入力してください".into());
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(format!("本文は {MAX_TEXT_CHARS} 文字までにしてください"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("名前は {MAX_NAME_CHARS} 文字までにしてください"));
    }
    Ok(Snippet {
        name: name.to_string(),
        text,
    })
}

/// 定型文を CSV にする（Excel でそのまま開けるよう、先頭に BOM を付け、改行は \r\n）。
/// 1 行目は見出し（名前,本文）。
pub fn to_csv(snippets: &[Snippet]) -> String {
    let mut out = String::from("\u{feff}");
    out.push_str(&CSV_HEADER.join(","));
    out.push_str("\r\n");
    for snippet in snippets {
        out.push_str(&csv_field(&snippet.name));
        out.push(',');
        out.push_str(&csv_field(&snippet.text.replace("\r\n", "\n").replace('\n', "\r\n")));
        out.push_str("\r\n");
    }
    out
}

/// CSV の 1 つの欄。カンマ・引用符・改行を含むときは " で囲み、" は "" にする。
fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

/// CSV から定型文を読む。1 列なら本文だけ、2 列以上なら「名前,本文」として読む。
/// 1 行目が見出し（名前,本文）なら飛ばす。空の行と本文が空の行は飛ばす。
pub fn from_csv(text: &str) -> Result<Vec<Snippet>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rows = parse_csv(text)?;
    let mut snippets = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        if i == 0 && row.len() >= 2 && row[0].trim() == CSV_HEADER[0] && row[1].trim() == CSV_HEADER[1]
        {
            continue;
        }
        let (name, body) = match row.as_slice() {
            [] => continue,
            [body] => ("", body.as_str()),
            [name, body, ..] => (name.as_str(), body.as_str()),
        };
        if body.trim().is_empty() {
            continue;
        }
        let snippet = make(name, body).map_err(|e| format!("{} 行目: {e}", i + 1))?;
        snippets.push(snippet);
        if snippets.len() > MAX_SNIPPETS {
            return Err(format!("定型文は {MAX_SNIPPETS} 件までです"));
        }
    }
    Ok(snippets)
}

/// CSV を行と欄に分ける（" で囲んだ欄の中のカンマ・改行・"" に対応）。
fn parse_csv(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    if quoted {
        return Err("\" で始まった欄が閉じられていません".into());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    // 中身の無い行（空行）は除く。
    rows.retain(|r| r.iter().any(|f| !f.is_empty()));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(name: &str, text: &str) -> Snippet {
        Snippet {
            name: name.into(),
            text: text.into(),
        }
    }

    #[test]
    fn names() {
        assert_eq!(display_name(&s("  住所 ", "東京都")), "住所");
        assert_eq!(display_name(&s("", "\n  お世話になっております。\n本文")), "お世話になっております。");
    }

    #[test]
    fn validates() {
        assert_eq!(make(" a ", "x\r\ny").unwrap(), s("a", "x\ny"));
        assert!(make("a", "  \n").is_err());
        assert!(make(&"n".repeat(101), "x").is_err());
        assert!(make("", &"x".repeat(MAX_TEXT_CHARS + 1)).is_err());
    }

    #[test]
    fn csv_round_trip() {
        let list = vec![
            s("あいさつ", "お世話になっております。\n株式会社サンプルの山田です。"),
            s("", "カンマ, と \"引用符\" を含む"),
            s("口座", "1234567"),
        ];
        let csv = to_csv(&list);
        assert!(csv.starts_with("\u{feff}名前,本文\r\n"));
        assert_eq!(from_csv(&csv).unwrap(), list);
    }

    #[test]
    fn csv_variants() {
        // 見出し無し・1 列だけ・空行・\n だけの改行。
        let list = from_csv("本文だけ\n\n名前,本文2,余分\n,\n").unwrap();
        assert_eq!(list, vec![s("", "本文だけ"), s("名前", "本文2")]);
        assert!(from_csv("\"閉じていない").is_err());
        assert!(from_csv("").unwrap().is_empty());
        let many: String = (0..=MAX_SNIPPETS).map(|i| format!("{i}\n")).collect();
        assert!(from_csv(&many).is_err());
    }
}
