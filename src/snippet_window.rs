//! 定型文の画面。
//!
//! トレイメニューの「定型文...」（または履歴の一覧の右クリック「定型文の編集...」）から開く。
//! 左に定型文の一覧、右に選んだ定型文の名前と本文を出す。書いて「追加」で一覧に足し、
//! 一覧で選んで書き換えたら「更新」で直す。「保存」を押すと `settings.json` の `snippets` に
//! 書き込み、すぐに履歴の一覧の「定型文」タブで使えるようにする（再起動は要らない）。
//!
//! CSV での読み込み・書き出しもできる（1 列目が名前、2 列目が本文。Excel で編集できる）。
//! 名前の決め方・検証・CSV の読み書きは [`crate::snippets`] にあり、Linux でもテストできる。

use std::cell::RefCell;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{DeleteObject, HFONT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, IsWindow, MessageBoxW, PostQuitMessage, SetForegroundWindow, ShowWindow,
    IDCANCEL, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING,
    MB_YESNOCANCEL, SW_SHOW, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_DPICHANGED,
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

/// 画面の中身の大きさ（96 DPI 基準）。
const CLIENT_W: i32 = 600;
const CLIENT_H: i32 = 440;

/// 画面の構成。並び順がそのまま Tab キーで移る順になる。
const ITEMS: &[Item] = &[
    item(600, Kind::Label, "定型文（この順に一覧に並びます）", 12, 8, 250, 22),
    item(ID_LIST, Kind::ListBox, "", 12, 32, 250, 312),
    item(ID_UP, Kind::Button, "上へ", 12, 352, 64, 28),
    item(ID_DOWN, Kind::Button, "下へ", 80, 352, 64, 28),
    item(ID_DELETE, Kind::Button, "削除", 180, 352, 82, 28),
    item(601, Kind::Label, "名前（空なら本文の最初の行を一覧に出します）", 276, 8, 312, 22),
    item(ID_NAME, Kind::Edit, "", 276, 32, 312, 24),
    item(602, Kind::Label, "本文（改行もそのまま貼り付けます）", 276, 64, 312, 22),
    item(ID_TEXT, Kind::MultiEdit, "", 276, 88, 312, 200),
    item(603, Kind::Note, "{date} は今日の日付、{time} は今の時刻、{datetime} は両方に置き換えて貼り付けます。", 276, 294, 312, 44),
    item(ID_ADD, Kind::Button, "新しい定型文として追加", 276, 352, 152, 28),
    item(ID_UPDATE, Kind::Button, "選んだ定型文を更新", 436, 352, 152, 28),
    item(ID_IMPORT, Kind::Button, "CSV から読み込む...", 12, 400, 150, 28),
    item(ID_EXPORT, Kind::Button, "CSV に書き出す...", 168, 400, 150, 28),
    item(ID_SAVE, Kind::DefaultButton, "保存", 412, 400, 86, 28),
    item(ID_CANCEL, Kind::Button, "キャンセル", 502, 400, 86, 28),
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
    ui::create_controls(hwnd, ITEMS, font);
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

unsafe fn refresh_list(hwnd: HWND, select: Option<usize>) {
    let names: Vec<String> = list_snapshot().iter().map(snippets::display_name).collect();
    ui::set_list_items(hwnd, ID_LIST, &names, select);
}

/// 定型文を右側の欄に入れる（`None` なら空にする）。
unsafe fn load_editor(hwnd: HWND, snippet: Option<&Snippet>) {
    set_text(hwnd, ID_NAME, snippet.map_or("", |s| s.name.as_str()));
    // 複数行の欄は改行を \r\n で渡す。
    let text = snippet.map_or(String::new(), |s| s.text.replace('\n', "\r\n"));
    set_text(hwnd, ID_TEXT, &text);
}

/// 右側の欄を読み、検証する。誤りがあれば、直すべき欄の ID と理由を返す。
unsafe fn read_editor(hwnd: HWND) -> Result<Snippet, (i32, String)> {
    let name = get_text(hwnd, ID_NAME);
    let text = get_text(hwnd, ID_TEXT);
    snippets::make(&name, &text).map_err(|e| {
        let id = if e.contains("名前") { ID_NAME } else { ID_TEXT };
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
                (ID_ADD, BN_CLICKED) => on_add(hwnd),
                (ID_UPDATE, BN_CLICKED) => on_update(hwnd),
                (ID_DELETE, BN_CLICKED) => on_delete(hwnd),
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

/// 右側が、出している定型文（新しく書くときは空）から変えられているか。
unsafe fn editor_differs(hwnd: HWND) -> bool {
    let shown = CONTEXT.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|ctx| ctx.shown.and_then(|i| ctx.list.get(i).cloned()))
    });
    let name = get_text(hwnd, ID_NAME);
    let text = get_text(hwnd, ID_TEXT).replace("\r\n", "\n");
    match shown {
        Some(s) => s.name != name.trim() || s.text != text,
        None => !name.trim().is_empty() || !text.trim().is_empty(),
    }
}

/// 右側の内容を、新しい定型文として一覧の最後に足す。
unsafe fn on_add(hwnd: HWND) {
    let snippet = match read_editor(hwnd) {
        Ok(snippet) => snippet,
        Err((id, message)) => return ui::report_invalid(hwnd, id, &message),
    };
    if list_snapshot().len() >= snippets::MAX_SNIPPETS {
        show_message(
            hwnd,
            &format!("定型文は {} 件までです。", snippets::MAX_SNIPPETS),
            MB_ICONWARNING,
        );
        return;
    }
    let index = CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let ctx = c.as_mut()?;
        ctx.list.push(snippet);
        ctx.dirty = true;
        ctx.shown = Some(ctx.list.len() - 1);
        Some(ctx.list.len() - 1)
    });
    refresh_list(hwnd, index);
}

/// 一覧で選んでいる定型文を、右側の内容で書き換える。
unsafe fn on_update(hwnd: HWND) {
    let Some(index) = ui::list_index(hwnd, ID_LIST) else {
        show_message(
            hwnd,
            "書き換える定型文を、左の一覧で選んでください。新しく足すときは「新しい定型文として追加」を押します。",
            MB_ICONWARNING,
        );
        return;
    };
    let snippet = match read_editor(hwnd) {
        Ok(snippet) => snippet,
        Err((id, message)) => return ui::report_invalid(hwnd, id, &message),
    };
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow_mut().as_mut() {
            if let Some(slot) = ctx.list.get_mut(index) {
                *slot = snippet;
                ctx.dirty = true;
                ctx.shown = Some(index);
            }
        }
    });
    refresh_list(hwnd, Some(index));
}

/// 一覧で選んでいる定型文を消す。
unsafe fn on_delete(hwnd: HWND) {
    let Some(index) = ui::list_index(hwnd, ID_LIST) else {
        return;
    };
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

/// 一覧で選んでいる定型文を上下に動かす。
unsafe fn on_move(hwnd: HWND, delta: isize) {
    let Some(index) = ui::list_index(hwnd, ID_LIST) else {
        return;
    };
    let moved = CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let ctx = c.as_mut()?;
        let to = index.checked_add_signed(delta).filter(|to| *to < ctx.list.len())?;
        ctx.list.swap(index, to);
        ctx.dirty = true;
        ctx.shown = ctx.shown.map(|s| {
            if s == index {
                to
            } else if s == to {
                index
            } else {
                s
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
        .and_then(|bytes| {
            String::from_utf8(bytes)
                .map_err(|_| "UTF-8 の CSV ではありません（Excel では「CSV UTF-8」で保存してください）".to_string())
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
            "{} 件の定型文を読み込みます。\n\n「はい」: 今の {current} 件の後ろに足す\n\
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
                &format!("{count} 件の定型文を読み込みました。「保存」を押すと使えるようになります。"),
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
    // 右側を書き換えたまま「更新」を押し忘れていないか確かめる。
    if let Some(index) = ui::list_index(hwnd, ID_LIST) {
        if let Ok(edited) = read_editor(hwnd) {
            let current = list_snapshot().get(index).cloned();
            if current.is_some_and(|current| current != edited)
                && ui::ask_yes_no(
                    hwnd,
                    "右側で変更した内容が、まだ一覧に反映されていません。\n\
                     選んでいる定型文に反映してから保存しますか？\n\n\
                     「いいえ」を選ぶと、右側の変更は保存しません。",
                )
            {
                on_update(hwnd);
            }
        }
    } else if editor_differs(hwnd)
        && ui::ask_yes_no(
            hwnd,
            "右側に書いた定型文が、まだ一覧に追加されていません。\n\
             新しい定型文として追加してから保存しますか？\n\n\
             「いいえ」を選ぶと、右側の内容は保存しません。",
        )
    {
        on_add(hwnd);
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
