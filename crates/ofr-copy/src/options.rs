//! コピーの設定。

use std::time::Duration;

use ofr_fs::ExtractOptions;

/// 宛先に同名のファイルがあったときの扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExistingFile {
    /// `名前 (2).jpg` のように番号を足して両方残す(既定)。
    #[default]
    Rename,
    /// 何もせず飛ばす。中断したコピーの続きをやるときに使う。
    Skip,
    /// 上書きする。
    Overwrite,
}

impl ExistingFile {
    /// 画面表示用の名前。
    pub fn label(self) -> &'static str {
        match self {
            ExistingFile::Rename => "番号を足す",
            ExistingFile::Skip => "飛ばす",
            ExistingFile::Overwrite => "上書きする",
        }
    }
}

/// コピーの設定。
///
/// リトライまわりの既定値は [`ExtractOptions`] と揃えてある(PLAN.md 5.5 は
/// 「各ファイルの読み込みにも 5.2 と同じリトライ戦略を適用する」)。
#[derive(Debug, Clone)]
pub struct CopyOptions {
    /// 読み込み失敗時のリトライ回数。
    pub retries: u32,
    /// リトライの待ち時間(指数バックオフの初期値)。
    pub retry_delay: Duration,
    /// 読み込み単位。
    pub chunk_size: usize,
    /// 読めなかった部分をゼロで埋めて残りを続けるか。
    ///
    /// 偽にすると、途中で読めなくなったファイルは失敗として扱う。真(既定)なら
    /// 読めた分を保存し、埋めたバイト数をレポートに記録する。
    pub zero_fill: bool,
    /// 元のタイムスタンプを宛先に反映するか。
    ///
    /// 反映できるのはファイルの更新日時だけ。フォルダの日時は Windows と
    /// macOS で設定方法が揃わないので触らない(PLAN.md 5.5 の
    /// 「可能な範囲で維持する」)。
    pub set_timestamps: bool,
    /// 宛先に同名のファイルがあったときの扱い。
    pub on_existing: ExistingFile,
    /// 空のディレクトリも作るか。偽ならファイルのある枝だけを作る。
    pub create_empty_dirs: bool,
    /// 進捗イベントの最短間隔(PLAN.md 5.7)。
    pub progress_interval: Duration,
}

impl Default for CopyOptions {
    fn default() -> Self {
        Self {
            retries: 2,
            retry_delay: Duration::from_millis(100),
            chunk_size: 1 << 20,
            zero_fill: true,
            set_timestamps: true,
            on_existing: ExistingFile::default(),
            create_empty_dirs: true,
            progress_interval: Duration::from_millis(100),
        }
    }
}

impl CopyOptions {
    /// 同じ設定の [`ExtractOptions`] を作る。
    pub fn extract_options(&self) -> ExtractOptions {
        ExtractOptions {
            retries: self.retries,
            retry_delay: self.retry_delay,
            chunk_size: self.chunk_size,
            zero_fill: self.zero_fill,
            set_timestamps: self.set_timestamps,
        }
    }
}
