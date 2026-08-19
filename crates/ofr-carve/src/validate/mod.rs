//! フォーマット別バリデータ。
//!
//! シグネチャに当たった位置を受け取り、「本当にそのフォーマットか」を確かめて
//! **終端を計算する**のがバリデータの仕事(PLAN.md 5.4)。終端はヘッダ内の
//! サイズ情報か終端マーカーから求める。求まらなかったものは
//! [`Confidence::Truncated`] を付けて返し、切り出し長は呼び出し側
//! (`carver`)が「次のシグネチャまで」を上限にして決める。
//!
//! # 実装の決まり
//!
//! - どのバリデータも panic しないこと(PLAN.md 6章 5項)。長さは必ず確認し、
//!   算術は `checked_*` / `saturating_*` を使う。
//! - `limit` を越えて読まないこと。`limit` は「このファイルの終端としてありうる
//!   最大位置」で、デバイス末尾と最大ファイルサイズから決まる。
//! - 候補でないと判断したら `None`。中途半端でも「確かにこの形式だ」と言えるなら
//!   [`Confidence::Truncated`] の [`Candidate`] を返す。

use crate::format::{Confidence, FileFormat, FileMetadata};

pub(crate) mod gif;
pub(crate) mod isobmff;
pub(crate) mod jpeg;
pub(crate) mod mp3;
pub(crate) mod pdf;
pub(crate) mod png;
pub(crate) mod riff;
pub(crate) mod zip;

/// バリデータが返す 1 件の切り出し候補。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// 判定した形式。
    pub format: FileFormat,
    /// 拡張子(ドットなし)。ZIP → `docx` のように中身で変わる。
    pub extension: &'static str,
    /// 切り出す長さ。[`Confidence::Truncated`] のときは上限値。
    pub size: u64,
    /// 確実にこのファイルの一部だと言える長さ。切り詰めるときの下限になる。
    pub min_size: u64,
    /// 境界の確からしさ。
    pub confidence: Confidence,
    /// 拾えたメタデータ。
    pub metadata: FileMetadata,
}

impl Candidate {
    /// 終端を確定できた候補。
    pub(crate) fn exact(format: FileFormat, extension: &'static str, size: u64) -> Self {
        Self {
            format,
            extension,
            size,
            min_size: size,
            confidence: Confidence::Exact,
            metadata: FileMetadata::default(),
        }
    }

    /// 終端を確定できなかった候補。`size` は上限、`min_size` は確定分。
    pub(crate) fn truncated(
        format: FileFormat,
        extension: &'static str,
        size: u64,
        min_size: u64,
    ) -> Self {
        Self {
            format,
            extension,
            size: size.max(min_size),
            min_size,
            confidence: Confidence::Truncated,
            metadata: FileMetadata::default(),
        }
    }

    pub(crate) fn with_metadata(mut self, metadata: FileMetadata) -> Self {
        self.metadata = metadata;
        self
    }
}

/// ボックス名やチャンク名として通る 4 バイトか。
pub(crate) fn is_ascii_tag(tag: &[u8]) -> bool {
    tag.len() == 4
        && tag
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'_' | b'-' | 0xA9))
}
