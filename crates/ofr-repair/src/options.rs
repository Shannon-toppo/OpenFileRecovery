//! 修復の設定。

use crate::format::RepairFormat;

/// メモリに載せる入力の既定上限(JPEG / PNG のみ)。
pub const DEFAULT_MAX_IN_MEMORY: u64 = 512 * 1024 * 1024;

/// 欠けた画素を埋める既定値。中間グレー(PLAN.md 5.6「残りはグレーで埋めて保存する」)。
pub const DEFAULT_FILL: u8 = 0x80;

/// 修復の設定。
#[derive(Debug, Clone)]
pub struct RepairOptions {
    /// 形式を指定する。`None`(既定)なら中身から判定する。
    pub format: Option<RepairFormat>,
    /// 修復結果を検証するか。
    ///
    /// JPEG / PNG は実際にデコードし、AVI / MP4 はコンテナ整合性を確かめる
    /// (動画の実視聴確認は人間の仕事。PLAN.md 5.6)。
    pub verify: bool,
    /// 読めなかった画素を埋める値。
    pub fill: u8,
    /// 元から壊れていなかった場合も出力を書くか。
    ///
    /// 真(既定)なら入力のコピーを出力に置く。GUI から「修復を試す」を押した
    /// 利用者が、結果として何もファイルを得られないのは分かりにくい。
    pub write_intact: bool,
    /// ヘッダ再構成で寸法が分からないときに使う幅。
    pub width: Option<u32>,
    /// ヘッダ再構成で寸法が分からないときに使う高さ。
    pub height: Option<u32>,
    /// メモリに載せる入力の上限(JPEG / PNG のみ)。
    pub max_in_memory: u64,
}

impl Default for RepairOptions {
    fn default() -> Self {
        Self {
            format: None,
            verify: true,
            fill: DEFAULT_FILL,
            write_intact: true,
            width: None,
            height: None,
            max_in_memory: DEFAULT_MAX_IN_MEMORY,
        }
    }
}

impl RepairOptions {
    /// 指定された寸法(両方揃っている場合のみ)。
    pub fn size_hint(&self) -> Option<(u32, u32)> {
        match (self.width, self.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
            _ => None,
        }
    }
}
