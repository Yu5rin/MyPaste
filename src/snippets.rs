//! 定型文の判断ロジック。
//!
//! 定型文は、よく使う文（あいさつ・住所・口座番号など）を登録しておき、クリップボードの履歴の
//! 一覧の「定型文」タブから選んで貼り付けるもの。`settings.json` の `snippets` に保存する
//! （設定の書き出し・読み込みにも含まれる）。編集する画面は [`crate::snippet_window`]。
//!
//! 定型文には、まとめておくための「グループ」を付けられる（空ならグループ無し）。一覧では
//! `[グループ] 名前` と出し、同じグループの定型文を続けて並べる（グループ無しを先に、
//! グループは初めて出てきた順、グループの中は元の順）。
//!
//! ここでは名前の決め方・並べ方・検証・CSV での読み書きを受け持つ。Win32 API に触れないので、
//! Linux 上でもテストできる。

use crate::clip_history;
use crate::config::Snippet;

/// 登録できる定型文の数の上限。
pub const MAX_SNIPPETS: usize = 1000;
/// 1 つの定型文の本文の文字数の上限。
pub const MAX_TEXT_CHARS: usize = 20_000;
/// 名前の文字数の上限。
pub const MAX_NAME_CHARS: usize = 100;
/// グループ名の文字数の上限。
pub const MAX_GROUP_CHARS: usize = 100;
/// CSV の見出しの行。グループは 3 列目に置く（2 列の古いファイルもそのまま読めるように）。
const CSV_HEADER: [&str; 3] = ["名前", "本文", "グループ"];

/// グループを除いた名前（名前が空なら、本文の最初の行）。
pub fn plain_name(snippet: &Snippet) -> String {
    let name = snippet.name.trim();
    if name.is_empty() {
        clip_history::row_text(&snippet.text).0
    } else {
        name.to_string()
    }
}

/// 一覧に出す名前。グループがあれば `[グループ] 名前` の形にする。
pub fn display_name(snippet: &Snippet) -> String {
    let group = snippet.group.trim();
    if group.is_empty() {
        plain_name(snippet)
    } else {
        format!("[{group}] {}", plain_name(snippet))
    }
}

/// 名前で定型文を探す（キー割り当ての「定型文を貼り付け」）。一覧に出る名前
/// （グループがあれば `[グループ] 名前`）がそのまま合うものを先に探し、無ければ、グループを
/// 除いた名前が合うものが 1 つだけのときにそれを返す（同じ名前が別のグループにもあるときは、
/// どれか決められないので返さない）。
pub fn find_by_name<'a>(list: &'a [Snippet], name: &str) -> Option<&'a Snippet> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if let Some(found) = list.iter().find(|s| display_name(s) == name) {
        return Some(found);
    }
    let mut plain = list.iter().filter(|s| plain_name(s) == name);
    match (plain.next(), plain.next()) {
        (Some(found), None) => Some(found),
        _ => None,
    }
}

/// 一覧にあるグループ（初めて出てきた順。グループ無しは含めない）。
pub fn groups(list: &[Snippet]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for snippet in list {
        let group = snippet.group.trim();
        if !group.is_empty() && !out.iter().any(|g| g == group) {
            out.push(group.to_string());
        }
    }
    out
}

/// 同じグループの定型文が続くように並べ直す。グループ無しを先に、グループは初めて出てきた順、
/// グループの中は元の順のまま。`keep` に位置を渡すと、その定型文の並べ直したあとの位置を返す。
pub fn sort_by_group(list: &mut Vec<Snippet>, keep: Option<usize>) -> Option<usize> {
    let groups = groups(list);
    let rank = |s: &Snippet| {
        let group = s.group.trim();
        if group.is_empty() {
            0
        } else {
            groups.iter().position(|g| g == group).map_or(0, |i| i + 1)
        }
    };
    let mut order: Vec<usize> = (0..list.len()).collect();
    order.sort_by_key(|&i| rank(&list[i]));
    let kept = keep.and_then(|k| order.iter().position(|&i| i == k));
    let mut old: Vec<Option<Snippet>> = std::mem::take(list).into_iter().map(Some).collect();
    *list = order.iter().filter_map(|&i| old[i].take()).collect();
    kept
}

/// 定型文を 1 つ上（`delta` が -1）か下（1）に動かし、動かしたあとの位置を返す。
/// 隣が同じグループならその定型文と入れ替える。隣が別のグループなら、グループごと隣の
/// グループと入れ替える（グループ無しは常に先頭なので、グループ無しとは入れ替えない）。
/// 動かせなければ `None`。`list` は [`sort_by_group`] で並べてあるものとする。
pub fn move_item(list: &mut [Snippet], index: usize, delta: isize) -> Option<usize> {
    let to = index.checked_add_signed(delta).filter(|to| *to < list.len())?;
    let group = list.get(index)?.group.trim().to_string();
    let other = list[to].group.trim().to_string();
    if group == other {
        list.swap(index, to);
        return Some(to);
    }
    if group.is_empty() || other.is_empty() {
        return None;
    }
    // 位置 `at` を含む、同じグループの続き（start..end）。
    let block = |list: &[Snippet], at: usize| {
        let g = list[at].group.trim();
        let mut start = at;
        while start > 0 && list[start - 1].group.trim() == g {
            start -= 1;
        }
        let mut end = at + 1;
        while end < list.len() && list[end].group.trim() == g {
            end += 1;
        }
        (start, end)
    };
    let (own_start, own_end) = block(list, index);
    let (other_start, other_end) = block(list, to);
    let other_len = other_end - other_start;
    if delta < 0 {
        // 上のグループと入れ替える（自分のグループが先に来るよう回す）。
        list[other_start..own_end].rotate_left(other_len);
        Some(index - other_len)
    } else {
        list[own_start..other_end].rotate_left(own_end - own_start);
        Some(index + other_len)
    }
}

/// 名前と本文を検証して定型文にする（グループ無し）。本文の改行は `\n` にそろえる。
pub fn make(name: &str, text: &str) -> Result<Snippet, String> {
    make_in_group("", name, text)
}

/// グループ・名前・本文を検証して定型文にする。本文の改行は `\n` にそろえ、グループと名前の
/// 前後の空白は除く。
pub fn make_in_group(group: &str, name: &str, text: &str) -> Result<Snippet, String> {
    let group = group.trim();
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
    if group.chars().count() > MAX_GROUP_CHARS {
        return Err(format!("グループは {MAX_GROUP_CHARS} 文字までにしてください"));
    }
    if group.contains(['\r', '\n']) {
        return Err("グループに改行は入れられません".into());
    }
    Ok(Snippet {
        group: group.to_string(),
        name: name.to_string(),
        text,
    })
}

/// 手で書き換えた設定ファイルなどの定型文を、[`make_in_group`] と同じ形にそろえる
/// （検証に通らないものは、そのまま返す）。
pub fn normalize(snippet: Snippet) -> Snippet {
    make_in_group(&snippet.group, &snippet.name, &snippet.text).unwrap_or(snippet)
}

/// 定型文を CSV にする（Excel でそのまま開けるよう、先頭に BOM を付け、改行は \r\n）。
/// 1 行目は見出し（名前,本文,グループ）。
pub fn to_csv(snippets: &[Snippet]) -> String {
    let mut out = String::from("\u{feff}");
    out.push_str(&CSV_HEADER.join(","));
    out.push_str("\r\n");
    for snippet in snippets {
        out.push_str(&csv_field(&snippet.name));
        out.push(',');
        out.push_str(&csv_field(&snippet.text.replace("\r\n", "\n").replace('\n', "\r\n")));
        out.push(',');
        out.push_str(&csv_field(&snippet.group));
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

/// CSV から定型文を読む。1 列なら本文だけ、2 列なら「名前,本文」、3 列以上なら
/// 「名前,本文,グループ」として読む（4 列目からは使わない）。
/// 1 行目が見出し（名前,本文 で始まる）なら飛ばす。空の行と本文が空の行は飛ばす。
/// 読んだ定型文は [`sort_by_group`] の順に並べる。
pub fn from_csv(text: &str) -> Result<Vec<Snippet>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rows = parse_csv(text)?;
    let mut snippets = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        if i == 0 && row.len() >= 2 && row[0].trim() == CSV_HEADER[0] && row[1].trim() == CSV_HEADER[1]
        {
            continue;
        }
        let (name, body, group) = match row.as_slice() {
            [] => continue,
            [body] => ("", body.as_str(), ""),
            [name, body] => (name.as_str(), body.as_str(), ""),
            [name, body, group, ..] => (name.as_str(), body.as_str(), group.as_str()),
        };
        if body.trim().is_empty() {
            continue;
        }
        let snippet =
            make_in_group(group, name, body).map_err(|e| format!("{} 行目: {e}", i + 1))?;
        snippets.push(snippet);
        if snippets.len() > MAX_SNIPPETS {
            return Err(format!("定型文は {MAX_SNIPPETS} 件までです"));
        }
    }
    sort_by_group(&mut snippets, None);
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
            // \r\n は 1 つの改行。\r だけ（古い Mac の改行）も改行として扱う。
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' | '\n' => {
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
        g("", name, text)
    }

    fn g(group: &str, name: &str, text: &str) -> Snippet {
        Snippet {
            group: group.into(),
            name: name.into(),
            text: text.into(),
        }
    }

    fn names_of(list: &[Snippet]) -> Vec<String> {
        list.iter().map(display_name).collect()
    }

    #[test]
    fn names() {
        assert_eq!(display_name(&s("  住所 ", "東京都")), "住所");
        assert_eq!(display_name(&s("", "\n  お世話になっております。\n本文")), "お世話になっております。");
        assert_eq!(display_name(&g(" 挨拶 ", " お礼 ", "x")), "[挨拶] お礼");
        assert_eq!(display_name(&g("挨拶", "", "本文の\n2 行目")), "[挨拶] 本文の");
        assert_eq!(plain_name(&g("挨拶", "お礼", "x")), "お礼");
        assert_eq!(display_name(&g("  ", "お礼", "x")), "お礼");
    }

    #[test]
    fn find_by_name_matches_plain_and_grouped() {
        let list = vec![
            s("住所", "東京都"),
            g("挨拶", "お礼", "ありがとうございます"),
            g("挨拶", "住所", "挨拶の住所"),
            g("社外", "署名", "社外の署名"),
            g("社内", "署名", "社内の署名"),
        ];
        let text = |name: &str| find_by_name(&list, name).map(|s| s.text.as_str());
        // グループ無しは今までどおり名前で見つかる（同じ名前がグループにあっても、こちらが先）。
        assert_eq!(text(" 住所 "), Some("東京都"));
        assert_eq!(text("[挨拶] 住所"), Some("挨拶の住所"));
        // グループ付きは、[グループ] 名前 でも、名前が 1 つだけなら名前だけでも見つかる。
        assert_eq!(text("[挨拶] お礼"), Some("ありがとうございます"));
        assert_eq!(text("お礼"), Some("ありがとうございます"));
        // 名前だけでは決められないときは見つからない。
        assert_eq!(text("署名"), None);
        assert_eq!(text("[社内] 署名"), Some("社内の署名"));
        assert_eq!(text("なし"), None);
        assert_eq!(text("  "), None);
    }

    #[test]
    fn sorts_by_group() {
        let mut list = vec![
            g("B", "b1", "x"),
            s("n1", "x"),
            g("A", "a1", "x"),
            g(" B ", "b2", "x"),
            s("n2", "x"),
            g("A", "a2", "x"),
        ];
        assert_eq!(sort_by_group(&mut list, Some(4)), Some(1));
        assert_eq!(names_of(&list), ["n1", "n2", "[B] b1", "[B] b2", "[A] a1", "[A] a2"]);
        assert_eq!(groups(&list), ["B", "A"]);
        // もう並んでいれば変わらない。
        let before = list.clone();
        assert_eq!(sort_by_group(&mut list, None), None);
        assert_eq!(list, before);
        let mut empty = Vec::new();
        assert_eq!(sort_by_group(&mut empty, Some(0)), None);
    }

    #[test]
    fn moves_items_and_groups() {
        let base = vec![
            s("n1", "x"),
            s("n2", "x"),
            g("A", "a1", "x"),
            g("A", "a2", "x"),
            g("B", "b1", "x"),
        ];
        // 同じグループの中は入れ替える。
        let mut list = base.clone();
        assert_eq!(move_item(&mut list, 1, -1), Some(0));
        assert_eq!(names_of(&list)[..2], ["n2", "n1"]);
        // グループ無しとグループの境目は越えない。
        let mut list = base.clone();
        assert_eq!(move_item(&mut list, 1, 1), None);
        assert_eq!(move_item(&mut list, 2, -1), None);
        assert_eq!(list, base);
        // 別のグループの隣では、グループごと入れ替える。
        let mut list = base.clone();
        assert_eq!(move_item(&mut list, 3, 1), Some(4));
        assert_eq!(names_of(&list), ["n1", "n2", "[B] b1", "[A] a1", "[A] a2"]);
        assert_eq!(move_item(&mut list, 3, -1), Some(2));
        assert_eq!(list, base);
        let mut list = base.clone();
        assert_eq!(move_item(&mut list, 4, -1), Some(2));
        assert_eq!(names_of(&list), ["n1", "n2", "[B] b1", "[A] a1", "[A] a2"]);
        // 端からは動かない。
        assert_eq!(move_item(&mut list, 0, -1), None);
        assert_eq!(move_item(&mut list, 4, 1), None);
    }

    #[test]
    fn validates() {
        assert_eq!(make(" a ", "x\r\ny").unwrap(), s("a", "x\ny"));
        assert!(make("a", "  \n").is_err());
        assert!(make(&"n".repeat(101), "x").is_err());
        assert!(make("", &"x".repeat(MAX_TEXT_CHARS + 1)).is_err());
        assert_eq!(make_in_group(" 挨拶 ", " a ", "x").unwrap(), g("挨拶", "a", "x"));
        assert!(make_in_group(&"g".repeat(MAX_GROUP_CHARS + 1), "a", "x").is_err());
        assert!(make_in_group("a\nb", "a", "x").is_err());
        assert_eq!(normalize(g(" A ", " a ", "x\r\ny")), g("A", "a", "x\ny"));
        let bad = g("", "", "  ");
        assert_eq!(normalize(bad.clone()), bad);
    }

    #[test]
    fn csv_round_trip() {
        let list = vec![
            s("あいさつ", "お世話になっております。\n株式会社サンプルの山田です。"),
            s("", "カンマ, と \"引用符\" を含む"),
            s("口座", "1234567"),
            g("挨拶", "お礼", "ありがとうございます"),
            g("社外, \"取引先\"", "", "引用符 \"\" も"),
        ];
        let csv = to_csv(&list);
        assert!(csv.starts_with("\u{feff}名前,本文,グループ\r\n"));
        assert!(csv.contains("お礼,ありがとうございます,挨拶\r\n"));
        assert!(csv.contains("口座,1234567,\r\n"));
        assert_eq!(from_csv(&csv).unwrap(), list);
    }

    #[test]
    fn csv_old_layout() {
        // グループの列が無い、前の版で書き出した CSV。
        let old = "\u{feff}名前,本文\r\nあいさつ,\"お世話に\r\nなっております\"\r\n口座,1234567\r\n";
        assert_eq!(
            from_csv(old).unwrap(),
            vec![s("あいさつ", "お世話に\nなっております"), s("口座", "1234567")]
        );
        // 3 列目がグループ。読んだものはグループごとに並べる。
        let new = "名前,本文,グループ\nb1,x,B\nn1,x,\na1,x,A\nb2,x, B \n";
        assert_eq!(names_of(&from_csv(new).unwrap()), ["n1", "[B] b1", "[B] b2", "[A] a1"]);
        assert!(from_csv("a,x,\"改行の\nあるグループ\"").is_err());
    }

    #[test]
    fn csv_variants() {
        // 見出し無し・1 列だけ・空行・\n だけの改行・4 列目からは使わない。
        let list = from_csv("本文だけ\n\n名前,本文2,グループ,余分\n,\n").unwrap();
        assert_eq!(list, vec![s("", "本文だけ"), g("グループ", "名前", "本文2")]);
        assert!(from_csv("\"閉じていない").is_err());
        // \r だけの改行も行の区切りとして読む。
        assert_eq!(from_csv("a,b\rc,d").unwrap(), vec![s("a", "b"), s("c", "d")]);
        assert!(from_csv("").unwrap().is_empty());
        let many: String = (0..=MAX_SNIPPETS).map(|i| format!("{i}\n")).collect();
        assert!(from_csv(&many).is_err());
    }
}
