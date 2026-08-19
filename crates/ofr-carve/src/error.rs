//! カービングのエラー型。
//!
//! 解析中のエラーは基本的に「その候補を捨てて先へ進む」で処理するので、
//! ここに出てくるのはジョブ全体を止めるもの(出力先の問題・設定不正)だけ。
//! デバイス読み込みの失敗は候補ごとにスキップされ、[`crate::CarveSummary::read_errors`]
//! に数だけ残る。

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// このクレートの共通 Result 型。
pub type Result<T> = std::result::Result<T, CarveError>;

/// カービングを続行できないエラー。
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CarveError {
    /// 出力先ディレクトリを作れない。
    #[error("出力先 {path} を作成できない: {source}")]
    CreateDir {
        /// 作ろうとしたディレクトリ。
        path: PathBuf,
        /// 元の IO エラー。
        #[source]
        source: io::Error,
    },

    /// 切り出したファイルの書き出しに失敗した。
    #[error("{path} の書き出しに失敗: {source}")]
    Write {
        /// 書き出そうとしたファイル。
        path: PathBuf,
        /// 元の IO エラー。
        #[source]
        source: io::Error,
    },

    /// 設定値が不正。
    #[error("設定が不正: {0}")]
    InvalidOptions(String),
}
