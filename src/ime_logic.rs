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
/// 表示の一辺（96 DPI 基準のピクセル）として設定できる範囲。
pub const SIZE_MIN: u32 = 40;
pub const SIZE_MAX: u32 = 600;

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
