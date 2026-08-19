//! 切り出し結果とサマリ。

use std::time::Duration;

use crate::format::{Confidence, FileFormat, FileMetadata};

/// 切り出した 1 ファイル。
///
/// カービングでは元のファイル名を取り戻せない(PLAN.md 5.4)。名前は連番と、
/// メタデータから拾えた日時で組み立てる。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CarvedFile {
    /// 連番(1 始まり)。
    pub index: u64,
    /// デバイス上の先頭位置。
    pub offset: u64,
    /// 切り出した長さ。
    pub size: u64,
    /// 形式。
    pub format: FileFormat,
    /// 拡張子(ドットなし)。
    pub extension: &'static str,
    /// 境界の確からしさ。
    pub confidence: Confidence,
    /// 拾えたメタデータ。
    pub metadata: FileMetadata,
    /// 付けたファイル名。
    pub file_name: String,
    /// 実際に書き出せたバイト数。不良セクタがあると `size` より小さくなる。
    pub bytes_written: u64,
    /// 読めずにゼロで埋めたバイト数。
    pub bad_bytes: u64,
}

impl CarvedFile {
    /// デバイス上の終端(この位置は含まない)。
    pub fn end(&self) -> u64 {
        self.offset + self.size
    }

    /// 不良セクタなしで全部読めたか。
    pub fn is_intact(&self) -> bool {
        self.bad_bytes == 0
    }
}

/// カービング完了時のサマリ。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CarveSummary {
    /// 走査した範囲の長さ。
    pub scanned: u64,
    /// 見つけたファイル数。
    pub found: u64,
    /// 終端を確定できたファイル数。
    pub exact: u64,
    /// 切り出した合計バイト数。
    pub bytes_recovered: u64,
    /// 読み込みに失敗した回数。
    pub read_errors: u64,
    /// 読めずにゼロで埋めた合計バイト数。
    pub bad_bytes: u64,
    /// 所要時間。
    pub elapsed: Duration,
    /// キャンセルで打ち切ったか。
    pub cancelled: bool,
}

impl CarveSummary {
    /// 終端を確定できた割合(0.0〜1.0)。
    pub fn exact_ratio(&self) -> f64 {
        if self.found == 0 {
            return 1.0;
        }
        self.exact as f64 / self.found as f64
    }
}

/// カービングの結果一式。
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CarveReport {
    /// サマリ。
    pub summary: CarveSummary,
    /// 見つけたファイル。位置の昇順。
    pub files: Vec<CarvedFile>,
}

impl CarveReport {
    /// 形式ごとの件数(件数の多い順、同数なら形式名順)。
    pub fn counts_by_format(&self) -> Vec<(FileFormat, u64)> {
        let mut counts: Vec<(FileFormat, u64)> = Vec::new();
        for f in &self.files {
            match counts.iter_mut().find(|(fmt, _)| *fmt == f.format) {
                Some((_, n)) => *n += 1,
                None => counts.push((f.format, 1)),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        counts
    }
}
