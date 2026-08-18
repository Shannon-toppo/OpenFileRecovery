//! テストイメージの生成(PLAN.md 9章)。
//!
//! FAT32 / exFAT の小さなイメージを Rust だけで組み立てる。OS のフォーマッタ
//! (`newfs_msdos` / `format`)を呼ぶ方式だと CI で両 OS 分の分岐が要るうえ、
//! 「削除」「クイックフォーマット」「断片化配置」を狙って作れない。ここで直接
//! 構造を書けば、テストは CI 内で完結する。
//!
//! 生成したイメージは本物のフォーマットとして妥当なので、macOS なら
//! `hdiutil attach`、Windows ならマウントして目視確認もできる
//! (`cargo run -p ofr-testfs -- testdata/out`)。
//!
//! ```
//! use ofr_testfs::{Fat32Image, FsTree};
//!
//! let mut image = Fat32Image::new(48 << 20);
//! image.tree().file("/DCIM/a.jpg", b"hello".to_vec());
//! image.tree().delete("/DCIM/a.jpg");
//! let bytes = image.build();
//! assert_eq!(&bytes[82..90], b"FAT32   ");
//! ```

#![deny(unsafe_code)]
// テスト専用のクレートなので、ドキュメントは要点だけでよい。
#![allow(missing_docs)]

mod exfat;
mod fat32;
pub mod scenarios;
mod tree;

pub use exfat::ExfatImage;
pub use fat32::Fat32Image;
pub use scenarios::{ExpectedFile, Scenario};
pub use tree::{FsTree, Node};

/// 決定的な内容のテストデータを作る。
///
/// 復元結果の突き合わせに使うので、同じ `seed` と `len` なら必ず同じ中身になる。
pub fn pattern_data(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// テストで使う日時(2026-08-19 12:34:56)。
pub const TEST_DATE: u16 = ((2026 - 1980) << 9) | (8 << 5) | 19;
/// テストで使う時刻。
pub const TEST_TIME: u16 = (12 << 11) | (34 << 5) | (56 / 2);
