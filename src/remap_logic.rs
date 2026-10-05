//! キーリマップの設定に関する判断ロジック。
//!
//! 値貼り付けを起動するキーの組み合わせ（例 `Ctrl+B`）の解析・表示・検証と、
//! 対象アプリ（プロセス名）の一覧の整理を行う。Win32 API に触れないので、
//! Linux 上でもテストを実行して確かめられる。
//!
//! キーの組み合わせは `settings.json` に `"Ctrl+B"` のような文字列で保存する
//! （利用者が直接読み書きしやすいように）。

/// 既定のキーの組み合わせ。
pub const DEFAULT_HOTKEY: &str = "Ctrl+B";
/// 既定の対象アプリ。
pub const DEFAULT_TARGET_APP: &str = "EXCEL.EXE";

/// 送出する組み合わせ（値貼り付け）の仮想キー 'V'。同じ組み合わせを登録させないために使う。
const VK_V: u32 = 0x56;

/// 修飾キーとキー本体の組み合わせ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// キー本体の仮想キーコード（[`KEYS`] のいずれか）。
    pub vk: u32,
}

/// 選べるキー本体（表示名, 仮想キーコード）。設定画面の一覧もこの順に並べる。
pub const KEYS: &[(&str, u32)] = &[
    ("A", 0x41), ("B", 0x42), ("C", 0x43), ("D", 0x44), ("E", 0x45), ("F", 0x46),
    ("G", 0x47), ("H", 0x48), ("I", 0x49), ("J", 0x4A), ("K", 0x4B), ("L", 0x4C),
    ("M", 0x4D), ("N", 0x4E), ("O", 0x4F), ("P", 0x50), ("Q", 0x51), ("R", 0x52),
    ("S", 0x53), ("T", 0x54), ("U", 0x55), ("V", 0x56), ("W", 0x57), ("X", 0x58),
    ("Y", 0x59), ("Z", 0x5A),
    ("0", 0x30), ("1", 0x31), ("2", 0x32), ("3", 0x33), ("4", 0x34),
    ("5", 0x35), ("6", 0x36), ("7", 0x37), ("8", 0x38), ("9", 0x39),
    ("F1", 0x70), ("F2", 0x71), ("F3", 0x72), ("F4", 0x73), ("F5", 0x74), ("F6", 0x75),
    ("F7", 0x76), ("F8", 0x77), ("F9", 0x78), ("F10", 0x79), ("F11", 0x7A), ("F12", 0x7B),
];

/// 仮想キーコードからキーの表示名を引く。
pub fn key_name(vk: u32) -> Option<&'static str> {
    KEYS.iter().find(|(_, v)| *v == vk).map(|(n, _)| *n)
}

/// ファンクションキー（F1〜F12）か。
fn is_function_key(vk: u32) -> bool {
    (0x70..=0x7B).contains(&vk)
}

impl Default for Hotkey {
    fn default() -> Self {
        Hotkey {
            ctrl: true,
            shift: false,
            alt: false,
            vk: 0x42,
        }
    }
}

impl Hotkey {
    /// `"Ctrl+Shift+B"` のような文字列を解析する。大文字小文字と空白は区別しない。
    ///
    /// 解析できても、使ってよい組み合わせかどうかは [`Hotkey::validate`] で別に確かめる。
    pub fn parse(text: &str) -> Result<Hotkey, String> {
        let mut hotkey = Hotkey {
            ctrl: false,
            shift: false,
            alt: false,
            vk: 0,
        };
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let Some((key, modifiers)) = parts.split_last() else {
            return Err("キーが指定されていません".into());
        };
        for m in modifiers {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => hotkey.ctrl = true,
                "shift" => hotkey.shift = true,
                "alt" => hotkey.alt = true,
                "" => return Err(format!("「{text}」の書き方が正しくありません")),
                other => return Err(format!("修飾キー「{other}」には対応していません")),
            }
        }
        hotkey.vk = KEYS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, vk)| *vk)
            .ok_or_else(|| format!("キー「{key}」には対応していません（A〜Z、0〜9、F1〜F12）"))?;
        Ok(hotkey)
    }

    /// `"Ctrl+Shift+B"` の形の文字列にする（[`Hotkey::parse`] で読み戻せる）。
    pub fn format(&self) -> String {
        let mut text = String::new();
        if self.ctrl {
            text.push_str("Ctrl+");
        }
        if self.shift {
            text.push_str("Shift+");
        }
        if self.alt {
            text.push_str("Alt+");
        }
        text.push_str(key_name(self.vk).unwrap_or("?"));
        text
    }

    /// 使ってよい組み合わせかを確かめる。
    ///
    /// - 英数字のキーは Ctrl か Alt との組み合わせが必要（単独や Shift だけでは、
    ///   普通の文字入力を奪ってしまう）。
    /// - ファンクションキーは単独でもよい。
    /// - 送出する `Ctrl+Shift+V` そのものは登録できない（意味がない）。
    pub fn validate(&self) -> Result<(), String> {
        if key_name(self.vk).is_none() {
            return Err("キーが選ばれていません".into());
        }
        if !is_function_key(self.vk) && !self.ctrl && !self.alt {
            return Err(
                "英字・数字のキーには Ctrl か Alt を組み合わせてください（単独や Shift だけでは、普段の文字入力ができなくなります）"
                    .into(),
            );
        }
        if self.ctrl && self.shift && !self.alt && self.vk == VK_V {
            return Err("Ctrl+Shift+V は値貼り付けそのものなので、登録できません".into());
        }
        Ok(())
    }

    /// 設定値を読み、使えない値なら既定（`Ctrl+B`）にする。2 つ目は既定にした理由。
    pub fn from_setting(text: &str) -> (Hotkey, Option<String>) {
        match Hotkey::parse(text).and_then(|h| h.validate().map(|()| h)) {
            Ok(h) => (h, None),
            Err(e) => (Hotkey::default(), Some(e)),
        }
    }

    /// キーボードフックと共有するための 1 つの整数にまとめる。
    pub fn pack(&self) -> u32 {
        (self.vk & 0xFF)
            | (u32::from(self.ctrl) << 8)
            | (u32::from(self.shift) << 9)
            | (u32::from(self.alt) << 10)
    }

    /// [`Hotkey::pack`] の逆。
    pub fn unpack(value: u32) -> Hotkey {
        Hotkey {
            vk: value & 0xFF,
            ctrl: value & (1 << 8) != 0,
            shift: value & (1 << 9) != 0,
            alt: value & (1 << 10) != 0,
        }
    }
}

/// 設定画面の入力欄の文字列から、対象アプリの一覧を作る。
///
/// 改行・空白・カンマ・セミコロン区切りの入力を受け付ける（空白を含む名前は
/// `"My App.exe"` のように `"` で囲む）。1 つずつ [`normalize_app`] で整え、
/// 重複を除く（順序は入力のまま）。
pub fn normalize_apps<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut apps: Vec<String> = Vec::new();
    for item in items {
        for part in split_app_list(item) {
            if let Some(name) = normalize_app(&part) {
                if !apps.contains(&name) {
                    apps.push(name);
                }
            }
        }
    }
    apps
}

/// 1 つのアプリ名を整える。前後の空白と `"` を除き、フォルダ付きで書かれていても
/// ファイル名だけにし、大文字にそろえ、`.exe` が無ければ補う。空なら `None`。
pub fn normalize_app(name: &str) -> Option<String> {
    let name = name.trim().trim_matches('"').trim();
    let name = name.rsplit(['\\', '/']).next().unwrap_or(name).trim();
    if name.is_empty() {
        return None;
    }
    let mut name = name.to_ascii_uppercase();
    if !name.ends_with(".EXE") {
        name.push_str(".EXE");
    }
    Some(name)
}

/// 対象アプリの入力を 1 つずつに分ける。区切りは改行・空白・カンマ・セミコロンで、
/// `"` で囲んだ部分は空白を含めて 1 つとして扱う。
fn split_app_list(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if !quoted && (c.is_whitespace() || c == ',' || c == ';') => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// 対象アプリの一覧を、設定画面の入力欄に出す文字列にする（1 行に 1 つ）。
/// 空白を含む名前は `"` で囲み、[`normalize_apps`] で読み戻したときに分かれないようにする。
pub fn apps_to_text(apps: &[String]) -> String {
    apps.iter()
        .map(|a| {
            if a.chars().any(char::is_whitespace) {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// 設定ファイルの対象アプリを、実際に使う一覧にする。
///
/// 書き方の揺れ（小文字、`.exe` 抜け、フォルダ付き）を [`normalize_app`] で直す。
/// 空になった場合は、どのアプリでも動かず故障に見えるため既定（Excel）に戻す。
pub fn effective_target_apps(apps: &[String]) -> Vec<String> {
    // 配列の要素はそれぞれ 1 つの名前なので、空白で分けない（"My App.exe" を許す）。
    let mut normalized: Vec<String> = Vec::new();
    for name in apps.iter().filter_map(|a| normalize_app(a)) {
        if !normalized.contains(&name) {
            normalized.push(name);
        }
    }
    let apps = normalized;
    if apps.is_empty() {
        vec![DEFAULT_TARGET_APP.to_string()]
    } else {
        apps
    }
}

/// プロセスのファイル名が対象アプリに含まれるか（大文字小文字は区別しない）。
pub fn is_target_app(file_name: &str, apps: &[String]) -> bool {
    apps.iter().any(|a| a.eq_ignore_ascii_case(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hk(ctrl: bool, shift: bool, alt: bool, vk: u32) -> Hotkey {
        Hotkey { ctrl, shift, alt, vk }
    }

    #[test]
    fn parses_and_formats_round_trip() {
        for text in ["Ctrl+B", "Ctrl+Shift+B", "Alt+Q", "Ctrl+Shift+Alt+9", "F12", "Shift+F5"] {
            let h = Hotkey::parse(text).unwrap();
            assert_eq!(h.format(), text);
        }
    }

    #[test]
    fn parse_is_lenient_about_case_and_spaces() {
        assert_eq!(Hotkey::parse(" ctrl + b ").unwrap(), hk(true, false, false, 0x42));
        assert_eq!(Hotkey::parse("CONTROL+f2").unwrap(), hk(true, false, false, 0x71));
        // 修飾キーの順序は問わず、表示は Ctrl, Shift, Alt の順にそろえる
        assert_eq!(Hotkey::parse("Shift+Ctrl+B").unwrap().format(), "Ctrl+Shift+B");
    }

    #[test]
    fn parse_rejects_unknown() {
        assert!(Hotkey::parse("").is_err());
        assert!(Hotkey::parse("Ctrl+").is_err());
        assert!(Hotkey::parse("Win+B").is_err());
        assert!(Hotkey::parse("Ctrl+Enter").is_err());
        assert!(Hotkey::parse("Ctrl++B").is_err());
    }

    #[test]
    fn letters_need_ctrl_or_alt() {
        assert!(hk(true, false, false, 0x42).validate().is_ok());
        assert!(hk(false, false, true, 0x42).validate().is_ok());
        assert!(hk(false, false, false, 0x42).validate().is_err());
        assert!(hk(false, true, false, 0x42).validate().is_err());
        assert!(hk(false, true, false, 0x31).validate().is_err());
    }

    #[test]
    fn function_keys_may_stand_alone() {
        assert!(hk(false, false, false, 0x7B).validate().is_ok());
        assert!(hk(false, true, false, 0x74).validate().is_ok());
    }

    #[test]
    fn paste_values_itself_is_rejected() {
        assert!(hk(true, true, false, 0x56).validate().is_err());
        // Ctrl+V や Ctrl+Alt+Shift+V は別の組み合わせなのでよい
        assert!(hk(true, false, false, 0x56).validate().is_ok());
        assert!(hk(true, true, true, 0x56).validate().is_ok());
    }

    #[test]
    fn invalid_setting_falls_back_to_ctrl_b() {
        let (h, why) = Hotkey::from_setting("B");
        assert_eq!(h, Hotkey::default());
        assert!(why.is_some());
        let (h, why) = Hotkey::from_setting("Ctrl+Q");
        assert_eq!(h.format(), "Ctrl+Q");
        assert!(why.is_none());
        assert_eq!(Hotkey::default().format(), DEFAULT_HOTKEY);
    }

    #[test]
    fn pack_round_trip() {
        for text in ["Ctrl+B", "Ctrl+Shift+Alt+F12", "Alt+0", "F1"] {
            let h = Hotkey::parse(text).unwrap();
            assert_eq!(Hotkey::unpack(h.pack()), h);
        }
    }

    #[test]
    fn every_key_name_parses_to_its_code() {
        for (name, vk) in KEYS {
            assert_eq!(Hotkey::parse(&format!("Ctrl+{name}")).unwrap().vk, *vk);
        }
    }

    #[test]
    fn normalizes_app_list() {
        let apps = normalize_apps([
            "excel.exe\r\n  WINWORD \n,C:\\Tools\\Foo\\bar.EXE; \"et.exe\"\n\nEXCEL.EXE",
        ]);
        assert_eq!(apps, ["EXCEL.EXE", "WINWORD.EXE", "BAR.EXE", "ET.EXE"]);
        // 1 行に空白区切りで並べても分ける
        assert_eq!(normalize_apps(["EXCEL.EXE winword"]), ["EXCEL.EXE", "WINWORD.EXE"]);
        // 空白を含む名前は " で囲む（フォルダ付きでもよい）
        assert_eq!(
            normalize_apps([r#""C:\Program Files\My App\My App.exe" excel"#]),
            ["MY APP.EXE", "EXCEL.EXE"]
        );
        assert!(normalize_apps(["", " \n , "]).is_empty());
    }

    #[test]
    fn apps_text_round_trips() {
        let apps = vec!["EXCEL.EXE".to_string(), "MY APP.EXE".to_string()];
        let text = apps_to_text(&apps);
        assert_eq!(text, "EXCEL.EXE\r\n\"MY APP.EXE\"");
        assert_eq!(normalize_apps([text.as_str()]), apps);
    }

    #[test]
    fn empty_target_apps_fall_back_to_excel() {
        assert_eq!(effective_target_apps(&[]), ["EXCEL.EXE"]);
        assert_eq!(effective_target_apps(&[" ".to_string()]), ["EXCEL.EXE"]);
        assert_eq!(effective_target_apps(&["winword".to_string()]), ["WINWORD.EXE"]);
        // 配列の要素は空白を含んでも 1 つの名前として扱う
        assert_eq!(
            effective_target_apps(&["My App.exe".to_string(), "MY APP.EXE".to_string()]),
            ["MY APP.EXE"]
        );
    }

    #[test]
    fn target_app_match_ignores_case() {
        let apps = vec!["EXCEL.EXE".to_string()];
        assert!(is_target_app("Excel.exe", &apps));
        assert!(!is_target_app("notepad.exe", &apps));
        assert!(!is_target_app("Excel.exe", &[]));
    }
}
