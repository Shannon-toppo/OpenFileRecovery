//! Open File Recovery の GUI 入口。
//!
//! 中身は [`ofr_gui_lib`] にある(Tauri の作法に合わせてライブラリと分けてある)。

// リリースビルドで Windows のコンソールウィンドウを出さない。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(unsafe_code)]

fn main() {
    ofr_gui_lib::run();
}
