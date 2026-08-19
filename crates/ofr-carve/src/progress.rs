//! カービングの進捗イベント。
//!
//! 発火は既定 100ms 間隔に間引く(PLAN.md 5.7)。見つけたファイルの通知は
//! 間引かず、見つかった順にそのまま流す(GUI が結果ツリーを育てるため)。

use std::time::Duration;

use crate::result::CarvedFile;

/// 走査の進捗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarveProgress {
    /// いま走査している位置。
    pub position: u64,
    /// 走査範囲の先頭。
    pub start: u64,
    /// 走査範囲の終端。
    pub end: u64,
    /// ここまでに見つけたファイル数。
    pub found: u64,
    /// ここまでに切り出したバイト数。
    pub bytes_recovered: u64,
    /// 読み込みエラーの回数。
    pub read_errors: u64,
    /// 開始からの経過時間。
    pub elapsed: Duration,
    /// 走査速度(バイト/秒)。
    pub rate: u64,
    /// 推定残り時間。速度が 0 なら `None`。
    pub eta: Option<Duration>,
}

impl CarveProgress {
    /// 走査の進み具合(0.0〜1.0)。
    pub fn ratio(&self) -> f64 {
        let total = self.end.saturating_sub(self.start);
        if total == 0 {
            return 1.0;
        }
        (self.position.saturating_sub(self.start) as f64 / total as f64).clamp(0.0, 1.0)
    }
}

/// 進捗コールバック。
pub type ProgressFn = Box<dyn FnMut(&CarveProgress) + Send>;

/// ファイル発見コールバック。
pub type FoundFn = Box<dyn FnMut(&CarvedFile) + Send>;
