// Windows 向けにアイコン・マニフェストなどのリソースを EXE へ埋め込む。
// Windows 以外のターゲットでは何もしない（開発環境でのチェック用）。
fn main() {
    // 判定には必ず CARGO_CFG_TARGET_OS（ビルド対象の OS）を使う。
    // ここで #[cfg(target_os = "windows")] を使うと、build.rs 自体を実行している
    // ホストの OS を見てしまうため、Linux などから Windows 向けにクロスビルドした
    // ときにリソースが埋め込まれず、アイコンもマニフェストも無い EXE ができる。
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    // リソースのコンパイルには外部ツールが要る。
    // - MSVC ABI: Windows SDK の rc.exe（Windows ホストでしか使えない）
    // - GNU  ABI: windres（mingw-w64 があれば Windows 以外でも使える）
    // Windows 以外のホストから MSVC ABI 向けに型チェックだけ行う場合
    // （cargo check --target x86_64-pc-windows-msvc）は rc.exe が無くて失敗するため、
    // ここは飛ばす。実行ファイルを作るビルドではないのでリソースは不要。
    let host_is_windows = cfg!(target_os = "windows");
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if !host_is_windows && target_env == "msvc" {
        println!(
            "cargo:warning=Windows 以外のホストでは MSVC 向けのリソースを埋め込めないため省略しました\
             （型チェック用のビルドのため実害はありません）"
        );
        return;
    }

    // app.rc に定義したアイコン（app / icon_on / icon_off）とマニフェストを
    // 実行ファイルへ埋め込む。tray-item は IconSource::Resource("icon_on") の
    // ように、ここで付けた名前で HICON を参照する。
    embed_resource::compile("app.rc", embed_resource::NONE);

    // リソース関連のファイルが変わったときだけ再実行する。
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=src/icons/app.ico");
    println!("cargo:rerun-if-changed=src/icons/icon_on.ico");
    println!("cargo:rerun-if-changed=src/icons/icon_off.ico");
}
