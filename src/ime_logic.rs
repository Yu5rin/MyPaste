//! IME 入力モード表示の判断ロジック。
//!
//! Win32 API に触れない部分だけをここに集めている。値を渡して戻り値を見るだけで
//! 試せるので、Linux 上でもテストを実行して確かめられる（ほかの部分は Windows
//! 専用のため、クレート全体の `cargo test` はそのままでは動かない）。
//!
//! 通信や描画を行う側は [`crate::ime_indicator`]。

/// 変換モードのビット（`IME_CMODE_NATIVE`）。立っていれば日本語（ひらがな／カタカナ）入力。
pub const CMODE_NATIVE: u32 = 0x0001;
/// 変換モードのビット（`IME_CMODE_KATAKANA`）。`NATIVE` と組み合わせてカタカナ入力。
pub const CMODE_KATAKANA: u32 = 0x0002;
/// 変換モードのビット（`IME_CMODE_FULLSHAPE`）。立っていれば全角。
pub const CMODE_FULLSHAPE: u32 = 0x0008;

/// 表示時間（ミリ秒）として設定できる範囲。
pub const HOLD_MS_MIN: u64 = 100;
pub const HOLD_MS_MAX: u64 = 5000;
/// フェードアウトにかける時間（ミリ秒）として設定できる範囲。0 はすぐ消す。
pub const FADE_MS_MIN: u64 = 0;
pub const FADE_MS_MAX: u64 = 2000;
/// フェードアウトにかける時間の既定（ミリ秒）。
pub const DEFAULT_FADE_MS: u64 = 250;
/// 表示の一辺（96 DPI 基準のピクセル）として設定できる範囲。
pub const SIZE_MIN: u32 = 40;
pub const SIZE_MAX: u32 = 600;

/// 不透明度（%）として設定できる範囲。低すぎると見えなくなるため 30% からにする。
pub const OPACITY_MIN: u32 = 30;
pub const OPACITY_MAX: u32 = 100;
/// 不透明度の既定（%）。少しだけ透かして背後を感じさせる。
pub const DEFAULT_OPACITY: u32 = 90;

/// 表示する位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// 前面ウィンドウのあるモニターの中央（既定）
    Center,
    /// マウスカーソルの近く
    Mouse,
    /// 文字の入力位置（キャレット）の近く。取れないアプリでは画面中央
    Caret,
}

impl Position {
    /// 設定画面の一覧の順（設定値, 表示名）。
    pub const ALL: [(Position, &'static str, &'static str); 3] = [
        (Position::Center, "center", "画面中央"),
        (Position::Mouse, "mouse", "マウスの近く"),
        (Position::Caret, "caret", "入力位置の近く"),
    ];

    /// settings.json の値から読む。知らない値は画面中央（設定ファイル全体を無効にしない）。
    pub fn from_setting(text: &str) -> Position {
        Self::ALL
            .iter()
            .find(|(_, key, _)| key.eq_ignore_ascii_case(text.trim()))
            .map(|(p, _, _)| *p)
            .unwrap_or(Position::Center)
    }

    /// settings.json に書く値。
    pub fn as_setting(self) -> &'static str {
        Self::ALL.iter().find(|(p, _, _)| *p == self).map(|(_, k, _)| *k).unwrap_or("center")
    }
}

/// 表示の色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    /// 濃いグレーに白い文字（既定）
    Dark,
    /// 明るいグレーに濃い文字
    Light,
}

impl Theme {
    /// 設定画面の一覧の順（設定値, 設定値の名前, 表示名）。
    pub const ALL: [(Theme, &'static str, &'static str); 2] =
        [(Theme::Dark, "dark", "ダーク"), (Theme::Light, "light", "ライト")];

    /// settings.json の値から読む。知らない値は濃い色。
    pub fn from_setting(text: &str) -> Theme {
        Self::ALL
            .iter()
            .find(|(_, key, _)| key.eq_ignore_ascii_case(text.trim()))
            .map(|(t, _, _)| *t)
            .unwrap_or(Theme::Dark)
    }

    /// settings.json に書く値。
    pub fn as_setting(self) -> &'static str {
        Self::ALL.iter().find(|(t, _, _)| *t == self).map(|(_, k, _)| *k).unwrap_or("dark")
    }

    /// 背景色と文字色（`0x00BBGGRR`。Win32 の COLORREF の並び）。
    pub fn colors(self) -> (u32, u32) {
        match self {
            Theme::Dark => (0x0020_2020, 0x00FF_FFFF),
            Theme::Light => (0x00F3_F3F3, 0x0020_2020),
        }
    }
}

/// 不透明度（%）を、レイヤードウィンドウの不透明度（0〜255）にする。範囲外は丸める。
pub fn opacity_to_alpha(percent: u32) -> u8 {
    let percent = percent.clamp(OPACITY_MIN, OPACITY_MAX);
    ((percent * 255 + 50) / 100) as u8
}

/// 点（マウスカーソルや入力位置）の近くに、一辺 `size` の正方形を置いたときの左上座標。
///
/// 点の右下に `gap` だけ離して置く。作業領域（`left, top, right, bottom`）からはみ出す
/// 場合は、下に収まらなければ点の上側へ、右に収まらなければ左へずらし、最後に
/// 作業領域の中へ収める。`below` には点の下端（入力位置なら文字の下端）を渡す。
#[allow(clippy::too_many_arguments)]
pub fn origin_near_point(
    x: i32,
    y_top: i32,
    y_below: i32,
    work: (i32, i32, i32, i32),
    size: i32,
    gap: i32,
) -> (i32, i32) {
    let (left, top, right, bottom) = work;
    let mut ox = x + gap;
    if ox + size > right {
        ox = x - gap - size;
    }
    let mut oy = y_below + gap;
    if oy + size > bottom {
        oy = y_top - gap - size;
    }
    // 作業領域が表示より小さい極端な場合でも、左上は作業領域の中に置く。
    ox = ox.min(right - size).max(left);
    oy = oy.min(bottom - size).max(top);
    (ox, oy)
}

/// いまの入力モードを、小さな印で出し続ける場所。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    /// 出さない。
    Off,
    /// 入力位置（キャレット）の横。入力位置が分からないアプリでは、マウスカーソルの横。
    Caret,
    /// マウスカーソルの横。
    Mouse,
}

/// 小さな印の一辺の範囲と既定（96 DPI 基準のピクセル）。
pub const BADGE_SIZE_MIN: u32 = 12;
pub const BADGE_SIZE_MAX: u32 = 48;
pub const DEFAULT_BADGE_SIZE: u32 = 20;

impl Badge {
    /// 設定ファイルの値・画面の表示名との対応（画面の並び順）。
    pub const ALL: [(Badge, &'static str, &'static str); 3] = [
        (Badge::Off, "off", "出さない"),
        (Badge::Caret, "caret", "入力位置の横（分からないときはマウスの横）"),
        (Badge::Mouse, "mouse", "マウスカーソルの横"),
    ];

    /// settings.json の値から読む。知らない値は「出さない」。
    pub fn from_setting(text: &str) -> Badge {
        let text = text.trim();
        Self::ALL
            .iter()
            .find(|(_, name, _)| name.eq_ignore_ascii_case(text))
            .map_or(Badge::Off, |(badge, _, _)| *badge)
    }

    /// settings.json に書く値。
    pub fn as_setting(self) -> &'static str {
        Self::ALL.iter().find(|(b, _, _)| *b == self).map_or("off", |(_, n, _)| *n)
    }
}

/// 小さな印を出すとき。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeWhen {
    /// 入力位置が分かるアプリでは、入力位置の横に出し続ける。分からないアプリでは、打ちそうな
    /// とき（クリック・ウィンドウの切り替え・マウスカーソルが文字入力の形になった・キーを打った・
    /// モードを切り替えた）に出し、少しして消す。
    Auto,
    /// キーを打ったとき・入力欄をクリックしたとき・モードを切り替えたときに出し、少しして消す。
    Typing,
    /// 出し続ける。
    Always,
}

impl BadgeWhen {
    /// 設定ファイルの値・画面の表示名との対応（画面の並び順）。
    pub const ALL: [(BadgeWhen, &'static str, &'static str); 3] = [
        (BadgeWhen::Auto, "auto", "おまかせ（入力欄のときだけ）"),
        (BadgeWhen::Typing, "typing", "打ち始めたときだけ"),
        (BadgeWhen::Always, "always", "出し続ける"),
    ];

    /// settings.json の値から読む。知らない値は「おまかせ」。
    pub fn from_setting(text: &str) -> BadgeWhen {
        let text = text.trim();
        Self::ALL
            .iter()
            .find(|(_, name, _)| name.eq_ignore_ascii_case(text))
            .map_or(BadgeWhen::Auto, |(when, _, _)| *when)
    }

    /// settings.json に書く値。
    pub fn as_setting(self) -> &'static str {
        Self::ALL.iter().find(|(w, _, _)| *w == self).map_or("auto", |(_, n, _)| *n)
    }
}

/// 「打ち始めたときだけ」のときに、小さな印を出しておく時間の範囲と既定（ミリ秒）。
pub const BADGE_HOLD_MS_MIN: u64 = 1000;
pub const BADGE_HOLD_MS_MAX: u64 = 10_000;
pub const DEFAULT_BADGE_HOLD_MS: u64 = 2000;
/// 小さな印の不透明度の範囲と既定（%）。
pub const BADGE_OPACITY_MIN: u32 = 20;
pub const BADGE_OPACITY_MAX: u32 = 100;
pub const DEFAULT_BADGE_OPACITY: u32 = 90;

/// 入力位置の横に出しているとき、打っている間は印を消す。最後にキーを打ってから、この時間
/// （ミリ秒）がたつまでは「打っている間」とみなす（変換の候補を選んでいる間に、出し直さないように）。
pub const BADGE_TYPING_QUIET_MS: u64 = 5000;

/// 小さな印を出すかどうかを決めるのに使う、いまの様子。時刻は `GetTickCount` のミリ秒
/// （約 49 日で 0 に戻るので、差で比べる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadgeMoment {
    /// 入力位置の横に出せるか（入力位置が分かり、そこに出す設定か）。
    pub at_caret: bool,
    /// 最後に文字のキーを打った時刻。
    pub key: Option<u32>,
    /// 最後のきっかけ（入力欄に来た・クリックした・モードを切り替えた など）の時刻。そのあとで
    /// キーを打ったら、`None` に戻す。
    pub trigger: Option<u32>,
    pub now: u32,
}

/// 小さな印を出しておくか。
///
/// 印は「打つ前に入力モードを知らせる」ためのものなので、「おまかせ」で入力位置の横に出すときは、
/// 打ち始めたら消す（変換中の文字や候補に重ならないように）。手を止めてしばらくたつか、入力欄に
/// 来直す・モードを切り替えるなどのきっかけがあれば、また出す。`keep_at_caret` が偽のときは、
/// 入力位置の横でも出し続けず、きっかけのあと `hold_ms` の間だけ出す。
pub fn badge_visible(
    when: BadgeWhen,
    keep_at_caret: bool,
    moment: BadgeMoment,
    hold_ms: u64,
) -> bool {
    let within = |at: Option<u32>, ms: u64| {
        at.is_some_and(|at| u64::from(moment.now.wrapping_sub(at)) < ms)
    };
    let triggered = within(moment.trigger, hold_ms);
    match when {
        BadgeWhen::Always => true,
        BadgeWhen::Typing => triggered || within(moment.key, hold_ms),
        BadgeWhen::Auto if moment.at_caret => {
            triggered || (keep_at_caret && !within(moment.key, BADGE_TYPING_QUIET_MS))
        }
        // 入力位置が分からないときは、マウスの横に出す（打つ文字には重ならない）ので、打ったときも出す。
        BadgeWhen::Auto => triggered || within(moment.key, hold_ms),
    }
}

/// 小さな印を出すきっかけになった出来事。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BadgeEvents {
    /// マウスのボタンを押した（押してから少しの間は、押したことにしておく。[`BADGE_CLICK_PENDING_MS`]）。
    pub click: bool,
    /// ボタンを押したとき、マウスカーソルが文字入力の形（I の形）だった。
    pub click_ibeam: bool,
    /// 入力モードが切り替わった。
    pub mode_switched: bool,
    /// マウスカーソルが文字入力の形（I の形）に変わった。
    pub ibeam_entered: bool,
    /// 入力位置（キャレット）が分かる。
    pub caret_known: bool,
    /// クリックのあと、フォーカスが編集できる入力欄にあると分かった（入力位置を知らせないアプリ
    /// でも、アクセシビリティの情報から分かることがある）。
    pub focus_editable: bool,
    /// 入力欄に来た（入力位置が分かるようになった・入力位置のある別の入力欄に移った）。
    pub caret_appeared: bool,
    /// 打っている最中（最後にキーを打ってから [`BADGE_TYPING_QUIET_MS`] たっていない）。
    pub typing: bool,
}

/// キーを打つこと以外の、きっかけとして数えるか。印は「これから文字を打つ場所で、打つ前に
/// 入力モードを知らせる」ためのものなので、文字を打つ場所だと分かる出来事だけを数える。
///
/// - モードを切り替えた: いつも数える。
/// - クリック: 入力欄をクリックしたときだけ（押した直後に入力位置が分かるようになった場合か、
///   フォーカスが編集できる入力欄にあると分かった場合）。ボタンや余白、読むだけの文字のクリックは
///   数えない。「おまかせ」で `on_ibeam` が真のときは、
///   マウスカーソルが文字入力の形のときに押した場合も数える（入力位置が分からないアプリ向け）。
/// - 入力欄に来た: 「おまかせ」のとき。ただし打っている最中は数えない（Excel のセルのように、
///   打ち始めると入力位置ができるアプリで、打っている文字の横に出さないように）。
/// - マウスカーソルが文字入力の形に変わった: 「おまかせ」で `on_ibeam` が真、入力位置が
///   分からないときだけ（分かるときは、入力位置の横に出すので要らない）。読むだけの文字の上でも
///   その形になるので、`on_ibeam` は既定で偽にしている。
pub fn badge_triggered(when: BadgeWhen, on_ibeam: bool, events: BadgeEvents) -> bool {
    match when {
        BadgeWhen::Always => false,
        BadgeWhen::Typing => {
            events.mode_switched || (events.click && (events.caret_known || events.focus_editable))
        }
        BadgeWhen::Auto => {
            events.mode_switched
                || (events.caret_appeared && !events.typing)
                || (events.click
                    && (events.caret_known
                        || events.focus_editable
                        || (on_ibeam && events.click_ibeam)))
                || (on_ibeam && events.ibeam_entered && !events.caret_known)
        }
    }
}

/// クリックを、押してからこの時間（ミリ秒）の間は「押した」ことにしておく。入力欄をクリックしても、
/// 入力位置が分かるようになるのは少しあとなので（押した直後の 1 回の監視では、まだ分からない）。
pub const BADGE_CLICK_PENDING_MS: u64 = 400;

/// 入力位置が、この回数続けて分からなかったら「入力位置が無くなった」とみなす。1 回だけ
/// 読み取れなかった（相手が忙しかった など）だけで、入力欄に来直したことにしないように。
pub const CARET_GONE_POLLS: u8 = 3;

/// 監視 1 回分の、小さな印を出すかどうかに使う様子。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BadgeInput {
    /// いまの時刻（`GetTickCount` のミリ秒）。
    pub now: u32,
    /// 前回から、文字のキーを打った。
    pub key: bool,
    /// 前回から、マウスのボタンを押した。
    pub click: bool,
    /// マウスカーソルが、いま文字入力の形（I の形）をしている。
    pub ibeam: bool,
    /// 読み取った入力モード（読み取れなかったら `None`）。
    pub mode: Option<ImeMode>,
    /// フォーカス先のウィンドウ（読み取れなかったら 0）。
    pub focus: isize,
    /// 入力位置（キャレット）が分かる。
    pub caret_known: bool,
    /// まだ数えていないクリックのあとで、フォーカスが編集できる入力欄にあると分かった。
    pub focus_editable: bool,
}

/// 小さな印のきっかけを、監視のたびに数えるための状態。
#[derive(Debug, Default)]
pub struct BadgeTracker {
    /// 最後に読み取ったモード（切り替わったことを知るため）。
    seen: Option<ImeMode>,
    /// マウスカーソルが文字入力の形だったか（変わった瞬間を知るため）。
    ibeam: bool,
    /// 入力位置が分かっていたか・続けて分からなかった回数・分かっていたときの入力先。
    caret_seen: bool,
    caret_missing: u8,
    seen_focus: isize,
    /// まだきっかけとして数えていないクリック（押した時刻と、そのとき I の形だったか）。
    click: Option<(u32, bool)>,
    /// 最後に文字のキーを打った時刻。そのあとにきっかけがあったら `None` に戻す
    /// （モードを切り替えるキーも打ったキーに数えるので、切り替えたあとに「打っている間」として
    /// 印を消さないように）。
    pub key: Option<u32>,
    /// 最後のきっかけの時刻。そのあとでキーを打ったら `None` に戻す。
    pub trigger: Option<u32>,
}

impl BadgeTracker {
    /// まだきっかけとして数えていないクリック（押した時刻と、そのとき I の形だったか）。
    pub fn pending_click(&self) -> Option<(u32, bool)> {
        self.click
    }

    /// 監視 1 回分の様子を受け取り、キーを打った時刻ときっかけの時刻を更新する。
    pub fn step(&mut self, when: BadgeWhen, on_ibeam: bool, input: BadgeInput) {
        let now = input.now;
        let within = |at: u32, ms: u64| u64::from(now.wrapping_sub(at)) < ms;
        if input.key {
            // 打ち始めたら、それまでのきっかけは終わり（入力位置の横の印を消す）。
            self.key = Some(now);
            self.trigger = None;
        }
        if input.click {
            self.click = Some((now, input.ibeam));
        }
        self.click = self.click.filter(|(at, _)| within(*at, BADGE_CLICK_PENDING_MS));

        let caret_appeared = if input.caret_known {
            let appeared = !self.caret_seen || input.focus != self.seen_focus;
            self.caret_seen = true;
            self.caret_missing = 0;
            self.seen_focus = input.focus;
            appeared
        } else {
            self.caret_missing = self.caret_missing.saturating_add(1);
            if self.caret_missing >= CARET_GONE_POLLS {
                self.caret_seen = false;
            }
            false
        };
        let events = BadgeEvents {
            click: self.click.is_some(),
            click_ibeam: self.click.is_some_and(|(_, ibeam)| ibeam),
            mode_switched: self.seen.is_some() && input.mode.is_some() && self.seen != input.mode,
            ibeam_entered: input.ibeam && !self.ibeam,
            caret_known: input.caret_known,
            focus_editable: input.focus_editable,
            caret_appeared,
            typing: self.key.is_some_and(|at| within(at, BADGE_TYPING_QUIET_MS)),
        };
        self.seen = input.mode;
        self.ibeam = input.ibeam;
        if badge_triggered(when, on_ibeam, events) {
            self.trigger = Some(now);
            // きっかけより前に打ったキーは、もう「打っている間」に数えない。
            self.key = None;
            self.click = None;
        }
    }
}

/// 2 つの四角（`left, top, right, bottom`）が重なるか、接しているか。幅の無い入力位置
/// （縦線だけのキャレット）も数えるため、端が触れているだけでも重なるとみなす。
pub fn rects_touch(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3
}

/// 小さな印の色（背景, 文字。`0x00BBGGRR`）。日本語を入力するモードは赤、半角英数は青にして、
/// 文字を読まなくても色で見分けられるようにする。
pub fn badge_colors(mode: ImeMode) -> (u32, u32) {
    match mode {
        ImeMode::AlnumHalf => (0x00C8_6E26, 0x00FF_FFFF),
        _ => (0x002F_32DC, 0x00FF_FFFF),
    }
}

/// 画面に出す入力モード。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImeMode {
    /// ひらがな（全角）
    Hiragana,
    /// 全角カタカナ
    KatakanaFull,
    /// 半角カタカナ
    KatakanaHalf,
    /// 全角英数
    AlnumFull,
    /// 半角英数（IME オフを含む）
    AlnumHalf,
}

impl ImeMode {
    /// 画面に表示する文字。Windows 標準の表示に合わせる。
    pub fn label(self) -> &'static str {
        match self {
            ImeMode::Hiragana => "あ",
            ImeMode::KatakanaFull => "カ",
            ImeMode::KatakanaHalf => "ｶ",
            ImeMode::AlnumFull => "Ａ",
            ImeMode::AlnumHalf => "A",
        }
    }
}

/// IME のオン／オフと変換モードから、表示する入力モードを決める。
///
/// IME がオフのときは、変換モードの値にかかわらず半角英数（`A`）とする
/// （Windows 標準の表示と同じ）。
pub fn mode_from_status(open: bool, conversion: u32) -> ImeMode {
    if !open {
        return ImeMode::AlnumHalf;
    }
    let native = conversion & CMODE_NATIVE != 0;
    let katakana = conversion & CMODE_KATAKANA != 0;
    let full = conversion & CMODE_FULLSHAPE != 0;
    match (native, katakana, full) {
        (true, true, true) => ImeMode::KatakanaFull,
        (true, true, false) => ImeMode::KatakanaHalf,
        // ひらがなに半角は無いため、NATIVE かつ KATAKANA でなければひらがなとする。
        (true, false, _) => ImeMode::Hiragana,
        (false, _, true) => ImeMode::AlnumFull,
        (false, _, false) => ImeMode::AlnumHalf,
    }
}

/// 「同じウィンドウ内でモードが変わったときだけ表示する」ための状態。
///
/// ウィンドウを切り替えただけ（Alt+Tab やクリックでのフォーカス移動）でモードの
/// 見た目が変わっても、それは利用者が切り替えた結果ではないので表示しない。
#[derive(Debug, Default)]
pub struct ModeTracker {
    /// 直前に観測した（フォーカス先ウィンドウ、モード）。
    last: Option<(isize, ImeMode)>,
}

impl ModeTracker {
    /// 新しく観測した状態を渡し、表示すべきなら表示するモードを返す。
    ///
    /// - `observed` が `None`（状態が取れなかった。管理者権限のウィンドウなど）の
    ///   場合は何も表示せず、記録も消す。取れるようになった直後に誤表示しないため。
    /// - フォーカス先が前回と違う場合は記録を更新するだけで表示しない。
    /// - 同じフォーカス先でモードが変わった場合だけ表示する。
    pub fn observe(&mut self, observed: Option<(isize, ImeMode)>) -> Option<ImeMode> {
        let Some((window, mode)) = observed else {
            self.last = None;
            return None;
        };
        let show = matches!(self.last, Some((w, m)) if w == window && m != mode);
        self.last = Some((window, mode));
        show.then_some(mode)
    }

    /// 記録を消す（機能をオフにしたときなど）。
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// 表示中の不透明度（0〜255）を、表示開始からの経過時間で決める。
///
/// `hold_ms` の間は `max_alpha` のまま保持し、続く `fade_ms` で 0 まで下げる。
/// `None` を返したら表示を終える。
pub fn alpha_at(elapsed_ms: u64, hold_ms: u64, fade_ms: u64, max_alpha: u8) -> Option<u8> {
    if elapsed_ms < hold_ms {
        return Some(max_alpha);
    }
    let into_fade = elapsed_ms - hold_ms;
    if fade_ms == 0 || into_fade >= fade_ms {
        return None;
    }
    let remaining = fade_ms - into_fade;
    Some((u64::from(max_alpha) * remaining / fade_ms) as u8)
}

/// 秒で書かれた文字列（例 `"0.4"`、`"1.5秒"`、全角の `"１．５"`）をミリ秒にする。
///
/// 小数第 3 位（1 ミリ秒）まで受け付け、それより細かい桁は四捨五入する。
/// 数として読めない・負の数・範囲外は `None`。
pub fn parse_seconds(text: &str, min_ms: u64, max_ms: u64) -> Option<u64> {
    let text: String = text
        .trim()
        .trim_end_matches('秒')
        .trim()
        .chars()
        .map(|c| match c {
            '０'..='９' => char::from(b'0' + (c as u32 - '０' as u32) as u8),
            '．' => '.',
            c => c,
        })
        .collect();
    // 数字と小数点だけを受け付ける（"1e3" や "-1"、"inf" などは不可）。
    if text.is_empty()
        || !text.chars().all(|c| c.is_ascii_digit() || c == '.')
        || text.matches('.').count() > 1
        || text == "."
    {
        return None;
    }
    let seconds: f64 = text.parse().ok()?;
    let ms = (seconds * 1000.0).round();
    if !(0.0..=u64::MAX as f64).contains(&ms) {
        return None;
    }
    let ms = ms as u64;
    (min_ms..=max_ms).contains(&ms).then_some(ms)
}

/// ミリ秒を、設定画面に出す秒の文字列にする（例 400 → `"0.4"`、1500 → `"1.5"`、2000 → `"2"`）。
pub fn format_seconds(ms: u64) -> String {
    let text = format!("{}.{:03}", ms / 1000, ms % 1000);
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// 96 DPI 基準の長さを、実際の DPI に合わせて拡大縮小する。
pub fn scale_for_dpi(base: i32, dpi: u32) -> i32 {
    let dpi = if dpi == 0 { 96 } else { dpi };
    ((i64::from(base) * i64::from(dpi) + 48) / 96) as i32
}

/// 表示領域（作業領域）の中央に、一辺 `size` の正方形を置いたときの左上座標。
pub fn centered_origin(left: i32, top: i32, right: i32, bottom: i32, size: i32) -> (i32, i32) {
    let x = left + (right - left - size) / 2;
    let y = top + (bottom - top - size) / 2;
    (x, y)
}

/// IME の状態を読み取れたか・読み取れなかったかの記録を、増やしすぎないための控え。
///
/// Excel や Outlook はウィンドウを切り替えるたびに一瞬だけ読み取れなくなるので、変わるたびに
/// 記録すると、一日で何千行にもなり、ほかの記録を探しにくくなる。そこで、読み取れないことは
/// **アプリごとに最初の 1 回だけ**記録し、読み取れるようになったことは記録しない
/// （監視を始めて最初に読み取れたときだけ記録する）。
#[derive(Debug, Default)]
pub struct ReadabilityLog {
    last_ok: Option<bool>,
    /// 読み取れないことを、すでに記録したアプリ。
    reported: Vec<String>,
}

/// 読み取れないことを記録したアプリを覚えておく数の上限（際限なく増えないように）。
const MAX_REPORTED_APPS: usize = 200;

impl ReadabilityLog {
    /// 監視を止めたとき（入力モード表示を OFF にしたとき）。次に始めたときの最初の結果を記録する。
    pub fn reset(&mut self) {
        self.last_ok = None;
    }

    /// 読み取った結果を受け取り、記録する文があれば返す。`app` は前面のアプリの名前を返す
    /// （読み取れなくなったときだけ呼ぶ）。
    pub fn observe(&mut self, ok: bool, app: impl FnOnce() -> String) -> Option<String> {
        let previous = self.last_ok.replace(ok);
        if previous == Some(ok) {
            return None;
        }
        if ok {
            return previous
                .is_none()
                .then(|| "入力モード: IME の状態を読み取れます".to_string());
        }
        let app = app();
        if self.reported.contains(&app) || self.reported.len() >= MAX_REPORTED_APPS {
            return None;
        }
        let line = format!(
            "入力モード: 前面のウィンドウ（{app}）の IME の状態を読み取れません\
             （このアプリについては、以後は記録しません）"
        );
        self.reported.push(app);
        Some(line)
    }
}

/// アプリごとの入力モード（そのアプリに切り替えたときに、自動でこの入力モードにする）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppImeMode {
    /// 日本語入力（IME オン・ひらがな）。
    Japanese,
    /// 英字（IME オフ）。
    English,
}

impl AppImeMode {
    /// 選択肢（値・設定ファイルの表記・設定画面の表示）。
    pub const ALL: [(AppImeMode, &'static str, &'static str); 2] = [
        (AppImeMode::Japanese, "on", "日本語（ひらがな）"),
        (AppImeMode::English, "off", "英字（IME オフ）"),
    ];

    /// 設定ファイルの値から読む（大文字小文字は区別しない）。知らない値は `None`。
    pub fn from_setting(text: &str) -> Option<AppImeMode> {
        let text = text.trim();
        AppImeMode::ALL
            .iter()
            .find(|(_, setting, _)| setting.eq_ignore_ascii_case(text))
            .map(|(mode, _, _)| *mode)
    }

    /// 設定ファイルに書く値。
    pub fn as_setting(self) -> &'static str {
        match self {
            AppImeMode::Japanese => "on",
            AppImeMode::English => "off",
        }
    }

    /// 一覧に出す短い名前。
    pub fn short_label(self) -> &'static str {
        match self {
            AppImeMode::Japanese => "日本語",
            AppImeMode::English => "英字",
        }
    }
}

/// アプリごとの入力モード 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppModeRule {
    /// プロセス名（[`crate::remap_logic::normalize_app`] でそろえたもの。例 `EXCEL.EXE`）。
    pub app: String,
    pub mode: AppImeMode,
}

impl AppModeRule {
    /// 設定画面の一覧に出す文字（例 `EXCEL.EXE → 英字`）。
    pub fn label(&self) -> String {
        format!("{} → {}", self.app, self.mode.short_label())
    }
}

/// 一覧にアプリを足す。すでにあるアプリなら、入力モードだけを置き換える。足した（置き換えた）
/// 位置を返す。アプリ名が空なら何もせず `None`。
pub fn add_app_mode_rule(rules: &mut Vec<AppModeRule>, app: &str, mode: AppImeMode) -> Option<usize> {
    let app = crate::remap_logic::normalize_app(app)?;
    if let Some(index) = rules.iter().position(|r| r.app == app) {
        rules[index].mode = mode;
        return Some(index);
    }
    rules.push(AppModeRule { app, mode });
    Some(rules.len() - 1)
}

/// 設定ファイルの値（アプリ名と `"on"` / `"off"`）から一覧を作る。名前の書き方の揺れは直し、
/// 名前が空・入力モードが読めないものは飛ばし、同じアプリが 2 度あれば後のものを使う。
pub fn app_mode_rules<'a>(items: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<AppModeRule> {
    let mut rules = Vec::new();
    for (app, mode) in items {
        if let Some(mode) = AppImeMode::from_setting(mode) {
            add_app_mode_rule(&mut rules, app, mode);
        }
    }
    rules
}

/// プロセス名に合う入力モード（大文字小文字は区別しない）。無ければ `None`。
pub fn app_mode_for(rules: &[AppModeRule], process_name: &str) -> Option<AppImeMode> {
    rules
        .iter()
        .find(|r| r.app.eq_ignore_ascii_case(process_name))
        .map(|r| r.mode)
}

/// IME へ送る変更。`None` の項目は変えない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImeChange {
    /// IME のオン（`true`）・オフ（`false`）。
    pub open: Option<bool>,
    /// 変換モード。
    pub conversion: Option<u32>,
}

/// いまの IME の状態（オン・オフと変換モード）を、`mode` にするための変更。すでにそうなって
/// いれば `None`。
///
/// 日本語は、ひらがな（`NATIVE` と `FULLSHAPE`、`KATAKANA` は外す）にする。ローマ字入力・
/// かな入力の別など、ほかのビットは利用者のものを残す。
pub fn ime_change(mode: AppImeMode, open: bool, conversion: u32) -> Option<ImeChange> {
    match mode {
        AppImeMode::English => open.then_some(ImeChange {
            open: Some(false),
            conversion: None,
        }),
        AppImeMode::Japanese => {
            let hiragana = (conversion & !CMODE_KATAKANA) | CMODE_NATIVE | CMODE_FULLSHAPE;
            if open && conversion == hiragana {
                return None;
            }
            Some(ImeChange {
                open: (!open).then_some(true),
                // IME がオフの間の変換モードは古いことがあるので、オンにするときは必ず送る。
                conversion: (!open || conversion != hiragana).then_some(hiragana),
            })
        }
    }
}

/// 離れてすぐ（この回数の監視のうち）に戻ってきたアプリは、離れなかったことにする
/// （Alt+Tab の切り替え画面や、切り替えの途中で一瞬だけ前面が変わったときに、利用者が
/// 自分で変えた入力モードを戻してしまわないように）。100ms ごとの監視で 0.5 秒。
pub const APP_RETURN_QUIET_POLLS: u32 = 5;
/// 切り替えた先の IME の状態がすぐに読めないとき（Excel などは切り替えた直後に一瞬
/// 読めなくなる）に、読めるようになるまで待つ監視の回数。100ms ごとの監視で 1 秒。
pub const APP_MODE_PENDING_POLLS: u32 = 10;

/// 「アプリに切り替えたときに 1 度だけ、そのアプリの入力モードにする」ための状態。
///
/// アプリは、前面ウィンドウのプロセス（プロセス ID）で見分ける。同じプロセスの別の
/// ウィンドウへ移ったときは、同じアプリのままとして扱い、切り替え直さない。切り替えたあとは
/// 利用者が自由に変えてよく、そのアプリから離れて戻ってきたときにだけ、また切り替える。
#[derive(Debug, Default)]
pub struct AppModeTracker {
    /// 監視を始めてから、まだ 1 度も見ていないか（`false` の間は、最初の 1 回）。
    started: bool,
    /// いま前面にあるアプリ（前回の監視で見たもの。前面ウィンドウが無ければ `None`）。
    current: Option<u32>,
    /// そのアプリが前面にある間の監視の回数。
    current_polls: u32,
    /// 直前に離れたアプリと、離れてからの監視の回数。
    left: Option<(u32, u32)>,
    /// まだ当てていない入力モードと、待てる残りの回数。
    pending: Option<(AppImeMode, u32)>,
}

impl AppModeTracker {
    /// 監視のたびに呼ぶ。`app` は前面ウィンドウのプロセス ID、`ime` は読めた IME の状態
    /// （オン・オフと変換モード。読めなければ `None`）、`rule` は前面のアプリの入力モードを
    /// 調べる（別のアプリへ移ったときだけ呼ぶ）。いま IME へ送る変更を返す。
    ///
    /// - 監視を始めて最初の 1 回は、いま前面にあるアプリを覚えるだけにする（起動したときや、
    ///   機能を ON にしたときに、使っている最中のアプリの入力モードを変えないように）。
    /// - 別のアプリへ移ったら、そのアプリの入力モードを覚え、IME の状態を読めたときに 1 度だけ
    ///   当てる。すでにその入力モードなら何もしない。しばらく読めなければあきらめる。
    /// - 離れてすぐに戻ってきたアプリ（[`APP_RETURN_QUIET_POLLS`]）は、離れなかったことにする。
    pub fn observe(
        &mut self,
        app: Option<u32>,
        ime: Option<(bool, u32)>,
        rule: impl FnOnce() -> Option<AppImeMode>,
    ) -> Option<ImeChange> {
        if !self.started {
            self.started = true;
            self.current = app;
            self.current_polls = APP_RETURN_QUIET_POLLS;
            return None;
        }
        if let Some((_, polls)) = self.left.as_mut() {
            *polls = polls.saturating_add(1);
        }
        if app == self.current {
            self.current_polls = self.current_polls.saturating_add(1);
        } else {
            let quick_return = matches!(
                (app, self.left),
                (Some(a), Some((l, polls))) if a == l && polls <= APP_RETURN_QUIET_POLLS
            );
            // 一瞬だけ前面になったもの（切り替え画面など）は、離れたアプリとして覚えない。
            if let Some(previous) = self.current {
                if self.current_polls >= APP_RETURN_QUIET_POLLS || self.left.is_none() {
                    self.left = Some((previous, 0));
                }
            }
            self.current = app;
            if quick_return {
                self.left = None;
                self.current_polls = APP_RETURN_QUIET_POLLS;
                self.pending = None;
            } else {
                self.current_polls = 0;
                self.pending = app.and_then(|_| rule()).map(|mode| (mode, APP_MODE_PENDING_POLLS));
            }
        }

        let (mode, remaining) = self.pending?;
        match ime {
            Some((open, conversion)) => {
                self.pending = None;
                ime_change(mode, open, conversion)
            }
            None => {
                self.pending = (remaining > 1).then_some((mode, remaining - 1));
                None
            }
        }
    }

    /// 記録を消す（機能を OFF にしたとき・一覧が空のとき）。次の監視は、最初の 1 回として扱う。
    pub fn reset(&mut self) {
        *self = AppModeTracker::default();
    }
}

#[cfg(test)]
mod app_mode_tests {
    use super::*;

    const N: u32 = CMODE_NATIVE;
    const K: u32 = CMODE_KATAKANA;
    const F: u32 = CMODE_FULLSHAPE;
    /// `IME_CMODE_ROMAN`（ローマ字入力）。
    const R: u32 = 0x0010;

    const OFF: Option<ImeChange> = Some(ImeChange {
        open: Some(false),
        conversion: None,
    });

    fn english() -> Option<AppImeMode> {
        Some(AppImeMode::English)
    }

    #[test]
    fn reads_and_writes_settings() {
        assert_eq!(AppImeMode::from_setting("on"), Some(AppImeMode::Japanese));
        assert_eq!(AppImeMode::from_setting(" OFF "), Some(AppImeMode::English));
        assert_eq!(AppImeMode::from_setting("kana"), None);
        for (mode, setting, _) in AppImeMode::ALL {
            assert_eq!(mode.as_setting(), setting);
            assert_eq!(AppImeMode::from_setting(setting), Some(mode));
        }
    }

    #[test]
    fn builds_rules_from_settings() {
        let rules = app_mode_rules([
            ("excel", "off"),
            ("C:\\Tools\\Slack.exe", "on"),
            ("", "on"),
            ("notepad.exe", "unknown"),
            ("EXCEL.EXE", "on"),
        ]);
        assert_eq!(
            rules,
            [
                AppModeRule { app: "EXCEL.EXE".into(), mode: AppImeMode::Japanese },
                AppModeRule { app: "SLACK.EXE".into(), mode: AppImeMode::Japanese },
            ]
        );
        assert_eq!(rules[0].label(), "EXCEL.EXE → 日本語");
        assert_eq!(app_mode_for(&rules, "slack.exe"), Some(AppImeMode::Japanese));
        assert_eq!(app_mode_for(&rules, "NOTEPAD.EXE"), None);
    }

    #[test]
    fn adding_a_listed_app_replaces_its_mode() {
        let mut rules = Vec::new();
        assert_eq!(add_app_mode_rule(&mut rules, "Excel.exe", AppImeMode::Japanese), Some(0));
        assert_eq!(add_app_mode_rule(&mut rules, "slack", AppImeMode::Japanese), Some(1));
        assert_eq!(add_app_mode_rule(&mut rules, "EXCEL", AppImeMode::English), Some(0));
        assert_eq!(add_app_mode_rule(&mut rules, "  ", AppImeMode::English), None);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].label(), "EXCEL.EXE → 英字");
    }

    #[test]
    fn change_for_english_turns_ime_off_only_when_on() {
        assert_eq!(ime_change(AppImeMode::English, true, N | F | R), OFF);
        assert_eq!(ime_change(AppImeMode::English, false, N | F | R), None);
    }

    #[test]
    fn change_for_japanese_sets_hiragana_and_keeps_roman_bit() {
        // すでにひらがな
        assert_eq!(ime_change(AppImeMode::Japanese, true, N | F | R), None);
        // オフからは、オンにしてひらがなにする（ローマ字入力のまま）
        assert_eq!(
            ime_change(AppImeMode::Japanese, false, R),
            Some(ImeChange { open: Some(true), conversion: Some(N | F | R) })
        );
        // オフでも、変換モードは必ず送る
        assert_eq!(
            ime_change(AppImeMode::Japanese, false, N | F),
            Some(ImeChange { open: Some(true), conversion: Some(N | F) })
        );
        // オンの半角英数・カタカナから。かな入力（ローマ字のビット無し）はそのまま
        assert_eq!(
            ime_change(AppImeMode::Japanese, true, 0),
            Some(ImeChange { open: None, conversion: Some(N | F) })
        );
        assert_eq!(
            ime_change(AppImeMode::Japanese, true, N | K | F | R),
            Some(ImeChange { open: None, conversion: Some(N | F | R) })
        );
    }

    #[test]
    fn first_poll_after_start_only_remembers() {
        let mut t = AppModeTracker::default();
        let mut asked = false;
        assert_eq!(t.observe(Some(1), Some((true, N | F)), || { asked = true; english() }), None);
        assert!(!asked, "最初の 1 回はアプリを調べない");
        assert_eq!(t.observe(Some(1), Some((true, N | F)), english), None);
    }

    #[test]
    fn applies_once_on_arrival_and_lets_the_user_switch() {
        let mut t = AppModeTracker::default();
        t.observe(Some(1), None, || None);
        // Excel に移った
        assert_eq!(t.observe(Some(2), Some((true, N | F)), english), OFF);
        // その後、利用者が日本語にしても、戻さない
        for _ in 0..20 {
            assert_eq!(t.observe(Some(2), Some((true, N | F)), english), None);
        }
    }

    #[test]
    fn does_nothing_when_already_in_the_mode_or_no_rule() {
        let mut t = AppModeTracker::default();
        t.observe(Some(1), None, || None);
        assert_eq!(t.observe(Some(2), Some((false, 0)), english), None);
        assert_eq!(t.observe(Some(3), Some((true, N | F)), || None), None);
    }

    #[test]
    fn same_app_other_window_is_not_an_arrival() {
        // 同じプロセスの別のウィンドウ（Excel の別のブック）は、プロセス ID が同じなので切り替え直さない
        let mut t = AppModeTracker::default();
        t.observe(Some(1), None, || None);
        assert_eq!(t.observe(Some(2), Some((true, N | F)), english), OFF);
        let mut asked = false;
        assert_eq!(t.observe(Some(2), Some((true, N | F)), || { asked = true; english() }), None);
        assert!(!asked);
    }

    #[test]
    fn returning_to_the_app_applies_again() {
        let mut t = AppModeTracker::default();
        t.observe(Some(1), None, || None);
        assert_eq!(t.observe(Some(2), Some((true, N | F)), english), OFF);
        // 別のアプリへ移り、しばらく使う
        for _ in 0..APP_RETURN_QUIET_POLLS + 1 {
            t.observe(Some(3), Some((true, N | F)), || None);
        }
        // 戻ってきたら、また英字にする
        assert_eq!(t.observe(Some(2), Some((true, N | F)), english), OFF);
    }

    #[test]
    fn quick_return_is_not_leaving() {
        // Alt+Tab の切り替え画面が一瞬だけ前面になって、同じアプリに戻った
        let mut t = AppModeTracker::default();
        t.observe(Some(1), None, || None);
        assert_eq!(t.observe(Some(2), Some((true, N | F)), english), OFF);
        for _ in 0..APP_RETURN_QUIET_POLLS {
            t.observe(Some(2), Some((true, N | F)), english);
        }
        t.observe(Some(9), None, || None);
        t.observe(None, None, || None);
        t.observe(Some(9), None, || None);
        let mut asked = false;
        assert_eq!(t.observe(Some(2), Some((true, N | F)), || { asked = true; english() }), None);
        assert!(!asked);
    }

    #[test]
    fn waits_until_the_state_can_be_read() {
        let mut t = AppModeTracker::default();
        t.observe(Some(1), None, || None);
        // 切り替えた直後は読めない
        assert_eq!(t.observe(Some(2), None, english), None);
        assert_eq!(t.observe(Some(2), None, english), None);
        assert_eq!(t.observe(Some(2), Some((true, N | F)), english), OFF);
        assert_eq!(t.observe(Some(2), Some((true, N | F)), english), None);
    }

    #[test]
    fn gives_up_when_the_state_stays_unreadable() {
        // 管理者として動いているアプリなど
        let mut t = AppModeTracker::default();
        t.observe(Some(1), None, || None);
        for _ in 0..APP_MODE_PENDING_POLLS {
            assert_eq!(t.observe(Some(2), None, english), None);
        }
        assert_eq!(t.observe(Some(2), Some((true, N | F)), english), None);
    }

    #[test]
    fn changed_rules_are_used_from_the_next_arrival() {
        let mut t = AppModeTracker::default();
        t.observe(Some(1), None, || None);
        // 一覧に無いアプリにいる間に、一覧が変わっても、そのアプリでは切り替えない
        assert_eq!(t.observe(Some(2), Some((true, N | F)), || None), None);
        assert_eq!(t.observe(Some(2), Some((true, N | F)), english), None);
        // 別のアプリ（設定画面など）へ移ってから戻ると、新しい一覧で切り替える
        for _ in 0..APP_RETURN_QUIET_POLLS + 1 {
            t.observe(Some(3), None, || None);
        }
        assert_eq!(t.observe(Some(2), Some((true, N | F)), english), OFF);
    }

    #[test]
    fn reset_treats_next_poll_as_first() {
        let mut t = AppModeTracker::default();
        t.observe(Some(1), None, || None);
        t.reset();
        assert_eq!(t.observe(Some(2), Some((true, N | F)), english), None);
        assert_eq!(t.observe(Some(3), Some((true, N | F)), english), OFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: u32 = CMODE_NATIVE;
    const K: u32 = CMODE_KATAKANA;
    const F: u32 = CMODE_FULLSHAPE;

    #[test]
    fn maps_each_mode_like_windows() {
        assert_eq!(mode_from_status(true, N | F), ImeMode::Hiragana);
        assert_eq!(mode_from_status(true, N | K | F), ImeMode::KatakanaFull);
        assert_eq!(mode_from_status(true, N | K), ImeMode::KatakanaHalf);
        assert_eq!(mode_from_status(true, F), ImeMode::AlnumFull);
        assert_eq!(mode_from_status(true, 0), ImeMode::AlnumHalf);
    }

    #[test]
    fn ime_off_is_always_half_alnum() {
        assert_eq!(mode_from_status(false, N | F), ImeMode::AlnumHalf);
        assert_eq!(mode_from_status(false, N | K | F), ImeMode::AlnumHalf);
    }

    #[test]
    fn labels_match_windows() {
        let labels: Vec<_> = [
            ImeMode::Hiragana,
            ImeMode::KatakanaFull,
            ImeMode::KatakanaHalf,
            ImeMode::AlnumFull,
            ImeMode::AlnumHalf,
        ]
        .iter()
        .map(|m| m.label())
        .collect();
        assert_eq!(labels, ["あ", "カ", "ｶ", "Ａ", "A"]);
    }

    #[test]
    fn first_observation_does_not_show() {
        let mut t = ModeTracker::default();
        assert_eq!(t.observe(Some((1, ImeMode::Hiragana))), None);
    }

    #[test]
    fn mode_change_in_same_window_shows() {
        let mut t = ModeTracker::default();
        t.observe(Some((1, ImeMode::AlnumHalf)));
        assert_eq!(t.observe(Some((1, ImeMode::Hiragana))), Some(ImeMode::Hiragana));
        // 変わっていなければ出さない
        assert_eq!(t.observe(Some((1, ImeMode::Hiragana))), None);
    }

    #[test]
    fn switching_windows_does_not_show() {
        // Alt+Tab で、モードの違うウィンドウへ移っただけ
        let mut t = ModeTracker::default();
        t.observe(Some((1, ImeMode::Hiragana)));
        assert_eq!(t.observe(Some((2, ImeMode::AlnumHalf))), None);
        // 移った先で切り替えたら出す
        assert_eq!(t.observe(Some((2, ImeMode::Hiragana))), Some(ImeMode::Hiragana));
    }

    #[test]
    fn unreadable_state_resets_and_never_shows() {
        let mut t = ModeTracker::default();
        t.observe(Some((1, ImeMode::Hiragana)));
        // 管理者権限のウィンドウなどで取れない
        assert_eq!(t.observe(None), None);
        // 取れるようになった直後は、前回と違っても出さない
        assert_eq!(t.observe(Some((1, ImeMode::AlnumHalf))), None);
    }

    #[test]
    fn alpha_holds_then_fades_then_ends() {
        assert_eq!(alpha_at(0, 400, 250, 230), Some(230));
        assert_eq!(alpha_at(399, 400, 250, 230), Some(230));
        assert_eq!(alpha_at(400, 400, 250, 230), Some(230));
        let mid = alpha_at(525, 400, 250, 230).unwrap();
        assert!(mid > 100 && mid < 130, "フェードの中間はおよそ半分: {mid}");
        assert_eq!(alpha_at(650, 400, 250, 230), None);
        assert_eq!(alpha_at(10_000, 400, 250, 230), None);
    }

    #[test]
    fn zero_fade_ends_right_after_hold() {
        assert_eq!(alpha_at(399, 400, 0, 230), Some(230));
        assert_eq!(alpha_at(400, 400, 0, 230), None);
    }

    #[test]
    fn parses_seconds() {
        assert_eq!(parse_seconds("0.4", 100, 5000), Some(400));
        assert_eq!(parse_seconds(" 1.5 ", 100, 5000), Some(1500));
        assert_eq!(parse_seconds("2", 100, 5000), Some(2000));
        assert_eq!(parse_seconds(".5", 100, 5000), Some(500));
        assert_eq!(parse_seconds("1.", 100, 5000), Some(1000));
        assert_eq!(parse_seconds("1.5秒", 100, 5000), Some(1500));
        assert_eq!(parse_seconds("１．５", 100, 5000), Some(1500));
        assert_eq!(parse_seconds("0.1234", 100, 5000), Some(123));
        assert_eq!(parse_seconds("0", 0, 2000), Some(0));
    }

    #[test]
    fn rejects_bad_seconds() {
        for text in ["", ".", "abc", "-1", "1e3", "inf", "1.2.3", "1,5", "0.05"] {
            assert_eq!(parse_seconds(text, 100, 5000), None, "{text}");
        }
        assert_eq!(parse_seconds("5.1", 100, 5000), None);
        assert_eq!(parse_seconds("99999999999999999999999", 0, 5000), None);
    }

    #[test]
    fn formats_seconds() {
        assert_eq!(format_seconds(400), "0.4");
        assert_eq!(format_seconds(1500), "1.5");
        assert_eq!(format_seconds(2000), "2");
        assert_eq!(format_seconds(250), "0.25");
        assert_eq!(format_seconds(0), "0");
        assert_eq!(format_seconds(1234), "1.234");
        // 表示した文字列を読み戻すと同じ値になる
        for ms in [0, 1, 100, 250, 400, 999, 1000, 1500, 5000] {
            assert_eq!(parse_seconds(&format_seconds(ms), 0, 5000), Some(ms));
        }
    }

    #[test]
    fn position_and_theme_settings() {
        assert_eq!(Position::from_setting("mouse"), Position::Mouse);
        assert_eq!(Position::from_setting(" Caret "), Position::Caret);
        assert_eq!(Position::from_setting("どこか"), Position::Center);
        for (p, key, _) in Position::ALL {
            assert_eq!(Position::from_setting(p.as_setting()), p);
            assert_eq!(p.as_setting(), key);
        }
        assert_eq!(Theme::from_setting("LIGHT"), Theme::Light);
        assert_eq!(Theme::from_setting(""), Theme::Dark);
        for (t, _, _) in Theme::ALL {
            assert_eq!(Theme::from_setting(t.as_setting()), t);
        }
    }

    #[test]
    fn opacity_maps_to_alpha() {
        assert_eq!(opacity_to_alpha(100), 255);
        assert_eq!(opacity_to_alpha(90), 230); // これまでの見た目と同じ
        assert_eq!(opacity_to_alpha(30), 77);
        assert_eq!(opacity_to_alpha(0), 77); // 下限に丸める
        assert_eq!(opacity_to_alpha(500), 255);
    }

    #[test]
    fn near_point_prefers_below_right() {
        let work = (0, 0, 1920, 1040);
        assert_eq!(origin_near_point(500, 500, 520, work, 120, 16), (516, 536));
    }

    #[test]
    fn near_point_flips_at_edges() {
        let work = (0, 0, 1920, 1040);
        // 右端の近く → 左側へ
        assert_eq!(origin_near_point(1900, 500, 520, work, 120, 16), (1764, 536));
        // 下端の近く（タスクバーの上）→ 上側へ。入力位置なら文字の上端より上に置く
        assert_eq!(origin_near_point(500, 1000, 1020, work, 120, 16), (516, 864));
    }

    #[test]
    fn near_point_stays_inside_secondary_monitor() {
        // 主モニターの左にあるモニター（座標が負）
        let work = (-1920, 0, 0, 1040);
        assert_eq!(origin_near_point(-10, 10, 30, work, 120, 16), (-146, 46));
        // 作業領域より大きい表示でも左上は領域内
        assert_eq!(origin_near_point(-10, 10, 30, (-100, 0, 0, 50), 120, 16), (-100, 0));
    }

    #[test]
    fn dpi_scaling() {
        assert_eq!(scale_for_dpi(120, 96), 120);
        assert_eq!(scale_for_dpi(120, 144), 180); // 150%
        assert_eq!(scale_for_dpi(120, 192), 240); // 200%
        assert_eq!(scale_for_dpi(120, 0), 120); // 取れなかったら 96 扱い
    }

    #[test]
    fn centered_on_secondary_monitor() {
        // 主モニターの右にある 1920x1080 のモニター（作業領域の下 40px はタスクバー）
        assert_eq!(centered_origin(1920, 0, 3840, 1040, 120), (2820, 460));
        // 主モニターの左にある（座標が負）
        assert_eq!(centered_origin(-1920, 0, 0, 1080, 120), (-1020, 480));
    }

    #[test]
    fn readability_log_reports_each_app_once() {
        let mut log = ReadabilityLog::default();
        assert_eq!(log.observe(true, || unreachable!()).as_deref(), Some("入力モード: IME の状態を読み取れます"));
        assert_eq!(log.observe(true, || unreachable!()), None);
        let first = log.observe(false, || "EXCEL.EXE".into()).unwrap();
        assert!(first.contains("EXCEL.EXE"));
        // 読み取れるようになったことは記録しない。同じアプリでまた読み取れなくなっても記録しない。
        assert_eq!(log.observe(true, || unreachable!()), None);
        assert_eq!(log.observe(false, || "EXCEL.EXE".into()), None);
        assert_eq!(log.observe(false, || unreachable!()), None);
        // 別のアプリは記録する。
        assert_eq!(log.observe(true, || unreachable!()), None);
        assert!(log.observe(false, || "OUTLOOK.EXE".into()).unwrap().contains("OUTLOOK.EXE"));
        // 監視をやめて始め直したら、最初に読み取れたことを記録する。
        log.reset();
        assert!(log.observe(true, || unreachable!()).is_some());
    }

    #[test]
    fn readability_log_has_a_limit() {
        let mut log = ReadabilityLog::default();
        for i in 0..MAX_REPORTED_APPS {
            assert!(log.observe(false, || format!("APP{i}.EXE")).is_some());
            log.observe(true, || unreachable!());
        }
        assert_eq!(log.observe(false, || "MORE.EXE".into()), None);
    }

    #[test]
    fn badge_round_trip() {
        for (badge, name, _) in Badge::ALL {
            assert_eq!(Badge::from_setting(name), badge);
            assert_eq!(badge.as_setting(), name);
        }
        assert_eq!(Badge::from_setting(" CARET "), Badge::Caret);
        assert_eq!(Badge::from_setting("unknown"), Badge::Off);
        for (when, name, _) in BadgeWhen::ALL {
            assert_eq!(BadgeWhen::from_setting(name), when);
            assert_eq!(when.as_setting(), name);
        }
        assert_eq!(BadgeWhen::from_setting("?"), BadgeWhen::Auto);
    }

    #[test]
    fn badge_shows_for_a_while_after_trigger() {
        use BadgeWhen::{Always, Typing};
        let at = |at_caret, key, trigger, now| BadgeMoment { at_caret, key, trigger, now };
        assert!(badge_visible(Always, true, at(false, None, None, 0), 3000));
        // 「打ち始めたときだけ」: 打った・きっかけのあと、決めた時間だけ。
        assert!(!badge_visible(Typing, true, at(false, None, None, 5000), 3000));
        assert!(badge_visible(Typing, true, at(false, Some(5000), None, 7999), 3000));
        assert!(!badge_visible(Typing, true, at(false, Some(5000), None, 8000), 3000));
        assert!(badge_visible(Typing, true, at(true, None, Some(5000), 6000), 3000));
        assert!(!badge_visible(Typing, true, at(true, None, None, 5000), 3000));
        // 時刻が 0 に戻っても、差で比べるので続けて数えられる。
        assert!(badge_visible(Typing, true, at(false, Some(u32::MAX - 10), None, 100), 3000));
        assert!(!badge_visible(Typing, true, at(false, Some(u32::MAX - 10), None, 5000), 3000));
    }

    #[test]
    fn auto_badge_hides_while_typing_at_caret() {
        use BadgeWhen::Auto;
        let at = |at_caret, key, trigger, now| BadgeMoment { at_caret, key, trigger, now };
        let quiet = BADGE_TYPING_QUIET_MS as u32;
        // 入力位置の横: 打っていなければ出し続ける。
        assert!(badge_visible(Auto, true, at(true, None, None, 50_000), 3000));
        // 打ち始めたら消し、手を止めてしばらくたったら、また出す。
        assert!(!badge_visible(Auto, true, at(true, Some(50_000), None, 50_100), 3000));
        assert!(!badge_visible(Auto, true, at(true, Some(50_000), None, 50_000 + quiet - 1), 3000));
        assert!(badge_visible(Auto, true, at(true, Some(50_000), None, 50_000 + quiet), 3000));
        // 打っている途中でも、モードを切り替えた直後などは出す。
        assert!(badge_visible(Auto, true, at(true, Some(50_000), Some(50_050), 50_100), 3000));
        // 出し続けない設定: きっかけのあとだけ。打っても出さない。
        assert!(!badge_visible(Auto, false, at(true, None, None, 50_000), 3000));
        assert!(badge_visible(Auto, false, at(true, None, Some(49_000), 50_000), 3000));
        assert!(!badge_visible(Auto, false, at(true, None, Some(40_000), 50_000), 3000));
        assert!(!badge_visible(Auto, false, at(true, Some(49_900), None, 50_000), 3000));
        // 入力位置が分からない（マウスの横）: 打ったとき・きっかけのあとだけ。
        assert!(!badge_visible(Auto, true, at(false, None, None, 50_000), 3000));
        assert!(badge_visible(Auto, true, at(false, Some(49_000), None, 50_000), 3000));
        assert!(badge_visible(Auto, true, at(false, None, Some(49_000), 50_000), 3000));
        assert!(!badge_visible(Auto, true, at(false, Some(40_000), None, 50_000), 3000));
    }

    #[test]
    fn badge_triggers_depend_on_when() {
        use BadgeWhen::{Always, Auto, Typing};
        let none = BadgeEvents::default();
        let click = BadgeEvents { click: true, ..none };
        let click_in_input = BadgeEvents { click: true, caret_known: true, ..none };
        let click_on_text = BadgeEvents { click: true, click_ibeam: true, ..none };
        let ibeam = BadgeEvents { ibeam_entered: true, ..none };
        let ibeam_at_caret = BadgeEvents { caret_known: true, ..ibeam };
        let arrived = BadgeEvents { caret_known: true, caret_appeared: true, ..none };
        let switched = BadgeEvents { mode_switched: true, ..none };
        for when in [Auto, Typing] {
            assert!(!badge_triggered(when, true, none));
            assert!(badge_triggered(when, true, switched));
            assert!(badge_triggered(when, true, click_in_input));
            // 入力欄ではない所（ボタン・余白）のクリックは数えない。
            assert!(!badge_triggered(when, true, click));
            // 入力位置を知らせない入力欄（エクスプローラーの検索欄など）でも、フォーカスが
            // 編集できる入力欄にあると分かれば数える。
            let click_in_edit = BadgeEvents { click: true, focus_editable: true, ..none };
            assert!(badge_triggered(when, false, click_in_edit));
        }
        // 入力欄に来た・文字入力の形のカーソルで押した・その形に変わった、は「おまかせ」だけ。
        assert!(badge_triggered(Auto, true, arrived));
        assert!(badge_triggered(Auto, true, click_on_text));
        assert!(badge_triggered(Auto, true, ibeam));
        // マウスカーソルの形を見ない設定（既定）では、読むだけの文字の上に乗せても・押しても出さない。
        assert!(!badge_triggered(Auto, false, click_on_text));
        assert!(!badge_triggered(Auto, false, ibeam));
        assert!(!badge_triggered(Auto, true, ibeam_at_caret));
        assert!(!badge_triggered(Typing, true, arrived));
        assert!(!badge_triggered(Typing, true, click_on_text));
        assert!(!badge_triggered(Typing, true, ibeam));
        assert!(!badge_triggered(Always, true, switched));
        // 打っている最中に入力位置ができた（Excel のセルで打ち始めた など）ときは数えない。
        let arrived_while_typing = BadgeEvents { typing: true, ..arrived };
        assert!(!badge_triggered(Auto, true, arrived_while_typing));
        assert!(badge_triggered(Auto, true, BadgeEvents { typing: true, ..switched }));
    }

    /// 入力位置の横に出し続ける「おまかせ」で、監視を 100ms ごとに進めながら、見えているかを返す。
    fn run(tracker: &mut BadgeTracker, input: BadgeInput) -> bool {
        tracker.step(BadgeWhen::Auto, true, input);
        let moment = BadgeMoment {
            at_caret: input.caret_known,
            key: tracker.key,
            trigger: tracker.trigger,
            now: input.now,
        };
        badge_visible(BadgeWhen::Auto, true, moment, 2000)
    }

    fn at_caret(now: u32, mode: ImeMode) -> BadgeInput {
        BadgeInput {
            now,
            mode: Some(mode),
            focus: 7,
            caret_known: true,
            ..BadgeInput::default()
        }
    }

    #[test]
    fn mode_switch_key_does_not_hide_the_badge_afterwards() {
        // 半角/全角キーで切り替えた。キーそのものも打ったキーに数えられる。
        let mut t = BadgeTracker::default();
        for now in (10_000..20_000).step_by(100) {
            run(&mut t, at_caret(now, ImeMode::AlnumHalf));
        }
        // キーを打ったと分かった監視では、まだモードが変わっていないことがある（その 1 回は消える）。
        run(&mut t, BadgeInput { key: true, ..at_caret(20_000, ImeMode::AlnumHalf) });
        // 次の監視でモードが変わったと分かる。それからは消えずに出し続ける。
        for now in (20_100..30_000).step_by(100) {
            assert!(run(&mut t, at_caret(now, ImeMode::Hiragana)), "{now}");
        }
        // そのあと打ち始めたら、また消す。
        assert!(!run(&mut t, BadgeInput { key: true, ..at_caret(30_000, ImeMode::Hiragana) }));
        assert!(!run(&mut t, at_caret(30_100, ImeMode::Hiragana)));
    }

    #[test]
    fn clicking_another_field_soon_after_typing_keeps_the_badge() {
        let mut t = BadgeTracker::default();
        run(&mut t, at_caret(10_000, ImeMode::Hiragana));
        assert!(!run(&mut t, BadgeInput { key: true, ..at_caret(10_100, ImeMode::Hiragana) }));
        // 1 秒後に別の入力欄をクリックした。
        assert!(run(&mut t, BadgeInput { click: true, focus: 8, ..at_caret(11_100, ImeMode::Hiragana) }));
        for now in (11_200..20_000).step_by(100) {
            let input = BadgeInput { focus: 8, ..at_caret(now, ImeMode::Hiragana) };
            assert!(run(&mut t, input), "{now}");
        }
    }

    #[test]
    fn caret_that_appears_while_typing_is_not_a_trigger() {
        // Excel のセルで打ち始めると、そこで初めて入力位置ができる。
        let mut t = BadgeTracker::default();
        let no_caret = |now| BadgeInput { caret_known: false, ..at_caret(now, ImeMode::Hiragana) };
        run(&mut t, no_caret(10_000));
        run(&mut t, BadgeInput { key: true, ..no_caret(10_100) });
        for now in (10_200..15_000).step_by(100) {
            assert!(!run(&mut t, at_caret(now, ImeMode::Hiragana)), "{now}");
        }
    }

    #[test]
    fn one_missed_caret_read_is_not_arriving_again() {
        let mut t = BadgeTracker::default();
        let missed = |now| BadgeInput { caret_known: false, ..at_caret(now, ImeMode::Hiragana) };
        run(&mut t, at_caret(10_000, ImeMode::Hiragana));
        assert_eq!(t.trigger, Some(10_000));
        // 1 回（2 回）だけ入力位置を読み取れなかった（相手が忙しかった など）。
        run(&mut t, missed(10_100));
        run(&mut t, missed(10_200));
        // 読み取れるようになっても、入力欄に来直したことにはしない。
        run(&mut t, at_caret(10_300, ImeMode::Hiragana));
        assert_eq!(t.trigger, Some(10_000));
        // 3 回続けて分からなければ、入力位置は無くなったとみなし、次に分かったら入力欄に来たとする。
        for now in [20_000, 20_100, 20_200] {
            run(&mut t, missed(now));
        }
        run(&mut t, at_caret(20_300, ImeMode::Hiragana));
        assert_eq!(t.trigger, Some(20_300));
    }

    #[test]
    fn click_counts_if_the_caret_shows_up_shortly_after() {
        // 「打ち始めたときだけ」: 入力位置の無い所から、入力欄をクリックした。押した直後の監視では、
        // まだ入力位置が分からない。
        let step = |t: &mut BadgeTracker, input| t.step(BadgeWhen::Typing, true, input);
        let base = |now| BadgeInput { now, mode: Some(ImeMode::Hiragana), focus: 7, ..BadgeInput::default() };
        let mut t = BadgeTracker::default();
        step(&mut t, base(10_000));
        step(&mut t, BadgeInput { click: true, ..base(10_100) });
        assert_eq!(t.trigger, None);
        step(&mut t, BadgeInput { caret_known: true, ..base(10_300) });
        assert_eq!(t.trigger, Some(10_300));
        // 押してから時間がたってから入力位置が分かっても、数えない。
        let mut t = BadgeTracker::default();
        step(&mut t, BadgeInput { click: true, ..base(10_100) });
        step(&mut t, base(10_200));
        step(&mut t, BadgeInput { caret_known: true, ..base(10_100 + BADGE_CLICK_PENDING_MS as u32) });
        assert_eq!(t.trigger, None);
        // 「おまかせ」: 押したときに文字入力の形なら、すぐ数える。
        let mut t = BadgeTracker::default();
        t.step(BadgeWhen::Auto, true, BadgeInput { click: true, ibeam: true, ..base(10_100) });
        assert_eq!(t.trigger, Some(10_100));
        // ボタンを押して、そのあとで文字入力の形の上へ動かしただけなら、クリックとしては数えない。
        let mut t = BadgeTracker::default();
        t.step(BadgeWhen::Auto, false, BadgeInput { click: true, ..base(10_100) });
        t.step(BadgeWhen::Auto, false, BadgeInput { ibeam: true, ..base(10_200) });
        assert_eq!(t.trigger, None);
    }

    #[test]
    fn rects_touch_counts_thin_carets() {
        let window = (100, 100, 500, 400);
        assert!(rects_touch((200, 200, 201, 220), window));
        // 幅の無いキャレット
        assert!(rects_touch((200, 200, 200, 220), window));
        // 窓の外（上へ流れて見えなくなった）
        assert!(!rects_touch((200, 40, 201, 60), window));
        assert!(!rects_touch((600, 200, 601, 220), window));
        assert!(!rects_touch((-50, 200, -49, 220), window));
    }
}
