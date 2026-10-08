//! キー割り当ての判断ロジック。
//!
//! 「このキーを押したら、この動作をする」という割り当て（[`Rule`]）を、
//! `settings.json` の書き方（[`HotkeyRuleSetting`]）から読み取り、検証する。
//! 実際にキーを送ったりプログラムを開いたりするのは [`crate::actions`]、
//! キーを捕まえるのは [`crate::keyboard`]、編集する画面は [`crate::hotkey_window`]。
//!
//! Win32 API に触れないので、Linux 上でもテストを実行して確かめられる。

use crate::config::HotkeyRuleSetting;
use crate::remap_logic::{self, Hotkey};
use crate::text_transform::{self, Transform};

/// 1 つの割り当てで送れるキー操作の数の上限（誤って長大な列を書いた場合に備える）。
const MAX_KEY_SEQUENCE: usize = 32;
/// 1 つの割り当てで入力できる文字数の上限。
const MAX_TEXT_CHARS: usize = 4000;

/// 動作の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    SendKeys,
    TypeText,
    Run,
    PastePlain,
    PasteTransform,
    ClipboardHistory,
    ToggleTopmost,
    ShowList,
    Block,
}

/// 動作の種類ごとの説明（設定値の名前、画面に出す名前、内容欄の見出し、補足）。
pub struct ActionInfo {
    pub kind: ActionKind,
    /// settings.json に書く名前。
    pub key: &'static str,
    /// 画面に出す名前。
    pub name: &'static str,
    /// 内容の欄の見出し。内容が要らない動作では `None`。
    pub value_label: Option<&'static str>,
    /// 補足の説明。
    pub hint: &'static str,
    /// 引数の欄を使うか。
    pub uses_args: bool,
    /// 内容を、文字の欄ではなく「整え方」のチェックで選ぶか（文字を整えて貼り付け）。
    pub uses_transforms: bool,
}

/// 動作の一覧（設定画面の一覧もこの順に並べる）。
pub const ACTIONS: [ActionInfo; 9] = [
    ActionInfo {
        kind: ActionKind::SendKeys,
        key: "send_keys",
        name: "キーを送る",
        value_label: Some("送るキー（例: Ctrl+Alt+V, V, Enter）"),
        hint: "カンマで区切ると、左から順に送ります。使えるキーは A〜Z、0〜9、F1〜F24、Space、Enter、Tab、Esc、Backspace、Delete、Insert、Home、End、PageUp、PageDown、Left、Up、Right、Down です。",
        uses_args: false,
        uses_transforms: false,
    },
    ActionInfo {
        kind: ActionKind::TypeText,
        key: "type_text",
        name: "文字を入力する",
        value_label: Some("入力する文字（改行もそのまま入力します）"),
        hint: "{date} は今日の日付（2026/10/05）、{time} は今の時刻（13:45）、{datetime} は両方に置き換えます。{ そのものは {{、} は }} と書きます。",
        uses_args: false,
        uses_transforms: false,
    },
    ActionInfo {
        kind: ActionKind::Run,
        key: "run",
        name: "プログラムやファイル・URL を開く",
        value_label: Some("開くもの（例: notepad.exe、C:\\資料\\一覧.xlsx、https://...）"),
        hint: "エクスプローラーでダブルクリックしたときと同じように開きます。プログラムに渡す引数があれば、下の「引数」に書きます。",
        uses_args: true,
        uses_transforms: false,
    },
    ActionInfo {
        kind: ActionKind::PastePlain,
        key: "paste_plain",
        name: "書式なしで貼り付け",
        value_label: None,
        hint: "クリップボードの文字だけを貼り付けます（文字の色や太字、表の書式などは付きません）。貼り付けたあとのクリップボードも文字だけになります。",
        uses_args: false,
        uses_transforms: false,
    },
    ActionInfo {
        kind: ActionKind::PasteTransform,
        key: "paste_transform",
        name: "文字を整えて貼り付け",
        value_label: Some("整え方（チェックしたものを、左の列の上から順に当てます）"),
        hint: "",
        uses_args: false,
        uses_transforms: true,
    },
    ActionInfo {
        kind: ActionKind::ClipboardHistory,
        key: "clipboard_history",
        name: "クリップボードの履歴から貼り付け",
        value_label: None,
        hint: "最近コピーした文字の一覧を出し、選んだものを書式なしで貼り付けます（クリックか、矢印キーと Enter で選びます。文字を入力すると絞り込めます）。設定画面の「クリップボードの履歴」でも、一覧を出すキーや件数を決められます。",
        uses_args: false,
        uses_transforms: false,
    },
    ActionInfo {
        kind: ActionKind::ToggleTopmost,
        key: "toggle_topmost",
        name: "前面のウィンドウを常に手前に表示（切り替え）",
        value_label: None,
        hint: "押すたびに、いま使っているウィンドウを常に手前に表示する・やめるを切り替えます。管理者として実行しているアプリのウィンドウには効きません。",
        uses_args: false,
        uses_transforms: false,
    },
    ActionInfo {
        kind: ActionKind::ShowList,
        key: "show_list",
        name: "キー割り当ての一覧を表示",
        value_label: None,
        hint: "いま使える値貼り付けのキーとキー割り当てを、一覧で表示します（トレイメニューの「キー割り当ての一覧」と同じ）。",
        uses_args: false,
        uses_transforms: false,
    },
    ActionInfo {
        kind: ActionKind::Block,
        key: "block",
        name: "何もしない（キーを無効にする）",
        value_label: None,
        hint: "押しても何も起きないようにします（うっかり押しやすいキーを止めるときに使います）。",
        uses_args: false,
        uses_transforms: false,
    },
];

impl ActionKind {
    pub fn info(self) -> &'static ActionInfo {
        ACTIONS
            .iter()
            .find(|a| a.kind == self)
            .unwrap_or(&ACTIONS[0])
    }

    /// settings.json の値から読む。知らない値は `None`。
    pub fn from_setting(text: &str) -> Option<ActionKind> {
        ACTIONS
            .iter()
            .find(|a| a.key.eq_ignore_ascii_case(text.trim()))
            .map(|a| a.kind)
    }
}

/// 実行する動作（内容を解析済みのもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    SendKeys(Vec<Hotkey>),
    /// 文字を入力する。`paste` が真なら、クリップボードを使って一度に貼り付ける
    /// （偽なら 1 文字ずつキー入力として送る）。
    TypeText { text: String, paste: bool },
    Run { target: String, args: String },
    PastePlain,
    /// クリップボードの文字を整えて貼り付ける（クリップボードの中身は変えない）。
    PasteTransform(Vec<Transform>),
    ClipboardHistory,
    ToggleTopmost,
    ShowList,
    Block,
}

/// 実際に使う割り当て（検証済み・有効なものだけ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub hotkey: Hotkey,
    /// 効くアプリ（大文字のプロセス名）。空ならすべてのアプリ。
    pub apps: Vec<String>,
    pub action: Action,
}

/// 誤りのある欄。設定画面で、直すべき欄へ移るために使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Hotkey,
    Action,
    Value,
}

/// 割り当ての誤り。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleError {
    pub field: Field,
    pub message: String,
}

fn error(field: Field, message: impl Into<String>) -> RuleError {
    RuleError {
        field,
        message: message.into(),
    }
}

impl Rule {
    /// settings.json の書き方から読み、検証する（`enabled` は見ない）。
    pub fn from_setting(setting: &HotkeyRuleSetting) -> Result<Rule, RuleError> {
        let hotkey = Hotkey::parse(&setting.hotkey).map_err(|e| error(Field::Hotkey, e))?;
        hotkey
            .validate_trigger()
            .map_err(|e| error(Field::Hotkey, e))?;
        let kind = ActionKind::from_setting(&setting.action)
            .ok_or_else(|| error(Field::Action, format!("動作「{}」には対応していません", setting.action)))?;
        let value = setting.value.trim();
        let action = match kind {
            ActionKind::SendKeys => {
                Action::SendKeys(parse_key_sequence(value).map_err(|e| error(Field::Value, e))?)
            }
            ActionKind::TypeText => {
                // 入力する文字は前後の空白や改行も意味があるので、そのまま使う。
                if setting.value.is_empty() {
                    return Err(error(Field::Value, "入力する文字を書いてください"));
                }
                if setting.value.chars().count() > MAX_TEXT_CHARS {
                    return Err(error(
                        Field::Value,
                        format!("入力する文字は {MAX_TEXT_CHARS} 文字までにしてください"),
                    ));
                }
                Action::TypeText {
                    text: setting.value.clone(),
                    paste: input_is_paste(&setting.input),
                }
            }
            ActionKind::Run => {
                if value.is_empty() {
                    return Err(error(Field::Value, "開くもの（プログラム・ファイル・URL）を書いてください"));
                }
                Action::Run {
                    target: value.trim_matches('"').to_string(),
                    args: setting.args.trim().to_string(),
                }
            }
            ActionKind::PastePlain => Action::PastePlain,
            ActionKind::PasteTransform => {
                let transforms =
                    text_transform::parse_list(value).map_err(|e| error(Field::Value, e))?;
                if transforms.is_empty() {
                    return Err(error(Field::Value, "整え方を 1 つ以上選んでください"));
                }
                Action::PasteTransform(transforms)
            }
            ActionKind::ClipboardHistory => Action::ClipboardHistory,
            ActionKind::ToggleTopmost => Action::ToggleTopmost,
            ActionKind::ShowList => Action::ShowList,
            ActionKind::Block => Action::Block,
        };
        Ok(Rule {
            hotkey,
            apps: remap_logic::normalize_apps(setting.apps.iter().map(String::as_str)),
            action,
        })
    }

    /// このアプリで効くか（`process` は大文字小文字を区別しない）。
    pub fn applies_to(&self, process: Option<&str>) -> bool {
        if self.apps.is_empty() {
            return true;
        }
        process.is_some_and(|p| remap_logic::is_target_app(p, &self.apps))
    }
}

/// 「文字を入力する」の入れ方が「まとめて貼り付ける」か。`"keys"` のときだけ 1 文字ずつにし、
/// それ以外（空・知らない値を含む）はまとめて貼り付ける。
pub fn input_is_paste(input: &str) -> bool {
    !input.trim().eq_ignore_ascii_case("keys")
}

/// 有効な割り当てだけを読み取る。読めないものは飛ばし、その理由を 2 つ目に返す（記録用）。
pub fn compile(settings: &[HotkeyRuleSetting]) -> (Vec<Rule>, Vec<String>) {
    let mut rules = Vec::new();
    let mut problems = Vec::new();
    for (i, setting) in settings.iter().enumerate() {
        if !setting.enabled {
            continue;
        }
        match Rule::from_setting(setting) {
            Ok(rule) => rules.push(rule),
            Err(e) => problems.push(format!(
                "キー割り当て {}（{}）を使えません: {}",
                i + 1,
                setting.hotkey,
                e.message
            )),
        }
    }
    (rules, problems)
}

/// 送るキーの列（`"Ctrl+Alt+V, V, Enter"`）を読む。
pub fn parse_key_sequence(text: &str) -> Result<Vec<Hotkey>, String> {
    let keys: Vec<Hotkey> = text
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(Hotkey::parse)
        .collect::<Result<_, _>>()?;
    if keys.is_empty() {
        return Err("送るキーを書いてください（例: Ctrl+Alt+V, V, Enter）".into());
    }
    if keys.len() > MAX_KEY_SEQUENCE {
        return Err(format!("送るキーは {MAX_KEY_SEQUENCE} 個までにしてください"));
    }
    Ok(keys)
}

/// 送るキーの列を、読み戻せる形の文字列にする。
pub fn format_key_sequence(keys: &[Hotkey]) -> String {
    keys.iter().map(Hotkey::format).collect::<Vec<_>>().join(", ")
}

/// 日付・時刻（PC の時刻）。
#[derive(Debug, Clone, Copy)]
pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
}

/// 入力する文字の `{date}` などを置き換える。知らない `{...}` はそのまま残す。
pub fn expand_placeholders(text: &str, now: &LocalTime) -> String {
    let date = format!("{:04}/{:02}/{:02}", now.year, now.month, now.day);
    let time = format!("{:02}:{:02}", now.hour, now.minute);
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if let Some(r) = rest.strip_prefix("{{") {
            out.push('{');
            rest = r;
        } else if let Some(r) = rest.strip_prefix("}}") {
            out.push('}');
            rest = r;
        } else if let Some(r) = rest.strip_prefix("{datetime}") {
            out.push_str(&date);
            out.push(' ');
            out.push_str(&time);
            rest = r;
        } else if let Some(r) = rest.strip_prefix("{date}") {
            out.push_str(&date);
            rest = r;
        } else if let Some(r) = rest.strip_prefix("{time}") {
            out.push_str(&time);
            rest = r;
        } else {
            // 置き換えない { や } はそのまま。
            out.push_str(&rest[..1]);
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// 一覧に出す 1 行の説明。例: `Ctrl+Alt+V → 書式なしで貼り付け（EXCEL.EXE）`
pub fn describe(setting: &HotkeyRuleSetting) -> String {
    let mut text = String::new();
    if !setting.enabled {
        text.push_str("［停止中］");
    }
    let rule = Rule::from_setting(setting);
    if rule.is_err() {
        text.push_str("［誤りあり］");
    }
    let hotkey = Hotkey::parse(&setting.hotkey)
        .map(|h| h.format())
        .unwrap_or_else(|_| setting.hotkey.clone());
    text.push_str(&hotkey);
    text.push_str(" → ");
    match ActionKind::from_setting(&setting.action) {
        Some(kind) => {
            text.push_str(kind.info().name);
            let value = setting.value.trim();
            if kind.info().uses_transforms {
                if let Ok(transforms) = text_transform::parse_list(value) {
                    text.push_str(&format!("（{} 種類）", transforms.len()));
                }
            } else if kind.info().value_label.is_some() && !value.is_empty() {
                // 長い内容や改行は一覧では短くする。
                let one_line: String = value.lines().next().unwrap_or("").chars().take(24).collect();
                let cut = value.chars().count() > one_line.chars().count();
                text.push_str(&format!("「{one_line}{}」", if cut { "…" } else { "" }));
            }
        }
        None => text.push_str(&setting.action),
    }
    let apps = remap_logic::normalize_apps(setting.apps.iter().map(String::as_str));
    if !apps.is_empty() {
        text.push_str(&format!("（{}）", apps.join("、")));
    }
    text
}

/// 「キー割り当ての一覧」に出す文。値貼り付けのキーと、すべてのキー割り当て（停止中・誤りありも
/// 印を付けて）を並べる。`remap_apps` は実際に効く対象アプリ。
pub fn list_text(
    remap: (Hotkey, &[String], bool),
    rules_on: bool,
    settings: &[HotkeyRuleSetting],
) -> String {
    let (remap_key, remap_apps, remap_on) = remap;
    let mut text = String::from("値貼り付け\n");
    text.push_str(&format!(
        "    {}{} → 値貼り付け（Ctrl+Shift+V）（{}）\n",
        if remap_on { "" } else { "［停止中］" },
        remap_key.format(),
        remap_apps.join("、")
    ));
    text.push_str(if rules_on {
        "\nキー割り当て\n"
    } else {
        "\nキー割り当て（すべて停止中）\n"
    });
    if settings.is_empty() {
        text.push_str("    （ありません）\n");
    }
    for setting in settings {
        text.push_str(&format!("    {}\n", describe(setting)));
    }
    text.trim_end().to_string()
}

/// 有効で読めるキー割り当てのうち、`hotkey` を使っているものの番号（1 から）を返す。
pub fn find_key_in_rules(settings: &[HotkeyRuleSetting], hotkey: Hotkey) -> Option<usize> {
    settings
        .iter()
        .enumerate()
        .filter(|(_, s)| s.enabled)
        .find(|(_, s)| Rule::from_setting(s).is_ok_and(|r| r.hotkey == hotkey))
        .map(|(i, _)| i + 1)
}

/// 2 つの範囲（空はすべてのアプリ）が重なるか。
fn apps_overlap(a: &[String], b: &[String]) -> bool {
    a.is_empty() || b.is_empty() || a.iter().any(|x| remap_logic::is_target_app(x, b))
}

/// 同じキーで同じアプリに効く割り当てがあれば、その説明を返す。
///
/// `remap` は値貼り付けのキーと対象アプリ（こちらが先に効くので、重なるとキー割り当ては効かない）。
/// 有効で、読める割り当てだけを調べる。
pub fn find_conflict(
    settings: &[HotkeyRuleSetting],
    remap: (Hotkey, &[String]),
) -> Option<String> {
    let rules: Vec<(usize, Rule)> = settings
        .iter()
        .enumerate()
        .filter(|(_, s)| s.enabled)
        .filter_map(|(i, s)| Rule::from_setting(s).ok().map(|r| (i, r)))
        .collect();
    let (remap_key, remap_apps) = remap;
    for (i, rule) in &rules {
        if rule.hotkey == remap_key && apps_overlap(&rule.apps, remap_apps) {
            return Some(format!(
                "{} 番目の割り当て（{}）は、値貼り付けのキーと同じです。値貼り付けが先に効くため、\
                 この割り当ては使われません。別のキーにするか、設定画面で値貼り付けのキーを変えてください。",
                i + 1,
                rule.hotkey.format()
            ));
        }
    }
    for (n, (i, a)) in rules.iter().enumerate() {
        for (j, b) in &rules[n + 1..] {
            if a.hotkey == b.hotkey && apps_overlap(&a.apps, &b.apps) {
                return Some(format!(
                    "{} 番目と {} 番目の割り当てが、同じキー（{}）で同じアプリに効きます。\
                     上にある {} 番目だけが使われます。キーか対象アプリを変えてください。",
                    i + 1,
                    j + 1,
                    a.hotkey.format(),
                    i + 1
                ));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(hotkey: &str, action: &str, value: &str) -> HotkeyRuleSetting {
        HotkeyRuleSetting {
            hotkey: hotkey.into(),
            action: action.into(),
            value: value.into(),
            ..Default::default()
        }
    }

    #[test]
    fn reads_each_action() {
        let r = Rule::from_setting(&setting("Ctrl+Alt+V", "paste_plain", "")).unwrap();
        assert_eq!(r.action, Action::PastePlain);
        let r = Rule::from_setting(&setting("F9", "send_keys", "Ctrl+Alt+V, V, Enter")).unwrap();
        assert_eq!(
            r.action,
            Action::SendKeys(parse_key_sequence("Ctrl+Alt+V,V,Enter").unwrap())
        );
        let r = Rule::from_setting(&setting("Win+Shift+T", "toggle_topmost", "")).unwrap();
        assert_eq!(r.action, Action::ToggleTopmost);
        let mut s = setting("Ctrl+Alt+N", "run", " \"C:\\Tools\\My App.exe\" ");
        s.args = " --new ".into();
        let r = Rule::from_setting(&s).unwrap();
        assert_eq!(
            r.action,
            Action::Run {
                target: "C:\\Tools\\My App.exe".into(),
                args: "--new".into()
            }
        );
        let r = Rule::from_setting(&setting("Ctrl+Alt+D", "type_text", " {date} \n")).unwrap();
        assert_eq!(
            r.action,
            Action::TypeText {
                text: " {date} \n".into(),
                paste: true
            }
        );
        let mut s = setting("Ctrl+Alt+D", "type_text", "abc");
        s.input = "keys".into();
        assert_eq!(
            Rule::from_setting(&s).unwrap().action,
            Action::TypeText {
                text: "abc".into(),
                paste: false
            }
        );
        let r = Rule::from_setting(&setting("F1", "block", "")).unwrap();
        assert_eq!(r.action, Action::Block);
    }

    #[test]
    fn reports_the_field_in_error() {
        let e = Rule::from_setting(&setting("B", "block", "")).unwrap_err();
        assert_eq!(e.field, Field::Hotkey);
        let e = Rule::from_setting(&setting("Ctrl+Q", "fly", "")).unwrap_err();
        assert_eq!(e.field, Field::Action);
        let e = Rule::from_setting(&setting("Ctrl+Q", "send_keys", "Ctrl+Nope")).unwrap_err();
        assert_eq!(e.field, Field::Value);
        let e = Rule::from_setting(&setting("Ctrl+Q", "type_text", "")).unwrap_err();
        assert_eq!(e.field, Field::Value);
        let e = Rule::from_setting(&setting("Ctrl+Q", "run", "  ")).unwrap_err();
        assert_eq!(e.field, Field::Value);
        let long = "あ".repeat(MAX_TEXT_CHARS + 1);
        assert!(Rule::from_setting(&setting("Ctrl+Q", "type_text", &long)).is_err());
    }

    #[test]
    fn key_sequence() {
        assert_eq!(
            format_key_sequence(&parse_key_sequence(" ctrl+alt+v ,v,,enter ").unwrap()),
            "Ctrl+Alt+V, V, Enter"
        );
        assert!(parse_key_sequence("").is_err());
        assert!(parse_key_sequence(" , ").is_err());
        let many = vec!["A"; MAX_KEY_SEQUENCE + 1].join(",");
        assert!(parse_key_sequence(&many).is_err());
    }

    #[test]
    fn compile_skips_disabled_and_broken() {
        let mut off = setting("Ctrl+Alt+V", "paste_plain", "");
        off.enabled = false;
        let (rules, problems) = compile(&[
            off,
            setting("Ctrl+Alt+P", "paste_plain", ""),
            setting("X", "paste_plain", ""),
        ]);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].hotkey.format(), "Ctrl+Alt+P");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("キー割り当て 3"));
    }

    #[test]
    fn scope() {
        let mut s = setting("Ctrl+Alt+P", "paste_plain", "");
        let r = Rule::from_setting(&s).unwrap();
        assert!(r.applies_to(None));
        assert!(r.applies_to(Some("anything.exe")));
        s.apps = vec!["excel".into()];
        let r = Rule::from_setting(&s).unwrap();
        assert_eq!(r.apps, ["EXCEL.EXE"]);
        assert!(r.applies_to(Some("Excel.exe")));
        assert!(!r.applies_to(Some("notepad.exe")));
        assert!(!r.applies_to(None));
    }

    #[test]
    fn placeholders() {
        let now = LocalTime {
            year: 2026,
            month: 10,
            day: 5,
            hour: 9,
            minute: 7,
        };
        assert_eq!(expand_placeholders("{date}", &now), "2026/10/05");
        assert_eq!(expand_placeholders("時刻 {time}。", &now), "時刻 09:07。");
        assert_eq!(expand_placeholders("{datetime}", &now), "2026/10/05 09:07");
        assert_eq!(expand_placeholders("{{date}} {x} }{", &now), "{date} {x} }{");
        assert_eq!(expand_placeholders("改行\nそのまま", &now), "改行\nそのまま");
    }

    #[test]
    fn reads_new_actions() {
        let r = Rule::from_setting(&setting("Ctrl+Alt+H", "clipboard_history", "")).unwrap();
        assert_eq!(r.action, Action::ClipboardHistory);
        let r = Rule::from_setting(&setting("Ctrl+Alt+L", "show_list", "")).unwrap();
        assert_eq!(r.action, Action::ShowList);
        let r = Rule::from_setting(&setting("Ctrl+Alt+T", "paste_transform", "trim,zen_to_han"))
            .unwrap();
        assert_eq!(
            r.action,
            Action::PasteTransform(vec![Transform::ZenToHan, Transform::Trim])
        );
        let e = Rule::from_setting(&setting("Ctrl+Alt+T", "paste_transform", "")).unwrap_err();
        assert_eq!(e.field, Field::Value);
        let e = Rule::from_setting(&setting("Ctrl+Alt+T", "paste_transform", "fly")).unwrap_err();
        assert_eq!(e.field, Field::Value);
        assert_eq!(
            describe(&setting("Ctrl+Alt+T", "paste_transform", "trim,zen_to_han")),
            "Ctrl+Alt+T → 文字を整えて貼り付け（2 種類）"
        );
    }

    #[test]
    fn list_shows_remap_and_rules() {
        let apps = vec!["EXCEL.EXE".to_string()];
        let mut stopped = setting("F9", "block", "");
        stopped.enabled = false;
        let text = list_text(
            (Hotkey::parse("Ctrl+B").unwrap(), &apps, true),
            true,
            &[setting("Ctrl+Alt+V", "paste_plain", ""), stopped],
        );
        assert_eq!(
            text,
            "値貼り付け\n    Ctrl+B → 値貼り付け（Ctrl+Shift+V）（EXCEL.EXE）\n\n\
             キー割り当て\n    Ctrl+Alt+V → 書式なしで貼り付け\n    ［停止中］F9 → 何もしない（キーを無効にする）"
        );
        let text = list_text((Hotkey::parse("Ctrl+B").unwrap(), &apps, false), false, &[]);
        assert!(text.contains("［停止中］Ctrl+B"));
        assert!(text.ends_with("キー割り当て（すべて停止中）\n    （ありません）"));
    }

    #[test]
    fn input_method() {
        assert!(input_is_paste("paste"));
        assert!(input_is_paste(""));
        assert!(input_is_paste("なにか"));
        assert!(!input_is_paste("keys"));
        assert!(!input_is_paste(" KEYS "));
    }

    #[test]
    fn describes_for_list() {
        let mut s = setting("ctrl+alt+v", "paste_plain", "");
        s.apps = vec!["excel".into()];
        assert_eq!(describe(&s), "Ctrl+Alt+V → 書式なしで貼り付け（EXCEL.EXE）");
        let mut s = setting("F9", "type_text", "とても長い定型文をここに書いておくと一覧では短くなります\n2 行目");
        s.enabled = false;
        assert_eq!(
            describe(&s),
            "［停止中］F9 → 文字を入力する「とても長い定型文をここに書いておくと一覧では短く…」"
        );
        assert!(describe(&setting("Q", "block", "")).starts_with("［誤りあり］Q → "));
    }

    #[test]
    fn conflicts() {
        let remap = Hotkey::parse("Ctrl+B").unwrap();
        let excel = vec!["EXCEL.EXE".to_string()];
        // 値貼り付けと同じキー・同じアプリ
        let mut s = setting("Ctrl+B", "block", "");
        assert!(find_conflict(&[s.clone()], (remap, &excel)).is_some());
        // アプリが違えばよい
        s.apps = vec!["notepad.exe".into()];
        assert!(find_conflict(&[s.clone()], (remap, &excel)).is_none());
        // 割り当て同士
        let a = setting("Ctrl+Alt+P", "paste_plain", "");
        let mut b = setting("Ctrl+Alt+P", "block", "");
        b.apps = vec!["excel".into()];
        let msg = find_conflict(&[a.clone(), b.clone()], (remap, &excel)).unwrap();
        assert!(msg.contains("1 番目と 2 番目"));
        // 片方が止めてあればよい
        b.enabled = false;
        assert!(find_conflict(&[a, b], (remap, &excel)).is_none());
    }
}
