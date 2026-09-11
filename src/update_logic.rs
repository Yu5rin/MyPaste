//! 更新確認のうち、外の世界に触れない部分だけを集めたもの。
//!
//! 通信（WinHTTP）・ファイル・ログ・ダイアログから切り離してあるので、値を渡して
//! 戻り値を見るだけで試せる（下記 `tests`）。このファイルは Win32 API に一切触れない
//! ため、Windows 以外のホスト上でも `cargo test` でそのまま検証できる（このリポジトリは
//! `tray-item` が Windows 専用のため、クレート全体の `cargo test` はそのままでは動かない。
//! 判断の中身をここへ寄せることで、この部分だけは切り出して検証できるようにしている）。
//!
//! ## なぜこれが要るか
//!
//! GitHub の Releases API（`https://api.github.com/...`）には未認証の場合
//! **1 時間 60 回**という上限があり、これは端末ごとではなく **IP アドレスごと**に
//! 数えられる。会社などの共有回線では他の通信で先に使い切られ、更新確認のたびに
//! HTTP 403 が返ることがある。
//!
//! `https://github.com/{owner}/{repo}/releases.atom`（Atom フィード）はこの上限とは
//! **別枠**であるため、普段の「新しい版があるか」の確認はこちらに寄せる。
//! API を呼ぶのは Atom で新しい版が見つかったとき（添付ファイルの詳細・SHA256 が
//! 要るとき）だけにし、それでも API が失敗した場合は、規則から組み立てられる
//! ダウンロード URL で続行する（SHA256 の照合は省く）。
//!
//! 詳しい経緯は `/home/user/yu5rin/pane` リポジトリの `UpdateCheckLogic.cs` を参照
//! （このリポジトリの実装は同じ考え方を Rust に移植したもの）。

/// 三つ組みのバージョン番号。
///
/// 比較は必ず**数値として**行う。文字列比較では `1.0.10` < `1.0.9` と
/// 誤判定してしまうため。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Version {
    /// `v1.2.3` / `1.2.3` / `1.2` などを解析する。
    /// `1.2.3-beta` のような接尾辞は無視して数値部分だけを見る。
    /// 版として読めないタグ（`nightly`、`wip` など）には `None` を返す。
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim();
        let t = t.strip_prefix('v').or_else(|| t.strip_prefix('V')).unwrap_or(t);
        // ハイフン以降（プレリリース識別子）とビルドメタデータは切り捨てる。
        let t = t.split(['-', '+']).next()?;
        if t.is_empty() {
            return None;
        }

        let mut parts = t.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().ok()?;
        let patch = parts.next().unwrap_or("0").parse().ok()?;
        Some(Self {
            major,
            minor,
            patch,
        })
    }

    /// このビルドのバージョン。
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self {
            major: 0,
            minor: 0,
            patch: 0,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// `settings.json` の `api_url`
/// （`https://api.github.com/repos/{owner}/{repo}/releases/latest`）から、
/// 同じリポジトリの Atom フィードの URL を組み立てる。
///
/// ホストが `api.github.com` でない場合や、パスの形が
/// `repos/{owner}/{repo}/releases/...` でない場合は `None` を返す
/// （利用者が配布元を差し替えている場合に、確認そのものを壊さないため。
/// その場合は呼び出し側が従来どおり API だけで確認する）。
pub fn atom_url_from_api_url(api_url: &str) -> Option<String> {
    let rest = api_url.strip_prefix("https://api.github.com/repos/")?;
    // クエリ・フラグメントが付いていても無視する。
    let path = rest.split(['?', '#']).next()?;

    let mut parts = path.split('/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    let releases = parts.next()?;
    if !releases.eq_ignore_ascii_case("releases") {
        return None;
    }

    Some(format!("https://github.com/{owner}/{repo}/releases.atom"))
}

/// Atom フィードの URL から、あるタグのダウンロード URL を組み立てる。
/// 組み立てられなければ `None`。
///
/// ```text
/// https://github.com/{owner}/{repo}/releases.atom
///   → https://github.com/{owner}/{repo}/releases/download/{tag}/{asset_name}
/// ```
///
/// GitHub API の未認証レート制限（1 時間 60 回・IP アドレス単位）に当たって
/// 添付ファイルの詳細が取れなくても、ダウンロード URL は規則から組み立てられる。
/// 引き換えに SHA256 は分からない（API からしか取れない）。組み立てた URL が
/// 404 ならダウンロードに失敗するだけで、誤ったものが入ることはない。
pub fn build_download_url(atom_url: &str, tag: &str, asset_name: &str) -> Option<String> {
    if tag.is_empty() || asset_name.is_empty() {
        return None;
    }
    let base = atom_url.strip_suffix(".atom")?;
    Some(format!(
        "{base}/download/{}/{}",
        percent_encode_path_segment(tag),
        percent_encode_path_segment(asset_name)
    ))
}

/// Atom フィードの URL から、あるタグのリリースページの URL を組み立てる。
/// 組み立てられなければ `None`。
pub fn build_release_page_url(atom_url: &str, tag: &str) -> Option<String> {
    if tag.is_empty() {
        return None;
    }
    let base = atom_url.strip_suffix(".atom")?;
    Some(format!("{base}/tag/{}", percent_encode_path_segment(tag)))
}

/// 更新ファイルの取得先として許可する URL か。
///
/// EXE を取得してそのまま実行する処理のため、応答（API の JSON や、組み立てた URL）
/// に書かれたホストをそのまま信用しない。HTTPS かつ、GitHub のリリース配信ホストに
/// 限る。
pub fn is_allowed_download_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    // `user:pass@host:port` の形にも一応備えるが、このアプリの用途では通常出てこない。
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = host_port.split(':').next().unwrap_or(host_port).trim();
    if host.is_empty() {
        return false;
    }

    let host = host.to_ascii_lowercase();
    matches!(
        host.as_str(),
        "github.com" | "api.github.com" | "objects.githubusercontent.com"
    ) || host.ends_with(".githubusercontent.com")
}

/// Atom フィード（GitHub の `releases.atom`）の XML から、いちばん新しいリリースの
/// タグ名を取り出す。読めなければ `None`。
///
/// **並び順に頼らない。** 読み取れたタグのうちバージョンとして最大のものを選ぶ。
/// フィードは普通は新しい順に並ぶが、それに依存すると並びが変わったときに
/// 古い版を「最新」と判断してしまう。バージョンとして読めないタグ
/// （`nightly`、`wip` など下書き用の名前）は無視する。
///
/// タグ名は `<entry>` 内の `<link ... href="https://github.com/{owner}/{repo}/releases/tag/{tag}"/>`
/// の**末尾セグメント**に出る。
///
/// XML パーサーのクレートは使わず、`<entry>` の範囲内に限定した文字列処理で
/// 抜き出す（依存を増やさないため）。壊れた XML や、想定と違う形が来ても
/// 例外を出さず `None` を返し、呼び出し側は API での確認にフォールバックできる。
pub fn extract_latest_tag_from_atom(xml: &str) -> Option<String> {
    let mut best: Option<(Version, String)> = None;

    for entry in find_elements(xml, "entry") {
        let Some(href) = first_link_href(entry) else {
            continue;
        };
        let Some(tag) = tag_from_release_url(&href) else {
            continue;
        };
        let Some(version) = Version::parse(&tag) else {
            continue;
        };
        let is_better = match &best {
            Some((best_version, _)) => version > *best_version,
            None => true,
        };
        if is_better {
            best = Some((version, tag));
        }
    }

    best.map(|(_, tag)| tag)
}

/// `xml` の中から `<name ...> ... </name>`（入れ子は想定しない）の内容部分を
/// すべて取り出す。閉じタグが見つからない要素に出会った時点で走査を打ち切る
/// （壊れた XML の残りを読み進めて誤検出しないため）。
fn find_elements<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open_prefix = format!("<{name}");
    let close_tag = format!("</{name}>");

    let mut result = Vec::new();
    let mut cursor = 0usize;

    while let Some(rel_start) = xml[cursor..].find(open_prefix.as_str()) {
        let tag_start = cursor + rel_start;
        let after = tag_start + open_prefix.len();

        // "<entry" が "<entryfoo" のような別タグの一部でないことを確認する。
        if !is_tag_boundary(xml.as_bytes().get(after).copied()) {
            cursor = after;
            continue;
        }

        let Some(open_end_rel) = xml[tag_start..].find('>') else {
            break; // 開始タグが閉じていない（壊れた XML）。これ以上は読めない。
        };
        let content_start = tag_start + open_end_rel + 1;

        // 自己終了タグ（`<entry .../>`）は中身が無いので飛ばす。
        if xml.as_bytes()[tag_start + open_end_rel - 1] == b'/' {
            cursor = content_start;
            continue;
        }

        let Some(close_rel) = xml[content_start..].find(close_tag.as_str()) else {
            break; // 閉じタグが見つからない（壊れた XML）。
        };
        let content_end = content_start + close_rel;

        result.push(&xml[content_start..content_end]);
        cursor = content_end + close_tag.len();
    }

    result
}

/// タグ名の直後の文字が、タグの終わりとして妥当か
/// （空白・`>`・`/`。`None` はバッファの終端を表す）。
fn is_tag_boundary(byte: Option<u8>) -> bool {
    matches!(byte, Some(b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/'))
}

/// `entry` の中身から、最初の `<link ... href="...">` の `href` 属性値を取り出す。
fn first_link_href(entry: &str) -> Option<String> {
    let mut cursor = 0usize;
    while let Some(rel) = entry[cursor..].find("<link") {
        let tag_start = cursor + rel;
        let after = tag_start + "<link".len();
        if !is_tag_boundary(entry.as_bytes().get(after).copied()) {
            cursor = after;
            continue;
        }

        let tag_end_rel = entry[tag_start..].find('>')?;
        let tag_end = tag_start + tag_end_rel;

        if let Some(href) = extract_attr(&entry[tag_start..=tag_end], "href") {
            if !href.is_empty() {
                return Some(href);
            }
        }
        cursor = tag_end + 1;
    }
    None
}

/// タグの文字列（`<link ...>` 全体）から、指定した属性の値を取り出す。
/// 引用符は `"` `'` のどちらでもよい。属性名は空白区切りの境界でのみ一致させる
/// （例: `href` を探すとき `xhref` には一致しない）。
fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let pattern = format!("{name}=");
    let bytes = tag.as_bytes();
    let mut cursor = 0usize;

    while let Some(rel) = tag[cursor..].find(pattern.as_str()) {
        let idx = cursor + rel;
        let preceded_by_boundary =
            idx == 0 || matches!(bytes[idx - 1], b' ' | b'\t' | b'\n' | b'\r');
        let value_start = idx + pattern.len();

        if preceded_by_boundary {
            if let Some(&quote) = bytes.get(value_start) {
                if quote == b'"' || quote == b'\'' {
                    let after_quote = value_start + 1;
                    if let Some(end_rel) = tag[after_quote..].find(quote as char) {
                        let end = after_quote + end_rel;
                        return Some(tag[after_quote..end].to_string());
                    }
                    return None; // 引用符が閉じていない（壊れた XML）。
                }
            }
        }
        cursor = value_start;
    }
    None
}

/// リリースページの URL（`.../releases/tag/{tag}`）からタグ名を取り出す。
/// パーセントエンコードされていれば元に戻す。
fn tag_from_release_url(href: &str) -> Option<String> {
    let trimmed = href.trim().trim_end_matches('/');
    let last = trimmed.rsplit('/').next()?;
    if last.is_empty() {
        return None;
    }
    Some(percent_decode(last))
}

/// `%XX` 形式のパーセントエンコードを元のバイト列に戻す。
/// 不正な UTF-8 になった場合は元の文字列をそのまま返す。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// URL の 1 パスセグメントとしてパーセントエンコードする
/// （英数字と `-` `.` `_` `~` 以外をすべて `%XX` にする）。
fn percent_encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Version -----------------------------------------------------

    #[test]
    fn parses_common_forms() {
        assert_eq!(Version::parse("v1.2.3"), Version::parse("1.2.3"));
        assert!(Version::parse("1.2").is_some());
        assert!(Version::parse("1.2.3-beta").is_some());
        assert!(Version::parse("なし").is_none());
        assert!(Version::parse("").is_none());
        assert!(Version::parse("v").is_none());
    }

    #[test]
    fn compares_numerically_not_lexically() {
        // 文字列比較では "1.0.10" < "1.0.9" と誤判定してしまう組み合わせ。
        let a = Version::parse("1.0.10").unwrap();
        let b = Version::parse("1.0.9").unwrap();
        assert!(a > b);

        let c = Version::parse("1.10.0").unwrap();
        let d = Version::parse("1.9.0").unwrap();
        assert!(c > d);
    }

    #[test]
    fn equal_versions_are_not_newer() {
        let a = Version::parse("v1.1.0").unwrap();
        let b = Version::parse("1.1.0").unwrap();
        assert!(a <= b);
    }

    // ---- atom_url_from_api_url ----------------------------------------

    #[test]
    fn builds_atom_url_from_typical_api_url() {
        assert_eq!(
            atom_url_from_api_url("https://api.github.com/repos/Yu5rin/MyPaste/releases/latest"),
            Some("https://github.com/Yu5rin/MyPaste/releases.atom".to_string())
        );
    }

    #[test]
    fn non_github_api_url_yields_none() {
        // 配布元を差し替えている場合はこの機能を使わず、従来どおり API だけで確認する。
        assert_eq!(
            atom_url_from_api_url("https://example.com/repos/Yu5rin/MyPaste/releases/latest"),
            None
        );
    }

    #[test]
    fn lookalike_host_is_rejected() {
        // ホスト名の一部一致で通してしまわないこと。
        assert_eq!(
            atom_url_from_api_url(
                "https://api.github.com.evil.com/repos/Yu5rin/MyPaste/releases/latest"
            ),
            None
        );
    }

    #[test]
    fn malformed_repos_path_yields_none() {
        assert_eq!(
            atom_url_from_api_url("https://api.github.com/repos/OnlyOwner"),
            None
        );
        assert_eq!(
            atom_url_from_api_url("https://api.github.com/not-repos/a/b/releases/latest"),
            None
        );
    }

    // ---- build_download_url / build_release_page_url ------------------

    #[test]
    fn builds_download_url_from_atom_url_and_tag() {
        assert_eq!(
            build_download_url(
                "https://github.com/Yu5rin/MyPaste/releases.atom",
                "v1.2.3",
                "Atai-paste.exe"
            ),
            Some(
                "https://github.com/Yu5rin/MyPaste/releases/download/v1.2.3/Atai-paste.exe"
                    .to_string()
            )
        );
    }

    #[test]
    fn download_url_needs_atom_shaped_url() {
        assert_eq!(
            build_download_url(
                "https://github.com/Yu5rin/MyPaste/releases",
                "v1.2.3",
                "Atai-paste.exe"
            ),
            None
        );
    }

    #[test]
    fn download_url_rejects_empty_parts() {
        assert_eq!(
            build_download_url(
                "https://github.com/Yu5rin/MyPaste/releases.atom",
                "",
                "Atai-paste.exe"
            ),
            None
        );
    }

    #[test]
    fn builds_release_page_url() {
        assert_eq!(
            build_release_page_url(
                "https://github.com/Yu5rin/MyPaste/releases.atom",
                "v1.0.8"
            ),
            Some("https://github.com/Yu5rin/MyPaste/releases/tag/v1.0.8".to_string())
        );
    }

    // ---- is_allowed_download_url ---------------------------------------

    #[test]
    fn allows_known_github_hosts() {
        assert!(is_allowed_download_url(
            "https://github.com/Yu5rin/MyPaste/releases/download/v1/Atai-paste.exe"
        ));
        assert!(is_allowed_download_url(
            "https://api.github.com/repos/Yu5rin/MyPaste/releases/latest"
        ));
        assert!(is_allowed_download_url(
            "https://objects.githubusercontent.com/foo/bar"
        ));
        assert!(is_allowed_download_url(
            "https://release-assets.githubusercontent.com/foo/bar"
        ));
    }

    #[test]
    fn rejects_other_hosts() {
        assert!(!is_allowed_download_url("https://evil.com/Atai-paste.exe"));
        assert!(!is_allowed_download_url(
            "https://github.com.evil.com/Atai-paste.exe"
        ));
        assert!(!is_allowed_download_url(
            "https://notgithubusercontent.com/foo"
        ));
    }

    #[test]
    fn rejects_non_https() {
        assert!(!is_allowed_download_url(
            "http://github.com/Yu5rin/MyPaste/releases/download/v1/Atai-paste.exe"
        ));
    }

    #[test]
    fn rejects_malformed_urls() {
        assert!(!is_allowed_download_url(""));
        assert!(!is_allowed_download_url("https://"));
        assert!(!is_allowed_download_url("not a url"));
    }

    // ---- extract_latest_tag_from_atom ----------------------------------

    /// GitHub の releases.atom と同じ形の最小フィードを組み立てる。
    fn feed(tags: &[&str]) -> String {
        let entries: String = tags
            .iter()
            .map(|t| {
                format!(
                    "  <entry>\n    \
                     <id>tag:github.com,2008:Repository/1/{t}</id>\n    \
                     <updated>2026-08-20T00:00:00Z</updated>\n    \
                     <link rel=\"alternate\" type=\"text/html\" href=\"https://github.com/Yu5rin/MyPaste/releases/tag/{t}\"/>\n    \
                     <title>{t}</title>\n  \
                     </entry>\n"
                )
            })
            .collect();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <feed xmlns=\"http://www.w3.org/2005/Atom\">\n  \
             <id>tag:github.com,2008:https://github.com/Yu5rin/MyPaste/releases</id>\n  \
             <link type=\"text/html\" rel=\"alternate\" href=\"https://github.com/Yu5rin/MyPaste/releases\"/>\n  \
             <title>Release notes from MyPaste</title>\n{entries}\
             </feed>\n"
        )
    }

    #[test]
    fn picks_latest_from_typical_feed() {
        assert_eq!(
            extract_latest_tag_from_atom(&feed(&["v1.0.8", "v1.0.7", "v1.0.6"])),
            Some("v1.0.8".to_string())
        );
    }

    #[test]
    fn picks_max_version_even_when_order_is_reversed() {
        // フィードは普通は新しい順だが、それに頼ると並びが変わったときに
        // 古い版を「最新」と判断してしまう。
        assert_eq!(
            extract_latest_tag_from_atom(&feed(&["v1.0.6", "v1.0.8", "v1.0.7"])),
            Some("v1.0.8".to_string())
        );
    }

    #[test]
    fn numeric_comparison_not_lexical_in_feed() {
        // "1.0.10" は文字列比較だと "1.0.9" より小さく見えてしまう。
        assert_eq!(
            extract_latest_tag_from_atom(&feed(&["v1.0.9", "v1.0.10"])),
            Some("v1.0.10".to_string())
        );
    }

    #[test]
    fn ignores_tags_that_are_not_versions() {
        // 下書き用の名前が混ざっていても、読める中での最大を返す。
        assert_eq!(
            extract_latest_tag_from_atom(&feed(&["nightly", "v1.0.7", "wip"])),
            Some("v1.0.7".to_string())
        );
    }

    #[test]
    fn returns_none_when_no_tag_is_readable() {
        assert_eq!(extract_latest_tag_from_atom(&feed(&["nightly", "wip"])), None);
    }

    #[test]
    fn returns_none_for_empty_feed() {
        assert_eq!(extract_latest_tag_from_atom(&feed(&[])), None);
    }

    #[test]
    fn returns_none_for_broken_xml() {
        for xml in [
            "",
            "<feed>閉じていない",
            "これは xml ではない",
            "<html><body>502 Bad Gateway</body></html>",
            "<feed><entry><link href=\"https://github.com/a/b/releases/tag/v1.0.0\"></feed>",
        ] {
            assert_eq!(extract_latest_tag_from_atom(xml), None, "input: {xml:?}");
        }
    }

    #[test]
    fn skips_entries_without_link() {
        let xml = "<?xml version=\"1.0\"?>\n\
             <feed xmlns=\"http://www.w3.org/2005/Atom\">\n  \
             <entry><title>v9.9.9</title></entry>\n  \
             <entry>\n    \
             <link rel=\"alternate\" type=\"text/html\" href=\"https://github.com/Yu5rin/MyPaste/releases/tag/v1.0.8\"/>\n    \
             <title>v1.0.8</title>\n  \
             </entry>\n\
             </feed>";
        // タグ名は link の href から取る。title だけの entry は当てにしない。
        assert_eq!(extract_latest_tag_from_atom(xml), Some("v1.0.8".to_string()));
    }

    #[test]
    fn decodes_percent_encoded_tag_names() {
        let xml = "<feed xmlns=\"http://www.w3.org/2005/Atom\">\n\
             <entry>\n\
             <link rel=\"alternate\" href=\"https://github.com/Yu5rin/MyPaste/releases/tag/v1.0.9%2Bwin\"/>\n\
             </entry>\n\
             </feed>";
        assert_eq!(
            extract_latest_tag_from_atom(xml),
            Some("v1.0.9+win".to_string())
        );
    }

    #[test]
    fn handles_single_quoted_attributes() {
        let xml = "<feed xmlns='http://www.w3.org/2005/Atom'>\n\
             <entry>\n\
             <link rel='alternate' href='https://github.com/Yu5rin/MyPaste/releases/tag/v2.0.0'/>\n\
             </entry>\n\
             </feed>";
        assert_eq!(extract_latest_tag_from_atom(xml), Some("v2.0.0".to_string()));
    }
}
