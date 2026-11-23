fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let mut attributes = tauri_build::Attributes::new();
    if target_os == "windows" && target_env == "msvc" {
        // Windows のアプリのマニフェスト（共通コントロール v6 を使う指定）を、リンカに埋め込ませる。
        //
        // tauri-build は、ふつうはマニフェストをリソースに入れて、アプリの実行ファイル（bin）にだけ付ける。
        // それだと単体テストの実行ファイルにはマニフェストが入らず、Tauri のダイアログなどが使う
        // 共通コントロール v6 の関数を読み込めずに、テストが 1 件も走る前に終了する（STATUS_ENTRYPOINT_NOT_FOUND）。
        // そこで、リソースには入れず、同じ内容のマニフェストを、テストを含むすべての実行ファイルにリンカで埋め込む
        // （Tauri 自身のテストと同じやり方）。
        attributes = attributes.windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
        embed_windows_manifest();
    }
    tauri_build::try_build(attributes).expect("tauri-build の実行に失敗しました");
}

/// `windows-app-manifest.xml`（tauri-build の既定のマニフェストと同じ内容）を、実行ファイルに埋め込む。
fn embed_windows_manifest() {
    let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("windows-app-manifest.xml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    // リンカが足す実行レベルの指定（asInvoker）は入れない。これまでのマニフェストと同じ内容に保つ
    println!("cargo:rustc-link-arg=/MANIFESTUAC:NO");
}
