//! Tauri のビルドスクリプト。
//!
//! Windows では実行ファイルの manifest に `requireAdministrator` を埋める。
//! 生デバイスの読み込みには管理者権限が要るので、起動時に UAC を出して
//! 昇格させる(PLAN.md 5.1)。macOS の昇格は実行時に行う(ofr-core の elevate)。

fn main() {
    let attributes = tauri_build::Attributes::new();

    #[cfg(windows)]
    let attributes = attributes.windows_attributes(
        tauri_build::WindowsAttributes::new()
            .app_manifest(include_str!("windows-app-manifest.xml")),
    );

    tauri_build::try_build(attributes).expect("Tauri のビルド設定に失敗した");
}
