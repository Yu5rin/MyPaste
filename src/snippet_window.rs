//! 定型文の画面。
//!
//! トレイメニューの「定型文の編集...」（または履歴の一覧の右クリック「定型文の編集...」）から開く。
//! 左に定型文の一覧、右に選んだ定型文のグループ・名前・本文を出す。「新規」で書き始め、書いて
//! 「追加」で一覧に足し、一覧で選んで書き換えたら「上書き」で直す。「OK」を押すと `settings.json` の
//! `snippets` に書き込み、すぐに履歴の一覧の「定型文」タブで使えるようにする（再起動は要らない）。
//!
//! グループを付けた定型文は、一覧に `[グループ] 名前` と出し、同じグループのものを続けて並べる。
//!
//! CSV での読み込み・書き出しもできる（1 列目が名前、2 列目が本文、3 列目がグループ。
//! Excel で編集できる。2 列だけの CSV も読める。読み込みは UTF-8 と Shift_JIS のどちらでもよい）。
//! 名前の決め方・並べ方・検証・CSV の読み書きは [`crate::snippets`] にあり、Linux でもテストできる。

use std::cell::RefCell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{DeleteObject, HFONT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, IsWindow, MessageBoxW, PostQuitMessage,
    SendMessageW, SetForegroundWindow, ShowWindow, CBS_AUTOHSCROLL, CBS_DROPDOWN, CB_LIMITTEXT,
    HMENU, IDCANCEL, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING,
    MB_YESNOCANCEL, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_DPICHANGED, WM_SETFONT, WS_CHILD, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::config::{self, Snippet};
use crate::ui::{
    self, get_text, item, set_text, show_message, Item, Kind, SingleWindow, BN_CLICKED,
    LBN_SELCHANGE,
};
use crate::{actions, snippets};

// --- コントロールの ID ---
const ID_SAVE: i32 = 1;
const ID_CANCEL: i32 = 2;
const ID_LIST: i32 = 501;
const ID_UP: i32 = 502;
const ID_DOWN: i32 = 503;
const ID_DELETE: i32 = 504;
const ID_NAME: i32 = 505;
const ID_TEXT: i32 = 506;
const ID_ADD: i32 = 507;
const ID_UPDATE: i32 = 508;
const ID_IMPORT: i32 = 509;
const ID_EXPORT: i32 = 510;
const ID_NEW: i32 = 511;
const ID_GROUP: i32 = 512;

/// 画面の中身の大きさ（96 DPI 基準）。
const CLIENT_W: i32 = 700;
const CLIENT_H: i32 = 460;

/// 画面の構成。並び順がそのまま Tab キーで移る順になる。
/// グループの欄（`ID_GROUP`）は、書き込めるコンボボックスにするため [`create_group_box`] で作る
/// （ここでは位置と大きさだけを使う）。
const ITEMS: &[Item] = &[
    item(600, Kind::Label, "定型文（この順に一覧に並びます）", 12, 8, 270, 22),
    item(ID_LIST, Kind::ListBox, "", 12, 32, 270, 332),
    item(ID_UP, Kind::Button, "上へ(&U)", 12, 372, 62, 28),
    item(ID_DOWN, Kind::Button, "下へ(&W)", 78, 372, 62, 28),
    item(ID_NEW, Kind::Button, "新規(&N)", 154, 372, 62, 28),
    item(ID_DELETE, Kind::Button, "削除(&D)", 220, 372, 62, 28),
    item(604, Kind::Label, "グループ（空でもよい。同じグループはまとめて並べます）", 296, 8, 392, 22),
    item(ID_GROUP, Kind::Combo, "", 296, 32, 392, 200),
    item(601, Kind::Label, "名前（空なら、本文の最初の行を一覧に出します）", 296, 64, 392, 22),
    item(ID_NAME, Kind::Edit, "", 296, 88, 392, 24),
    item(602, Kind::Label, "本文（改行もそのまま貼り付けます）", 296, 120, 392, 22),
    item(ID_TEXT, Kind::MultiEdit, "", 296, 144, 392, 170),
    item(603, Kind::Note, "{date} は今日の日付、{time} は今の時刻、{datetime} は両方に置き換えて貼り付けます。{ そのものは {{、} は }} と書きます。", 296, 320, 392, 44),
    item(ID_ADD, Kind::Button, "新しい定型文として追加(&A)", 296, 372, 192, 28),
    item(ID_UPDATE, Kind::Button, "選んだ定型文に上書き(&R)", 496, 372, 192, 28),
    item(ID_IMPORT, Kind::Button, "CSV から読み込む(&I)...", 12, 420, 160, 28),
    item(ID_EXPORT, Kind::Button, "CSV に書き出す(&E)...", 178, 420, 160, 28),
    item(ID_SAVE, Kind::DefaultButton, "OK", 518, 420, 82, 28),
    item(ID_CANCEL, Kind::Button, "キャンセル", 606, 420, 82, 28),
];

/// 画面のスレッドが持つ状態。
struct Context {
    /// 編集中の定型文（保存するまで settings.json には書かない）。
    list: Vec<Snippet>,
    font: HFONT,
    /// 保存していない変更があるか。
    dirty: bool,
    /// 開いたときの定型文（保存するときに、ほかの場所で足されていないかを確かめる）。
    loaded: Vec<Snippet>,
    /// 右側に出している定型文の位置（`None` は新しく書くとき）。
    shown: Option<usize>,
}

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

/// 開いている定型文の画面。
static WINDOW: SingleWindow = SingleWindow::new();

/// 画面が開いているか（尋ねずに更新するとき、編集中に再起動しないように確かめる）。
pub fn is_open() -> bool {
    WINDOW.is_open()
}

/// 定型文の画面を開く。すでに開いていれば手前に出す。定型文は settings.json から読み直す
/// （履歴の一覧から登録したものも出すため）。
pub fn open() {
    WINDOW.open(|| unsafe { thread_main(config::Settings::load().snippets) });
}

/// 開いていれば閉じ（保存はしない）、スレッドが終わるのを待つ。アプリの終了時に呼ぶ。
pub fn close() {
    WINDOW.close();
}

unsafe fn thread_main(list: Vec<Snippet>) {
    // 手で書き換えた設定ファイルの定型文は、改行や名前の前後の空白がそろっていないことがある。
    // そろえておかないと、何も変えていないのに「変更があります」と尋ねてしまう。
    let mut list: Vec<Snippet> = list.into_iter().map(snippets::normalize).collect();
    snippets::sort_by_group(&mut list, None);
    ui::init_common_controls();
    let title = format!("定型文 - アタイの貼り付け v{}", env!("CARGO_PKG_VERSION"));
    let Some(hwnd) = ui::create_top_window(w!("AtaiPasteSnippets"), Some(wnd_proc), &title) else {
        log::error!("定型文の画面を作成できませんでした");
        show_message(HWND::default(), "定型文の画面を開けませんでした。", MB_ICONERROR);
        return;
    };
    let dpi = GetDpiForWindow(hwnd);
    let font = ui::create_ui_font(dpi);
    let first = list.first().cloned();
    CONTEXT.with(|c| {
        *c.borrow_mut() = Some(Context {
            loaded: list.clone(),
            shown: (!list.is_empty()).then_some(0),
            list,
            font,
            dirty: false,
        })
    });
    // Tab キーの順を ITEMS の並びのままにするため、グループの欄の前後に分けて作る。
    let group_at = ITEMS.iter().position(|i| i.id == ID_GROUP).unwrap_or(ITEMS.len());
    ui::create_controls(hwnd, &ITEMS[..group_at], font);
    create_group_box(hwnd, font);
    ui::create_controls(hwnd, ITEMS.get(group_at + 1..).unwrap_or(&[]), font);
    refresh_list(hwnd, first.as_ref().map(|_| 0));
    load_editor(hwnd, first.as_ref());
    ui::place_window(hwnd, dpi, CLIENT_W, CLIENT_H);
    ui::layout(hwnd, ITEMS, dpi);
    WINDOW.set_window(hwnd);

    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    ui::focus(hwnd, if first.is_some() { ID_LIST } else { ID_TEXT });

    ui::run_message_loop(hwnd);

    CONTEXT.with(|c| {
        if let Some(context) = c.borrow_mut().take() {
            let _ = DeleteObject(context.font);
        }
    });
}

fn list_snapshot() -> Vec<Snippet> {
    CONTEXT.with(|c| c.borrow().as_ref().map(|ctx| ctx.list.clone()).unwrap_or_default())
}

/// グループの欄を、書き込めるコンボボックス（一覧から選ぶことも、新しく書くこともできる）として作る。
unsafe fn create_group_box(hwnd: HWND, font: HFONT) {
    let instance = GetModuleHandleW(None).unwrap_or_default();
    let control = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("COMBOBOX"),
        PCWSTR::null(),
        WINDOW_STYLE(
            WS_CHILD.0
                | WS_VISIBLE.0
                | WS_VSCROLL.0
                | WS_TABSTOP.0
                | CBS_DROPDOWN as u32
                | CBS_AUTOHSCROLL as u32,
        ),
        0,
        0,
        0,
        0,
        hwnd,
        HMENU(ID_GROUP as isize as *mut core::ffi::c_void),
        instance,
        None,
    );
    if let Ok(control) = control {
        if !font.is_invalid() {
            SendMessageW(control, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
        SendMessageW(control, CB_LIMITTEXT, WPARAM(snippets::MAX_GROUP_CHARS), LPARAM(0));
    }
}

/// 一覧を出し直す。グループの欄の選択肢も、今あるグループに合わせる（書いている文字は残す）。
unsafe fn refresh_list(hwnd: HWND, select: Option<usize>) {
    let list = list_snapshot();
    let names: Vec<String> = list.iter().map(snippets::display_name).collect();
    ui::set_list_items(hwnd, ID_LIST, &names, select);
    let typed = get_text(hwnd, ID_GROUP);
    let groups = snippets::groups(&list);
    ui::add_combo_items(hwnd, ID_GROUP, groups.iter().map(String::as_str));
    set_text(hwnd, ID_GROUP, &typed);
}

/// 定型文を右側の欄に入れる（`None` なら空にする）。
unsafe fn load_editor(hwnd: HWND, snippet: Option<&Snippet>) {
    set_text(hwnd, ID_GROUP, snippet.map_or("", |s| s.group.as_str()));
    set_text(hwnd, ID_NAME, snippet.map_or("", |s| s.name.as_str()));
    // 複数行の欄は改行を \r\n で渡す。
    let text = snippet.map_or(String::new(), |s| s.text.replace('\n', "\r\n"));
    set_text(hwnd, ID_TEXT, &text);
}

/// 右側の欄を読み、検証する。誤りがあれば、直すべき欄の ID と理由を返す。
unsafe fn read_editor(hwnd: HWND) -> Result<Snippet, (i32, String)> {
    let group = get_text(hwnd, ID_GROUP);
    let name = get_text(hwnd, ID_NAME);
    let text = get_text(hwnd, ID_TEXT);
    snippets::make_in_group(&group, &name, &text).map_err(|e| {
        let id = if e.contains("グループ") {
            ID_GROUP
        } else if e.contains("名前") {
            ID_NAME
        } else {
            ID_TEXT
        };
        (id, format!("{e}。"))
    })
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            match (id, code) {
                (ID_LIST, LBN_SELCHANGE) => on_select(hwnd),
                (ID_ADD, BN_CLICKED) => {
                    on_add(hwnd);
                }
                (ID_UPDATE, BN_CLICKED) => {
                    on_update(hwnd);
                }
                (ID_DELETE, BN_CLICKED) => on_delete(hwnd),
                (ID_NEW, BN_CLICKED) => on_new(hwnd),
                (ID_UP, BN_CLICKED) => on_move(hwnd, -1),
                (ID_DOWN, BN_CLICKED) => on_move(hwnd, 1),
                (ID_IMPORT, BN_CLICKED) => on_import(hwnd),
                (ID_EXPORT, BN_CLICKED) => on_export(hwnd),
                (ID_SAVE, BN_CLICKED) => on_save(hwnd),
                (ID_CANCEL, BN_CLICKED) => on_close(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            on_close(hwnd);
            LRESULT(0)
        }
        ui::WM_APP_FORCE_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let old = CONTEXT.with(|c| c.borrow().as_ref().map(|context| context.font));
            let font = ui::on_dpi_changed(hwnd, ITEMS, wparam, lparam, old.unwrap_or_default());
            CONTEXT.with(|c| {
                if let Some(context) = c.borrow_mut().as_mut() {
                    context.font = font;
                }
            });
            LRESULT(0)
        }
        WM_DESTROY => {
            WINDOW.set_window(HWND::default());
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 一覧で選んだ定型文を右側に出す。
unsafe fn on_select(hwnd: HWND) {
    let shown = CONTEXT.with(|c| c.borrow().as_ref().and_then(|ctx| ctx.shown));
    if ui::list_index(hwnd, ID_LIST) != shown
        && editor_differs(hwnd)
        && !ui::ask_yes_no(
            hwnd,
            "右側で変更した内容が、まだ一覧に反映されていません。\n\
             変更を捨てて、選んだ定型文を表示しますか？",
        )
    {
        refresh_list(hwnd, shown);
        return;
    }
    show_selected(hwnd);
}

/// 一覧で選んでいる定型文を右側に出す（確かめずに）。
unsafe fn show_selected(hwnd: HWND) {
    let index = ui::list_index(hwnd, ID_LIST);
    let snippet = CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let ctx = c.as_mut()?;
        ctx.shown = index;
        index.and_then(|i| ctx.list.get(i).cloned())
    });
    load_editor(hwnd, snippet.as_ref());
}

/// 「新規」: 一覧の選択を外し、右側を空にして、新しい定型文を書き始める。
unsafe fn on_new(hwnd: HWND) {
    if editor_differs(hwnd)
        && !ui::ask_yes_no(
            hwnd,
            "右側で変更した内容が、まだ一覧に反映されていません。\n\
             変更を捨てて、新しい定型文を書き始めますか？",
        )
    {
        return;
    }
    if !IsWindow(hwnd).as_bool() {
        return;
    }
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.shown = None;
        }
    });
    refresh_list(hwnd, None);
    load_editor(hwnd, None);
    ui::focus(hwnd, ID_NAME);
}

/// 同じ名前（一覧に出る名前。グループがあればグループも同じ）の定型文が、`except` 以外にあるか。
/// キー割り当ての「定型文を貼り付け」は名前で選ぶので、同じ名前が 2 つあると後ろのものを選べない
/// （グループが違えば `[グループ] 名前` で選べるので、知らせない）。
fn name_taken(list: &[Snippet], snippet: &Snippet, except: Option<usize>) -> bool {
    let name = snippets::display_name(snippet);
    list.iter()
        .enumerate()
        .any(|(i, s)| Some(i) != except && snippets::display_name(s) == name)
}

/// 同じ名前の定型文があることを知らせ、それでも続けるかを尋ねる。
unsafe fn confirm_same_name(hwnd: HWND, snippet: &Snippet) -> bool {
    ui::ask_yes_no(
        hwnd,
        &format!(
            "同じ名前の定型文「{}」がすでにあります。\n\n\
             キー割り当ての「定型文を貼り付け」は名前で選ぶため、同じ名前が 2 つあると、\
             上にあるものだけが貼り付けられます。このまま続けますか？",
            snippets::display_name(snippet)
        ),
    ) && IsWindow(hwnd).as_bool()
}

/// 右側が、出している定型文（新しく書くときは空）から変えられているか。
unsafe fn editor_differs(hwnd: HWND) -> bool {
    let shown = CONTEXT.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|ctx| ctx.shown.and_then(|i| ctx.list.get(i).cloned()))
    });
    let group = get_text(hwnd, ID_GROUP);
    let name = get_text(hwnd, ID_NAME);
    let text = get_text(hwnd, ID_TEXT).replace("\r\n", "\n");
    match shown {
        Some(s) => s.group != group.trim() || s.name != name.trim() || s.text != text,
        // グループだけ書いて（選んで）あっても、書き始めたとはみなさない。
        None => !name.trim().is_empty() || !text.trim().is_empty(),
    }
}

/// 右側の内容を、新しい定型文として一覧の最後に足す。
unsafe fn on_add(hwnd: HWND) -> bool {
    let snippet = match read_editor(hwnd) {
        Ok(snippet) => snippet,
        Err((id, message)) => {
            ui::report_invalid(hwnd, id, &message);
            return false;
        }
    };
    if list_snapshot().len() >= snippets::MAX_SNIPPETS {
        show_message(
            hwnd,
            &format!("定型文は {} 件までです。", snippets::MAX_SNIPPETS),
            MB_ICONWARNING,
        );
        return false;
    }
    if name_taken(&list_snapshot(), &snippet, None) && !confirm_same_name(hwnd, &snippet) {
        return false;
    }
    let index = CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let ctx = c.as_mut()?;
        ctx.list.push(snippet);
        // 同じグループの最後に入るよう並べ直す。
        let last = ctx.list.len() - 1;
        let index = snippets::sort_by_group(&mut ctx.list, Some(last));
        ctx.dirty = true;
        ctx.shown = index;
        index
    });
    refresh_list(hwnd, index);
    true
}

/// 一覧で選んでいる定型文を、右側の内容で書き換える。
unsafe fn on_update(hwnd: HWND) -> bool {
    let Some(index) = ui::list_index(hwnd, ID_LIST) else {
        show_message(
            hwnd,
            "上書きする定型文を、左の一覧で選んでください。新しく足すときは「新しい定型文として追加」を押します。",
            MB_ICONWARNING,
        );
        return false;
    };
    let snippet = match read_editor(hwnd) {
        Ok(snippet) => snippet,
        Err((id, message)) => {
            ui::report_invalid(hwnd, id, &message);
            return false;
        }
    };
    if name_taken(&list_snapshot(), &snippet, Some(index)) && !confirm_same_name(hwnd, &snippet) {
        return false;
    }
    let index = CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let ctx = c.as_mut()?;
        let slot = ctx.list.get_mut(index)?;
        *slot = snippet;
        // グループを変えたときは、そのグループの所へ動く。
        let index = snippets::sort_by_group(&mut ctx.list, Some(index));
        ctx.dirty = true;
        ctx.shown = index;
        index
    });
    refresh_list(hwnd, index);
    true
}

/// 一覧で選んでいる定型文を消す。
unsafe fn on_delete(hwnd: HWND) {
    let Some(index) = ui::list_index(hwnd, ID_LIST) else {
        return;
    };
    let name = list_snapshot().get(index).map(snippets::display_name).unwrap_or_default();
    if !ui::ask_yes_no(hwnd, &format!("定型文「{name}」を削除しますか？")) || !IsWindow(hwnd).as_bool()
    {
        return;
    }
    let next = CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let ctx = c.as_mut()?;
        if index >= ctx.list.len() {
            return None;
        }
        ctx.list.remove(index);
        ctx.dirty = true;
        (!ctx.list.is_empty()).then(|| index.min(ctx.list.len() - 1))
    });
    refresh_list(hwnd, next);
    show_selected(hwnd);
}

/// 一覧で選んでいる定型文を上下に動かす。グループの境目では、グループごと動かす
/// （[`snippets::move_item`]）。
unsafe fn on_move(hwnd: HWND, delta: isize) {
    let Some(index) = ui::list_index(hwnd, ID_LIST) else {
        return;
    };
    let moved = CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let ctx = c.as_mut()?;
        let shown = ctx.shown.and_then(|s| ctx.list.get(s).cloned().map(|snippet| (s, snippet)));
        let to = snippets::move_item(&mut ctx.list, index, delta)?;
        ctx.dirty = true;
        // 右側に出している定型文の、動かしたあとの位置（同じ中身が 2 つあっても、出す中身は同じ）。
        ctx.shown = shown.and_then(|(s, snippet)| {
            if s == index {
                Some(to)
            } else {
                ctx.list.iter().position(|x| *x == snippet)
            }
        });
        Some(to)
    });
    if let Some(to) = moved {
        refresh_list(hwnd, Some(to));
    }
}

/// CSV で選べるファイルの種類。
const CSV_FILTER: &str = "CSV ファイル (*.csv)\0*.csv\0すべてのファイル (*.*)\0*.*\0";

/// 「CSV から読み込む...」: 今の一覧の後ろに足すか、置き換える（「保存」を押すまでは使わない）。
unsafe fn on_import(hwnd: HWND) {
    let Some(path) = ui::choose_file(hwnd, false, CSV_FILTER, "csv", "") else {
        return;
    };
    let read = std::fs::metadata(&path)
        .map_err(|e| e.to_string())
        .and_then(|m| {
            if m.len() > config::HOTKEYS_FILE_MAX_BYTES * 4 {
                Err("ファイルが大きすぎます".to_string())
            } else {
                std::fs::read(&path).map_err(|e| e.to_string())
            }
        })
        // Excel の「CSV（コンマ区切り）」は Shift_JIS で保存される。UTF-8 でなければ、そちらで読む。
        .and_then(|bytes| match String::from_utf8(bytes) {
            Ok(text) => Ok(text),
            Err(e) => ui::decode_shift_jis(e.as_bytes())
                .ok_or_else(|| "文字コードを読み取れません（UTF-8 か Shift_JIS の CSV にしてください）".to_string()),
        })
        .and_then(|text| snippets::from_csv(&text));
    let imported = match read {
        Ok(list) if !list.is_empty() => list,
        Ok(_) => {
            show_message(hwnd, "この CSV には定型文がありません。", MB_ICONWARNING);
            return;
        }
        Err(e) => {
            show_message(
                hwnd,
                &format!("読み込めませんでした。\n\n{}\n{e}", path.display()),
                MB_ICONERROR,
            );
            return;
        }
    };
    let current = list_snapshot().len();
    let append = if current == 0 {
        true
    } else {
        let text = ui::wide(&format!(
            "{} 件の定型文を読み込みます。\n\n「はい」: 今の {current} 件に足す（グループのあるものは、同じグループの所に並べる）\n\
             「いいえ」: 今の定型文を消して、読み込んだものに置き換える",
            imported.len()
        ));
        match MessageBoxW(
            hwnd,
            windows::core::PCWSTR(text.as_ptr()),
            w!("アタイの貼り付け"),
            MB_YESNOCANCEL | MB_ICONQUESTION,
        ) {
            IDCANCEL => return,
            answer => answer == IDYES,
        }
    };
    let count = imported.len();
    let result = CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let ctx = c.as_mut()?;
        if !append {
            ctx.list.clear();
        }
        if ctx.list.len() + imported.len() > snippets::MAX_SNIPPETS {
            return Some(Err(()));
        }
        let first = ctx.list.len();
        ctx.list.extend(imported);
        // 読み込んだ定型文も、同じグループの所へ並べる。
        let first = snippets::sort_by_group(&mut ctx.list, Some(first)).unwrap_or(0);
        ctx.dirty = true;
        Some(Ok(first))
    });
    match result {
        Some(Ok(first)) => {
            refresh_list(hwnd, Some(first));
            show_selected(hwnd);
            log::info!("定型文を CSV から読み込みました（{count} 件）");
            show_message(
                hwnd,
                &format!("{count} 件の定型文を読み込みました。「OK」を押すと保存して、使えるようになります。"),
                MB_ICONINFORMATION,
            );
        }
        Some(Err(())) => show_message(
            hwnd,
            &format!("定型文は合わせて {} 件までです。", snippets::MAX_SNIPPETS),
            MB_ICONWARNING,
        ),
        None => {}
    }
}

/// 「CSV に書き出す...」: 一覧にある定型文（保存前のものも含む）を CSV にする。
unsafe fn on_export(hwnd: HWND) {
    let list = list_snapshot();
    if list.is_empty() {
        show_message(hwnd, "書き出す定型文がありません。", MB_ICONWARNING);
        return;
    }
    let Some(path) = ui::choose_file(hwnd, true, CSV_FILTER, "csv", "定型文.csv") else {
        return;
    };
    match std::fs::write(&path, snippets::to_csv(&list)) {
        Ok(()) => show_message(
            hwnd,
            &format!("{} 件の定型文を書き出しました。\n\n{}", list.len(), path.display()),
            MB_ICONINFORMATION,
        ),
        Err(e) => show_message(
            hwnd,
            &format!("書き出せませんでした。\n\n{}\n{e}", path.display()),
            MB_ICONERROR,
        ),
    }
}

/// 「保存」: settings.json に書き、すぐに使えるようにして閉じる。
unsafe fn on_save(hwnd: HWND) {
    // 右側を書き換えたまま「更新」「追加」を押し忘れていないか確かめる。反映しようとして
    // 入力に誤りがあれば、保存をやめて直してもらう（黙って捨てない）。
    if editor_differs(hwnd) {
        let selected = ui::list_index(hwnd, ID_LIST).is_some();
        let question = if selected {
            "右側で変更した内容が、まだ一覧に反映されていません。\n\
             選んでいる定型文に反映してから保存しますか？\n\n\
             「いいえ」を選ぶと、右側の変更は保存しません。"
        } else {
            "右側に書いた定型文が、まだ一覧に追加されていません。\n\
             新しい定型文として追加してから保存しますか？\n\n\
             「いいえ」を選ぶと、右側の内容は保存しません。"
        };
        if ui::ask_yes_no(hwnd, question) {
            if !IsWindow(hwnd).as_bool() {
                return;
            }
            let applied = if selected { on_update(hwnd) } else { on_add(hwnd) };
            if !applied {
                return;
            }
        }
    }
    // 確かめている間に、アプリの終了で画面が閉じられていたら、保存しない。
    if !IsWindow(hwnd).as_bool() {
        return;
    }
    // 開いている間に、ほかの場所（履歴の一覧の「定型文に登録」など）で足された定型文があれば、
    // 消してしまわないよう、残すかを尋ねる。
    let loaded = CONTEXT.with(|c| c.borrow().as_ref().map(|ctx| ctx.loaded.clone()).unwrap_or_default());
    let current = list_snapshot();
    let added: Vec<Snippet> = config::Settings::load()
        .snippets
        .into_iter()
        // 開いたときと同じそろえ方をしてから比べる（そろえないと、手で書いた定型文を
        // 「ほかの場所で足された」と数えてしまう）。
        .map(snippets::normalize)
        .filter(|s| !loaded.contains(s) && !current.contains(s))
        .collect();
    if !added.is_empty()
        && ui::ask_yes_no(
            hwnd,
            &format!(
                "この画面を開いたあとで、ほかの場所で定型文が {} 件足されています\n\
                 （履歴の一覧の「定型文に登録」など）。一覧の最後に残して保存しますか？\n\n\
                 「いいえ」を選ぶと、その定型文は消えます。",
                added.len()
            ),
        )
    {
        CONTEXT.with(|c| {
            if let Some(ctx) = c.borrow_mut().as_mut() {
                ctx.list.extend(added);
                ctx.list.truncate(snippets::MAX_SNIPPETS);
                snippets::sort_by_group(&mut ctx.list, None);
            }
        });
    }
    if !IsWindow(hwnd).as_bool() {
        return;
    }
    let list = list_snapshot();
    if let Err(e) = config::save_snippets(&list) {
        log::error!("定型文の保存に失敗: {e}");
        show_message(hwnd, &format!("定型文を保存できませんでした。\n\n{e}"), MB_ICONERROR);
        return;
    }
    log::info!("定型文を保存しました（{} 件）", list.len());
    actions::set_snippets(list);
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            ctx.dirty = false;
        }
    });
    let _ = DestroyWindow(hwnd);
}

/// 「キャンセル」・閉じるボタン: 保存していない変更があれば確かめてから閉じる。
unsafe fn on_close(hwnd: HWND) {
    let dirty =
        CONTEXT.with(|c| c.borrow().as_ref().is_some_and(|ctx| ctx.dirty)) || editor_differs(hwnd);
    if dirty && !ui::ask_yes_no(hwnd, "保存していない変更があります。保存せずに閉じますか？") {
        return;
    }
    let _ = DestroyWindow(hwnd);
}
