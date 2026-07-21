// Windows 向けにアイコン・マニフェストなどのリソースを EXE へ埋め込む。
// Windows 以外のターゲットでは何もしない（開発環境でのチェック用）。
fn main() {
    #[cfg(target_os = "windows")]
    {
        // app.rc に定義したアイコン（app / icon_on / icon_off）とマニフェストを
        // 実行ファイルへ埋め込む。tray-item は IconSource::Resource("icon_on") の
        // ように、ここで付けた名前で HICON を参照する。
        embed_resource::compile("app.rc", embed_resource::NONE);
    }
}
