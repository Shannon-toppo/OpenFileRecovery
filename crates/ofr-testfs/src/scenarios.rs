//! テストシナリオ(PLAN.md 9章)。
//!
//! 「作成 → ファイル配置 → 削除」「クイックフォーマット」「断片化配置」を
//! FAT32 と exFAT の両方で用意する。復元結果の突き合わせに使うので、
//! 期待するファイルの中身もセットで返す。

use crate::{ExfatImage, Fat32Image, pattern_data};

/// イメージと、そこから復元できるはずのファイル。
pub struct Scenario {
    /// イメージの中身。
    pub image: Vec<u8>,
    /// 復元できるはずのファイル。
    pub files: Vec<ExpectedFile>,
}

impl Scenario {
    /// パスで引く。
    pub fn file(&self, path: &str) -> Option<&ExpectedFile> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// 復元できるはずのファイル 1 つ。
pub struct ExpectedFile {
    /// ボリューム内でのパス。
    pub path: String,
    /// 中身。
    pub data: Vec<u8>,
    /// 削除された状態か。
    pub deleted: bool,
}

fn expected(path: &str, data: Vec<u8>, deleted: bool) -> ExpectedFile {
    ExpectedFile {
        path: path.to_string(),
        data,
        deleted,
    }
}

/// 全シナリオ。
pub fn all() -> Vec<(&'static str, Scenario)> {
    vec![
        ("fat32_deleted", fat32_deleted()),
        ("fat32_quick_format", fat32_quick_format()),
        ("fat32_fragmented", fat32_fragmented()),
        ("exfat_deleted", exfat_deleted()),
        ("exfat_quick_format", exfat_quick_format()),
        ("exfat_fragmented", exfat_fragmented()),
    ]
}

/// 中身の定義。両 FS で同じものを使う。
fn contents() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("/README.TXT", b"Open File Recovery test volume\n".to_vec()),
        ("/DCIM/100MSDCF/DSC00001.JPG", pattern_data(1, 40_000)),
        ("/DCIM/100MSDCF/DSC00002.JPG", pattern_data(2, 12_345)),
        ("/DCIM/長い名前の写真.jpg", pattern_data(3, 5_000)),
        ("/DOCS/報告書 2026.txt", pattern_data(4, 900)),
        ("/DOCS/NOTES.TXT", pattern_data(5, 100)),
    ]
}

/// FAT32: 一部のファイルを削除した状態。
pub fn fat32_deleted() -> Scenario {
    let mut image = Fat32Image::new(48 << 20);
    let files = contents();
    for (path, data) in &files {
        image.tree().file(path, data.clone());
    }
    image.tree().delete("/DCIM/100MSDCF/DSC00002.JPG");
    image.tree().delete("/DCIM/長い名前の写真.jpg");
    image.tree().delete("/DOCS/NOTES.TXT");

    let deleted = [
        "/DCIM/100MSDCF/DSC00002.JPG",
        "/DCIM/長い名前の写真.jpg",
        "/DOCS/NOTES.TXT",
    ];
    Scenario {
        image: image.build(),
        files: files
            .into_iter()
            .map(|(path, data)| expected(path, data, deleted.contains(&path)))
            .collect(),
    }
}

/// FAT32: クイックフォーマット後。ルートと FAT が消えている。
pub fn fat32_quick_format() -> Scenario {
    let mut image = Fat32Image::new(48 << 20);
    let files = contents();
    for (path, data) in &files {
        image.tree().file(path, data.clone());
    }
    Scenario {
        image: image.quick_format().build(),
        // ルート直下のファイルはエントリごと消えるので復元対象から外れる。
        files: files
            .into_iter()
            .filter(|(path, _)| path.matches('/').count() > 1)
            .map(|(path, data)| expected(path, data, true))
            .collect(),
    }
}

/// FAT32: 断片化したファイルを削除した状態(連続配置の仮定が外れるケース)。
pub fn fat32_fragmented() -> Scenario {
    let mut image = Fat32Image::new(48 << 20);
    let data = pattern_data(7, 20_000);
    image.tree().file("/FRAG.BIN", data.clone());
    image.tree().fragment("/FRAG.BIN");
    image.tree().delete("/FRAG.BIN");
    Scenario {
        image: image.build(),
        files: vec![expected("/FRAG.BIN", data, true)],
    }
}

/// exFAT: 一部のファイルを削除した状態。
pub fn exfat_deleted() -> Scenario {
    let mut image = ExfatImage::new(32 << 20);
    let files = contents();
    for (path, data) in &files {
        image.tree().file(path, data.clone());
    }
    image.tree().delete("/DCIM/100MSDCF/DSC00002.JPG");
    image.tree().delete("/DCIM/長い名前の写真.jpg");
    image.tree().delete("/DOCS/NOTES.TXT");

    let deleted = [
        "/DCIM/100MSDCF/DSC00002.JPG",
        "/DCIM/長い名前の写真.jpg",
        "/DOCS/NOTES.TXT",
    ];
    Scenario {
        image: image.build(),
        files: files
            .into_iter()
            .map(|(path, data)| expected(path, data, deleted.contains(&path)))
            .collect(),
    }
}

/// exFAT: クイックフォーマット後。
pub fn exfat_quick_format() -> Scenario {
    let mut image = ExfatImage::new(32 << 20);
    let files = contents();
    for (path, data) in &files {
        image.tree().file(path, data.clone());
    }
    Scenario {
        image: image.quick_format().build(),
        files: files
            .into_iter()
            .filter(|(path, _)| path.matches('/').count() > 1)
            .map(|(path, data)| expected(path, data, true))
            .collect(),
    }
}

/// exFAT: 断片化したファイルを削除した状態。
pub fn exfat_fragmented() -> Scenario {
    let mut image = ExfatImage::new(32 << 20);
    let data = pattern_data(7, 20_000);
    image.tree().file("/FRAG.BIN", data.clone());
    image.tree().fragment("/FRAG.BIN");
    image.tree().delete("/FRAG.BIN");
    Scenario {
        image: image.build(),
        files: vec![expected("/FRAG.BIN", data, true)],
    }
}
