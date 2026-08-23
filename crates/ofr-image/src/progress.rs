//! 進捗イベントと結果サマリ。
//!
//! GUI がそのまま表示できる粒度で持つ(PLAN.md 5.2)。発火は既定 100ms 間隔に
//! 間引かれる(PLAN.md 5.7)。

use std::time::Duration;

use crate::blocks::{Block, BlockList};

/// 進捗イベントに載せる領域マップの最大区間数。
///
/// GUI の帯グラフは画面幅ぶんしか描けないので、これ以上細かくしても意味がない。
pub const MAP_SEGMENTS: usize = 256;

/// イメージングのパス。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// 大きめのブロックで読める所を全部確保する。
    Copy,
    /// 不良域の端をセクタ単位で詰める。
    Trim,
    /// 残った不良域をセクタ単位で総当たりする。
    Scrape,
    /// 不良セクタを指定回数リトライする。
    Retry,
}

impl Pass {
    /// 画面表示用の名前。
    pub fn label(self) -> &'static str {
        match self {
            Pass::Copy => "コピー",
            Pass::Trim => "トリム",
            Pass::Scrape => "スクレイプ",
            Pass::Retry => "リトライ",
        }
    }
}

impl std::fmt::Display for Pass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// 進捗イベント。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// 実行中のパス。
    pub pass: Pass,
    /// パス番号(リトライの回数など。1 始まり)。
    pub pass_number: u32,
    /// いま読んでいる位置。
    pub position: u64,
    /// デバイス全長。
    pub total: u64,
    /// 取得済みバイト数。
    pub rescued: u64,
    /// 不良と判定されたバイト数。
    pub bad: u64,
    /// まだ試していない(または再試行待ちの)バイト数。
    pub pending: u64,
    /// 読み込みエラーの回数。
    pub errors: u64,
    /// 開始からの経過時間。
    pub elapsed: Duration,
    /// 直近の平均速度(バイト/秒)。
    pub rate: u64,
    /// 推定残り時間。速度が 0 なら `None`。
    pub eta: Option<Duration>,
    /// 領域マップ(取得済み / 不良 / 未試行)。GUI が帯グラフにする。
    ///
    /// [`MAP_SEGMENTS`] 区間まで間引いてあるので、そのまま描いてよい。
    pub map: Vec<Block>,
}

/// イメージング完了時のサマリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSummary {
    /// デバイス全長。
    pub total: u64,
    /// 取得済みバイト数。
    pub rescued: u64,
    /// 不良バイト数。
    pub bad: u64,
    /// 未取得のまま残ったバイト数(不良を含む)。
    pub remaining: u64,
    /// 読み込みエラーの回数。
    pub errors: u64,
    /// 応答が返らず見捨てた読み込みの回数(PLAN.md 11.2)。
    ///
    /// [`errors`](Self::errors) にも含まれる。不良セクタと、コントローラが
    /// 固まって返ってこないメディアを取り違えないために分けて数える。
    pub timeouts: u64,
    /// デバイスハンドルを開き直した回数。
    pub reopens: u32,
    /// デバイスが応答しなくなって足踏みした回数(PLAN.md 11.3)。
    pub stalls: u32,
    /// 所要時間。
    pub elapsed: Duration,
    /// キャンセルで打ち切ったか。
    pub cancelled: bool,
}

impl ImageSummary {
    /// 全域を取得できたか。
    pub fn is_complete(&self) -> bool {
        self.remaining == 0
    }

    /// 取得率(0.0〜1.0)。
    pub fn rescued_ratio(&self) -> f64 {
        if self.total == 0 {
            return 1.0;
        }
        self.rescued as f64 / self.total as f64
    }

    pub(crate) fn from_blocks(
        blocks: &BlockList,
        errors: u64,
        timeouts: u64,
        reopens: u32,
        stalls: u32,
        elapsed: Duration,
        cancelled: bool,
    ) -> Self {
        Self {
            total: blocks.total(),
            rescued: blocks.rescued(),
            bad: blocks.bytes_with(crate::blocks::BlockStatus::BadSector),
            remaining: blocks.remaining(),
            errors,
            timeouts,
            reopens,
            stalls,
            elapsed,
            cancelled,
        }
    }
}

/// 進捗コールバック。
pub type ProgressFn = Box<dyn FnMut(&Progress) + Send>;

/// デバイスが応答しなくなり、これ以上進めなくなった状況(PLAN.md 11.3)。
///
/// 固まったコントローラは電源を入れ直す以外に復帰の手がない。ソフトからは
/// USB の電源を切れないので、**人の手を電源スイッチとして使う**。
/// この知らせを受けた側が「抜き差ししてください」と出し、戻ってくるのを待つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Stall {
    /// 何度目の足踏みか(1 始まり)。
    pub attempt: u32,
    /// 足踏みした位置。
    pub position: u64,
    /// これまでに見捨てた読み込みの回数。
    pub timeouts: u64,
}

/// 足踏みしたあとどうするか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallAction {
    /// デバイスが戻ってきたので続ける。同じ範囲をもう一度読み直す。
    Resume,
    /// 打ち切る。
    Abort,
}

/// 足踏みしたときに呼ばれるコールバック。
///
/// 戻るまで待つ処理(抜き差しの検知)は呼ばれた側の仕事。ここでブロックしてよい。
pub type StallFn = Box<dyn FnMut(&Stall) -> StallAction + Send>;
