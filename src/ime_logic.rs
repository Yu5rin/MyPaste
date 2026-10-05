//! IME 入力モード表示の判断ロジック。
//!
//! Win32 API に触れない部分だけをここに集めている。値を渡して戻り値を見るだけで
//! 試せるので、Linux 上でもテストを実行して確かめられる（`tray-item` が Windows
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
        [(Theme::Dark, "dark", "濃い色"), (Theme::Light, "light", "明るい色")];

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
}
