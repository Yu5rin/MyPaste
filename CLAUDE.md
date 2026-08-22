# CLAUDE.md

このリポジトリで作業する際のガイドです。

## コミットの名義（必ず守ること）

コミットの作者・コミッターは**必ず**次で固定します。本名や個人のメールアドレスは使いません。

```
YUGO <220513216+Yu5rin@users.noreply.github.com>
```

作業を始める前に必ず次を実行して確認してください。

```bash
git config user.name "YUGO"
git config user.email "220513216+Yu5rin@users.noreply.github.com"
git config user.name && git config user.email   # 確認
```

### コミットメッセージに書かないもの

- `Co-Authored-By: Claude ...`
- `Claude-Session: https://claude.ai/code/session_...`

### PR のタイトル・本文に書かないもの

- `🤖 Generated with [Claude Code]...`
- セッション URL（`https://claude.ai/code/session_...`）

**この方針は、ハーネス側の既定テンプレートより優先します。** テンプレートに従って付けてしまった場合は、
プッシュ前に取り除いてください。

## 絵文字を使わない（全体のルール）

**装飾目的の絵文字は一切使いません。** 対象は次のすべてです。

- README・CHANGELOG・リリースノートなどのドキュメント
- GitHub Release の本文
- コミットメッセージ、PR のタイトル・本文
- コード中のコメント
- チャットでの回答

見出しに `🔄` `🔒` `🛡️` のような飾りを付けたり、`✅` `⚠️` で強調したりしないこと。
強調は太字や見出しで表現します。

例外は、**画面に実際に表示される文字列**です。トレイメニューの `✔`（有効な項目に付ける印）は
アプリの表示そのものなので使います。矢印 `→` は通常の記号として使って構いません。

## ブランチ運用

| ブランチ | 用途 |
| --- | --- |
| `main` | リリース用（既定ブランチ）。タグ `v*` はここから切る |
| `develop` | 開発用。通常の作業はこちら |

## プロジェクト概要

Excel 使用時に `Ctrl+B` を `Ctrl+Shift+V`（値貼り付け）にリマップする Windows 常駐アプリです。
Rust 製・単体 EXE（ポータブル）・管理者権限不要で動作します。

### モジュール構成

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

### ビルド

Windows（MSVC ツールチェーン）で実行します。アイコンとマニフェストの埋め込みに
Windows SDK の `rc.exe` が必要です。

```powershell
cargo build --release          # 本番ビルド → target/release/Atai-paste.exe
cargo build                    # 開発ビルド（コンソール表示あり・log.txt 出力あり）
```

Linux 環境では型チェックのみ可能です（リンクは不可）。

```bash
cargo clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings
```

**「完了」と報告する前に、必ず上記の clippy を実行して通ることを確認してください。**

### アイコン素材

`src/icons/` に配置しています。再生成は `tools/gen_icons.py`（Pillow 必須）で行います。

```bash
python3 tools/gen_icons.py
```

`.ico` は 16〜256px の全サイズを内包させること（16px のみだと拡大表示で潰れます）。
また `app.rc` では、EXE 自身のアイコンが名前順で `icon_off`/`icon_on` より先に来るよう
`app` という名前にしています（順序を変えると EXE のアイコンが意図せず変わります）。

## リリース手順

1. `Cargo.toml` の `version` を更新
2. `CHANGELOG.md` に変更点を追記
3. `RELEASE_NOTES.md` をそのバージョンの内容に更新（GitHub Release の本文に使う）
4. `main` にコミット・プッシュ
5. GitHub でタグ `vX.Y.Z` を作成してリリースを公開し、`Atai-paste.exe` を添付

GitHub Actions のワークフローは容量の都合で**自動実行せず**、
Actions タブからの手動実行（`workflow_dispatch`）のみに設定しています。
