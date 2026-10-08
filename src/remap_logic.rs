//! キーリマップの設定に関する判断ロジック。
//!
//! 値貼り付けやキー割り当てを起動するキーの組み合わせ（例 `Ctrl+B`）の解析・表示・検証と、
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
    /// Windows キー（左右どちらでもよい）。
    pub win: bool,
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
    ("F13", 0x7C), ("F14", 0x7D), ("F15", 0x7E), ("F16", 0x7F), ("F17", 0x80), ("F18", 0x81),
    ("F19", 0x82), ("F20", 0x83), ("F21", 0x84), ("F22", 0x85), ("F23", 0x86), ("F24", 0x87),
    ("Space", 0x20), ("Enter", 0x0D), ("Tab", 0x09), ("Esc", 0x1B), ("Backspace", 0x08),
    ("Delete", 0x2E), ("Insert", 0x2D), ("Home", 0x24), ("End", 0x23),
    ("PageUp", 0x21), ("PageDown", 0x22),
    ("Left", 0x25), ("Up", 0x26), ("Right", 0x27), ("Down", 0x28),
];

/// キーの別名（よく使われる書き方。表示は [`KEYS`] の名前に直す）。
const KEY_ALIASES: &[(&str, u32)] = &[
    ("Escape", 0x1B), ("Return", 0x0D), ("Del", 0x2E), ("Ins", 0x2D),
    ("PgUp", 0x21), ("PgDn", 0x22), ("BS", 0x08),
];

/// 送るときに「拡張キー」の印（`KEYEVENTF_EXTENDEDKEY`）が要るキーか。
/// 矢印や Home などは、印が無いとテンキー側のキーとして扱われることがある。
pub fn is_extended_key(vk: u32) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2D | 0x2E)
}

/// 仮想キーコードからキーの表示名を引く。
pub fn key_name(vk: u32) -> Option<&'static str> {
    KEYS.iter().find(|(_, v)| *v == vk).map(|(n, _)| *n)
}

/// ファンクションキー（F1〜F24）か。
fn is_function_key(vk: u32) -> bool {
    (0x70..=0x87).contains(&vk)
}

impl Default for Hotkey {
    fn default() -> Self {
        Hotkey {
            ctrl: true,
            shift: false,
            alt: false,
            win: false,
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
            win: false,
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
                "win" | "windows" => hotkey.win = true,
                "" => return Err(format!("「{text}」の書き方が正しくありません")),
                other => return Err(format!("修飾キー「{other}」には対応していません")),
            }
        }
        hotkey.vk = KEYS
            .iter()
            .chain(KEY_ALIASES)
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, vk)| *vk)
            .ok_or_else(|| {
                format!(
                    "キー「{key}」には対応していません（A〜Z、0〜9、F1〜F24、Space、Enter、Tab、Esc、\
                     Backspace、Delete、Insert、Home、End、PageUp、PageDown、Left、Up、Right、Down）"
                )
            })?;
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
        if self.win {
            text.push_str("Win+");
        }
        text.push_str(key_name(self.vk).unwrap_or("?"));
        text
    }

    /// 起動するキーとして使ってよい組み合わせかを確かめる（キー割り当てにも使う）。
    ///
    /// - ファンクションキー以外は Ctrl・Alt・Win のどれかとの組み合わせが必要
    ///   （単独や Shift だけでは、普通の文字入力や Enter などを奪ってしまう）。
    /// - ファンクションキーは単独でもよい。
    pub fn validate_trigger(&self) -> Result<(), String> {
        if key_name(self.vk).is_none() {
            return Err("キーが選ばれていません".into());
        }
        if !is_function_key(self.vk) && !self.ctrl && !self.alt && !self.win {
            return Err(
                "F1〜F24 以外のキーには Ctrl・Alt・Win のどれかを組み合わせてください（単独や Shift だけでは、普段の文字入力ができなくなります）"
                    .into(),
            );
        }
        Ok(())
    }

    /// 値貼り付けを起動するキーとして使ってよい組み合わせかを確かめる。
    ///
    /// [`Hotkey::validate_trigger`] に加え、送出する `Ctrl+Shift+V` そのものは
    /// 登録できない（意味がない）。
    pub fn validate(&self) -> Result<(), String> {
        self.validate_trigger()?;
        if self.ctrl && self.shift && !self.alt && !self.win && self.vk == VK_V {
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
            | (u32::from(self.win) << 11)
    }

    /// [`Hotkey::pack`] の逆。
    pub fn unpack(value: u32) -> Hotkey {
        Hotkey {
            vk: value & 0xFF,
            ctrl: value & (1 << 8) != 0,
            shift: value & (1 << 9) != 0,
            alt: value & (1 << 10) != 0,
            win: value & (1 << 11) != 0,
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

/// アプリの一覧を、1 行の欄に入れる文字にする（空白で区切る。空白を含む名前は `"` で囲む）。
pub fn apps_to_line(apps: &[String]) -> String {
    apps_to_text(apps).replace("\r\n", " ")
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

/// 動いているアプリ 1 つ分（プロセス名を選ぶ画面の 1 行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningApp {
    /// プロセス名（[`normalize_app`] でそろえたもの。例 `EXCEL.EXE`）。
    pub name: String,
    /// そのアプリのいちばん手前のウィンドウの題名。
    pub title: String,
    /// そのアプリの（題名のある）ウィンドウの数。
    pub windows: usize,
}

/// 開いているウィンドウ（プロセス名と題名。手前から順）を、アプリごとにまとめて名前順に並べる。
pub fn running_apps<'a>(windows: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<RunningApp> {
    let mut apps: Vec<RunningApp> = Vec::new();
    for (name, title) in windows {
        let Some(name) = normalize_app(name) else {
            continue;
        };
        match apps.iter_mut().find(|a| a.name == name) {
            Some(app) => app.windows += 1,
            None => apps.push(RunningApp {
                name,
                title: title.trim().to_string(),
                windows: 1,
            }),
        }
    }
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    apps
}

/// 一覧に出す 1 行（例 `EXCEL.EXE　Book1 - Excel（ほか 2 つ）`）。
pub fn running_app_row(app: &RunningApp) -> String {
    let mut row = format!("{}\u{3000}{}", app.name, app.title);
    if app.windows > 1 {
        row.push_str(&format!("（ほか {} つ）", app.windows - 1));
    }
    row
}

/// アプリの一覧の欄の文字に 1 つ足す（すでにあれば何もしない）。そろえた一覧を返す。
pub fn add_app(text: &str, name: &str) -> Vec<String> {
    let mut apps = normalize_apps([text]);
    if let Some(name) = normalize_app(name) {
        if !apps.contains(&name) {
            apps.push(name);
        }
    }
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hk(ctrl: bool, shift: bool, alt: bool, vk: u32) -> Hotkey {
        Hotkey {
            ctrl,
            shift,
            alt,
            win: false,
            vk,
        }
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
        assert!(Hotkey::parse("Super+B").is_err());
        assert!(Hotkey::parse("Ctrl+NumLock").is_err());
        assert!(Hotkey::parse("Ctrl++B").is_err());
    }

    #[test]
    fn parses_win_and_named_keys() {
        let h = Hotkey::parse("Win+Shift+T").unwrap();
        assert!(h.win && h.shift && !h.ctrl);
        assert_eq!(h.format(), "Shift+Win+T");
        assert_eq!(Hotkey::parse("ctrl+escape").unwrap().format(), "Ctrl+Esc");
        assert_eq!(Hotkey::parse("Enter").unwrap().vk, 0x0D);
        assert_eq!(Hotkey::parse("PgDn").unwrap().format(), "PageDown");
        assert_eq!(Hotkey::parse("F24").unwrap().vk, 0x87);
        let h = Hotkey::parse("Ctrl+Alt+Win+Delete").unwrap();
        assert_eq!(Hotkey::unpack(h.pack()), h);
        assert!(is_extended_key(0x25) && is_extended_key(0x2E) && !is_extended_key(0x0D));
    }

    #[test]
    fn trigger_rules() {
        // Win も修飾キーとして数える
        assert!(Hotkey::parse("Win+T").unwrap().validate_trigger().is_ok());
        // Enter や矢印を単独で奪うのは不可。F13〜F24 は単独でよい
        assert!(Hotkey::parse("Enter").unwrap().validate_trigger().is_err());
        assert!(Hotkey::parse("Shift+Left").unwrap().validate_trigger().is_err());
        assert!(Hotkey::parse("F13").unwrap().validate_trigger().is_ok());
        // Ctrl+Shift+V は値貼り付けでは不可だが、キー割り当てのきっかけには使える
        let paste = Hotkey::parse("Ctrl+Shift+V").unwrap();
        assert!(paste.validate().is_err());
        assert!(paste.validate_trigger().is_ok());
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

    #[test]
    fn running_apps_are_grouped() {
        let apps = running_apps([
            ("notepad.exe", "メモ"),
            ("EXCEL.EXE", " Book1 - Excel "),
            ("excel.exe", "Book2 - Excel"),
            ("", "名前なし"),
        ]);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0], RunningApp { name: "EXCEL.EXE".into(), title: "Book1 - Excel".into(), windows: 2 });
        assert_eq!(running_app_row(&apps[0]), "EXCEL.EXE\u{3000}Book1 - Excel（ほか 1 つ）");
        assert_eq!(running_app_row(&apps[1]), "NOTEPAD.EXE\u{3000}メモ");
    }

    #[test]
    fn adds_app_once() {
        assert_eq!(add_app("EXCEL.EXE", "notepad.exe"), vec!["EXCEL.EXE", "NOTEPAD.EXE"]);
        assert_eq!(add_app("excel notepad", "NOTEPAD.EXE"), vec!["EXCEL.EXE", "NOTEPAD.EXE"]);
        assert_eq!(add_app("", "My App.exe"), vec!["MY APP.EXE"]);
    }
}
