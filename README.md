# アタイの貼り付け

Excel 使用時に **`Ctrl+B` を `Ctrl+Shift+V`（値貼り付け）にリマップ**する、Windows 常駐アプリです。
Rust 製・単体 EXE（ポータブル）で、通常ユーザー権限で動作します。

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
- **ON/OFF 切替**: タスクトレイの右クリックメニュー「ON/OFF切替」から。OFF 中はリマップせず素通しします。
- **自動起動設定**: メニュー「自動起動 ON/OFF」で、スタートアップフォルダのショートケット（`.lnk`）を作成／削除します。
- **アイコン切替**: ON = 黒「B」／OFF = 黒「B」＋赤い禁止マーク。
- **終了**: メニュー「終了」から。フックを解除してから終了します（リソースリークなし）。
- **状態保持**: ON/OFF はメモリのみ。終了時にリセット（次回起動時は ON）。
- **ログ**: 開発時のみ実行ファイルと同じフォルダの `log.txt` に出力。本番ビルドでは無効。

## 使い方

1. `atai-paste.exe` を任意のフォルダに置いて起動します（インストール不要）。
2. タスクトレイに「B」アイコンが表示されます（起動時は ON）。
3. Excel を最前面にして `Ctrl+B` を押すと、`Ctrl+Shift+V`（値貼り付け）が送出されます。
4. 一時的に無効化したいときは、トレイアイコンを右クリック →「ON/OFF切替」。
5. ログオン時に自動起動させたいときは、右クリック →「自動起動 ON/OFF」。
6. 終了は右クリック →「終了」。

> **メモ（左クリックについて）**: 仕様では「左クリックで ON/OFF トグル」を想定していますが、
> 採用している `tray-item` クレートは左クリック専用のハンドラを公開していません。
> そのため ON/OFF 切替は右クリックメニューの「ON/OFF切替」に集約しています。

## ビルド

Windows（MSVC ツールチェーン推奨）で以下を実行します。

```powershell
# 開発ビルド（コンソール表示あり・log.txt 出力あり）
cargo build

# 本番ビルド（コンソール非表示・ログ無効・サイズ最適化）
cargo build --release
```

生成物: `target/release/atai-paste.exe`（単体で配布可能）。

### ビルドに関する補足

- アイコン（`src/icons/*.ico`）とマニフェスト（`app.manifest`）は、`build.rs` から
  [`embed-resource`](https://crates.io/crates/embed-resource) 経由で `app.rc` を
  コンパイルし、EXE に埋め込みます。MSVC 環境では Windows SDK の `rc.exe` が使われます。
- 開発中にログを有効化したい本番ビルドでは、`--features devlog` を付けます。
  （通常の本番リリースビルドではログは完全に無効です。）

```powershell
cargo build --release --features devlog
```

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
| ログ | ファイル出力（`log.txt`、開発時のみ） |

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
