//! コピーの進捗イベント。
//!
//! 発火は既定 100ms 間隔に間引く(PLAN.md 5.7)。1 ファイル終わるごとの通知は
//! 間引かず、終わった順にそのまま流す(GUI が結果一覧を育てるため)。

use std::time::Duration;

use crate::report::FileResult;

/// コピーの進捗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyProgress {
    /// いま処理している項目のパス(復旧元でのパス)。
    pub current: String,
    /// 終わったファイル数。
    pub files_done: u64,
    /// コピー対象のファイル数。
    pub files_total: u64,
    /// 書き出したバイト数。
    pub bytes_done: u64,
    /// コピー対象の合計バイト数。
    pub bytes_total: u64,
    /// 読めずに埋めたバイト数。
    pub bytes_missing: u64,
    /// 失敗したファイル数。
    pub failed: u64,
    /// 開始からの経過時間。
    pub elapsed: Duration,
    /// 書き出し速度(バイト/秒)。
    pub rate: u64,
    /// 推定残り時間。速度が 0 なら `None`。
    pub eta: Option<Duration>,
}

impl CopyProgress {
    /// 進み具合(0.0〜1.0)。バイト数を基準にする。
    pub fn ratio(&self) -> f64 {
        if self.bytes_total == 0 {
            // 中身が空のファイルばかりのときは件数で見る。
            if self.files_total == 0 {
                return 1.0;
            }
            return (self.files_done as f64 / self.files_total as f64).clamp(0.0, 1.0);
        }
        (self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0)
    }
}

/// 進捗コールバック。
pub type ProgressFn = Box<dyn FnMut(&CopyProgress) + Send>;

/// 1 ファイル完了コールバック。
pub type FileDoneFn = Box<dyn FnMut(&FileResult) + Send>;
