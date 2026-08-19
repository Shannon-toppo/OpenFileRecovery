//! テストイメージを書き出すツール。
//!
//! 生成したイメージが本物として通用するかは、OS にマウントさせるのが一番早い。
//!
//! ```text
//! cargo run -p ofr-testfs -- testdata/out
//! hdiutil attach -imagekey diskimage-class=CRawDiskImage testdata/out/fat32_deleted.img
//! ```

use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("testdata/out"));
    std::fs::create_dir_all(&dir)?;

    for (name, scenario) in ofr_testfs::scenarios::all() {
        let path = dir.join(format!("{name}.img"));
        std::fs::write(&path, &scenario.image)?;
        println!(
            "{} ({} バイト, 期待するファイル {} 個)",
            path.display(),
            scenario.image.len(),
            scenario.files.len()
        );
    }
    Ok(())
}
