# アタイの貼り付け

Excel 使用時に **`Ctrl+B` を `Ctrl+Shift+V`（値貼り付け）にリマップ**する、Windows 常駐アプリです。
Rust 製・単体 EXE（ポータブル）で、通常ユーザー権限で動作します。

> **ダウンロード**: ビルド済みの `Atai-paste.exe` は
> [Releases](https://github.com/Yu5rin/MyPaste/releases) から入手できます。

<p align="center">
  <img src="src/icons/icon_on@256.png" width="96" alt="ON アイコン">
  &nbsp;&nbsp;&nbsp;&nbsp;
  <img src="src/icons/icon_off@256.png" width="96" alt="OFF アイコン">
</p>

## 概要

| 項目 | 内容 |
| --- | --- |
| 目的 | Excel 使用時に `Ctrl+B` → `Ctrl+Shift+V` を送出する |
| 範囲 | フォアグラウンドが Excel（`EXCEL.EXE`）のときのみ有効 |
| 動作環境 | Windows 10 / 11 |
| 配布形式 | 単体 EXE（ポータブル） |
| 権限 | 通常ユーザー（権限昇格不要） |

## 機能

- **キーリマップ**: Excel が最前面かつ ON のとき、`Ctrl+B` を握りつぶして `Ctrl+Shift+V` を送出します。押しっぱなしにしても最初の 1 回だけ送出します（オートリピート抑制）。
- **Excel 判定**: フォアグラウンドウィンドウのプロセスイメージ名が `EXCEL.EXE` の場合のみ有効。それ以外では `Ctrl+B` はそのまま通します。
- **ON/OFF 切替**: タスクトレイの右クリックメニュー「Ctrl+B で値貼り付け」から。有効時は先頭に「✔」が付きます。OFF 中はリマップせず素通しします。
- **自動起動設定**: メニュー「自動起動」で、スタートアップフォルダのショートカット（`.lnk`）を作成／削除します。有効時は先頭に「✔」が付きます。
- **アイコン切替**: ON = 黒「B」／OFF = 黒「B」＋赤い禁止マーク。
- **更新の確認と適用**: メニュー「更新を確認」、および**起動のたび**に GitHub の
  最新リリースを調べ、新しい版があれば確認のうえ自動で入れ替えます。詳細は
  [自動更新](#自動更新) を参照してください。
- **終了**: メニュー「終了」から。フックを解除してから終了します（リソースリークなし）。
- **状態保持**: ON/OFF はメモリのみ。終了時にリセット（次回起動時は ON）。
- **ログ**: 開発時のみ実行ファイルと同じフォルダの `log.txt` に出力。本番ビルドでは無効。

## 使い方

1. `Atai-paste.exe` を任意のフォルダに置いて起動します（インストール不要）。
2. タスクトレイに「B」アイコンが表示されます（起動時は ON）。
3. Excel を最前面にして `Ctrl+B` を押すと、`Ctrl+Shift+V`（値貼り付け）が送出されます。
4. 一時的に無効化したいときは、トレイアイコンを右クリック →「Ctrl+B で値貼り付け」。
5. ログオン時に自動起動させたいときは、右クリック →「自動起動」。
6. 終了は右クリック →「終了」。

> **メモ（左クリックについて）**: 仕様では「左クリックで ON/OFF トグル」を想定していますが、
> 採用している `tray-item` クレートは左クリック専用のハンドラを公開していません。
> そのため ON/OFF 切替は右クリックメニューの「Ctrl+B で値貼り付け」に集約しています。

## 自動更新

新しいバージョンが公開されたら、アプリ内から更新できます。

### 通信するタイミング

このアプリが外部と通信するのは、**次の 2 つの場合だけ**です。それ以外では一切通信しません。

| きっかけ | 内容 |
| --- | --- |
| メニュー「更新を確認」を押したとき | 最新リリースを問い合わせる |
| 起動時（**毎回**） | 起動のたびに最新リリースを問い合わせる |

通信は必ず別スレッドで行うため、起動やキー操作を妨げません。失敗しても
「確認できなかった」で済ませ、アプリの動作には影響しません。

### 通信先

| 用途 | URL |
| --- | --- |
| 更新の確認 | `https://api.github.com/repos/Yu5rin/MyPaste/releases/latest` |
| 更新の取得 | 上記の応答に含まれる `https://github.com/...` のダウンロード URL |

**通信先は `settings.json` に書かれており、利用者が確認・変更できます。**
起動時に自動生成されるので、内容を見れば「どこへ通信するのか」が分かります。

```jsonc
{
  "update": {
    "check_on_startup": true,   // false にすると起動時の確認をやめ、手動のみになる
    "check_interval_hours": 0,  // 0 は起動のたびに確認。24 にすると 1 日 1 回までになる
    "api_url": "https://api.github.com/repos/Yu5rin/MyPaste/releases/latest",
    "releases_page": "https://github.com/Yu5rin/MyPaste/releases/latest",
    "asset_name": "Atai-paste.exe"
  }
}
```

`settings.json` と `update_state.json`（前回の確認時刻）は実行ファイルと同じフォルダに
置かれます。`Program Files` 配下など書き込めない場所にある場合は
`%LOCALAPPDATA%\Atai-paste` に切り替わります。

### 更新の流れ

1. 最新リリースの `tag_name` を取得し、現在のバージョンと**数値として**比較します
   （文字列比較では `1.0.10` < `1.0.9` と誤判定するため）。
2. 新しい版があれば、確認のダイアログを表示します。
3. 「はい」を選ぶと実行ファイルをダウンロードし、**SHA256 を検証**します
   （GitHub がリリース資産に付与する `digest` と突き合わせます）。
   進捗はトレイアイコンのツールチップに表示されます。
4. 検証を通ったら実行ファイルを入れ替え、新しい版を起動して自分は終了します。

ダウンロードした一時ファイルは、**入れ替えの成否にかかわらず削除**します。
書き出しが途中で失敗した場合も中途半端なファイルを残しません。強制終了などで
万一残った場合に備えて、起動時に一時フォルダの古い残骸（1 時間以上前のもの）も
掃除します。削除対象は `Atai-paste-<バージョン>.exe` という名前のものだけで、
本体の実行ファイルや無関係なファイルには触れません。

### 実行中の EXE を入れ替える仕組み

実行中の EXE は Windows がロックしているため上書きできませんが、**リネームはできます**。
そこで次の順で入れ替えます。

1. `Atai-paste.exe` → `Atai-paste.exe.old` にリネーム
2. 新しい `Atai-paste.exe` を配置
3. 新しい EXE を起動して、自分は終了
4. 次回起動時に `.old` を削除

2 で失敗した場合は 1 を巻き戻すため、**アプリが壊れた状態では残りません**。
また `Program Files` 配下など書き込めない場所では自動更新を行わず、
リリースページを開いて手動での入れ替えを案内します。

## ビルド

Windows（MSVC ツールチェーン推奨）で以下を実行します。

```powershell
# 開発ビルド（コンソール表示あり・log.txt 出力あり）
cargo build

# 本番ビルド（コンソール非表示・ログ無効・サイズ最適化）
cargo build --release
```

生成物: `target/release/Atai-paste.exe`（単体で配布可能）。

### ビルドに関する補足

- アイコン（`src/icons/*.ico`）とマニフェスト（`app.manifest`）は、`build.rs` から
  [`embed-resource`](https://crates.io/crates/embed-resource) 経由で `app.rc` を
  コンパイルし、EXE に埋め込みます。MSVC 環境では Windows SDK の `rc.exe` が使われます。
- 開発中にログを有効化したい本番ビルドでは、`--features devlog` を付けます。
  （通常の本番リリースビルドではログは完全に無効です。）

```powershell
cargo build --release --features devlog
```

### GitHub Actions（手動実行のみ）

`.github/workflows/` の CI / Release ワークフローは自動実行せず、**Actions タブから手動実行**する設定（`workflow_dispatch`）です。

- **CI**: 手動実行すると `clippy` + リリースビルドを行い、`Atai-paste.exe` を Artifact として保存します。
- **Release**: タグ（例 `v1.0.0`）を入力して手動実行すると、EXE をビルドして `RELEASE_NOTES.md` を本文に GitHub Release を作成・添付します。

> ローカルに Windows 環境がある場合は、これらを使わず `cargo build --release` で直接ビルドできます。

## アイコン素材

`src/icons/` に配置しています。ソース PNG から `.ico` を生成し直したい場合は
`tools/gen_icons.py`（Pillow 必須）を使います。

```bash
python3 tools/gen_icons.py
```

| ファイル | 用途 |
| --- | --- |
| `icon_on.ico` / `icon_on.png` | ON アイコン（黒「B」） |
| `icon_off.ico` / `icon_off.png` | OFF アイコン（黒「B」＋赤禁止マーク） |
| `app.ico` | 実行ファイル自身のアイコン |
| `*@256.png` | README / プレビュー用の大きめ PNG |

## モジュール構成

```
src/
 ├ main.rs          // エントリーポイント・スレッド構成・メインループ
 ├ keyboard.rs      // 低レベルキーボードフック（WH_KEYBOARD_LL）
 ├ sendinput.rs     // Ctrl+Shift+V 送出（SendInput）
 ├ excel_check.rs   // Excel 判定（フォアグラウンドプロセス名）
 ├ tray.rs          // タスクトレイ制御（tray-item）
 ├ startup.rs       // 自動起動設定（スタートアップの .lnk 作成／削除）
 ├ update.rs        // 更新の確認・ダウンロード・適用
 ├ http.rs          // HTTPS 通信（WinHTTP）
 ├ config.rs        // 設定（settings.json）と確認時刻の記録
 ├ log.rs           // 開発時ログ出力
 └ icons/           // ON/OFF アイコン素材
```

## 技術仕様

| 項目 | 内容 |
| --- | --- |
| 言語 | Rust（edition 2021） |
| フック | `WH_KEYBOARD_LL`（低レベルキーボードフック） |
| キー送出 | `SendInput` API |
| Excel 判定 | `GetForegroundWindow` + `GetWindowThreadProcessId` + `OpenProcess` + `QueryFullProcessImageNameW` |
| タスクトレイ | `tray-item` クレート |
| 自動起動 | スタートアップフォルダの `.lnk`（`IShellLinkW` / `IPersistFile`） |
| HTTPS 通信 | Windows 標準の **WinHTTP**（証明書の検証は OS に任せる） |
| 更新の検証 | SHA256（`sha2` クレート） |
| ログ | ファイル出力（`log.txt`、開発時のみ） |

通信に WinHTTP を使っているのは、証明書ストアを自前で抱えずに済み、
C やアセンブリのビルドを伴う TLS クレート（rustls / OpenSSL 系）を避けられるためです。
システムのプロキシ設定にも自動で従います。

### スレッド構成

- **フックスレッド**: `WH_KEYBOARD_LL` を設置し、メッセージループを回します。
  低レベルフックのコールバック配信にはメッセージループが必須です。
- **メインスレッド**: タスクトレイ（`tray-item`）を保持し、メニュー操作をチャネル経由で
  受け取ってアイコン切替・自動起動切替・終了処理を行います。`tray-item` は内部で
  独自のメッセージループを持つため、メインスレッドはチャネル受信に専念できます。
- 終了時はフックスレッドへ `WM_QUIT` を送ってループを抜け、フックを確実に解除します。

### 自分が送出したキーの除外

リマップで送る入力には `dwExtraInfo` に固有の署名を付け、フック側で
（`LLKHF_INJECTED` と併せて）自己入力を確実に無視することで、無限ループを防ぎます。

## 安全性・運用

- 常駐メモリは数 MB 程度。
- 権限昇格は不要（`asInvoker` マニフェスト）。
- 終了時にフックを解除し、リソースリークを起こしません。
- ログは開発時のみ出力。本番ビルドでは無効化されます。
- 他ソフトとのキー競合は仕様外です。

## ライセンス

MIT
