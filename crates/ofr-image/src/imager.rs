//! ddrescue 方式の多段パス・イメージングエンジン(PLAN.md 5.2)。
//!
//! GNU ddrescue のコードは GPL なので参照していない。公開されているマニュアルが
//! 説明しているアルゴリズム(コピー → トリム → スクレイプ → リトライ)を自前で実装したもの。
//!
//! 設計の要は「1 セクタに固執して全体を止めない」こと。読めない所は不良として
//! マップに記録し、まず読める領域を全部確保してから、だんだん粒度を細かくして
//! 不良域に踏み込んでいく。
//!
//! フラッシュメモリの不良は消去ブロック単位(数百KiB〜数MiB)でまとまって出るので、
//! 死んだ領域をセクタ単位で叩き続けると、取りこぼしの原因が「読めない」ことより
//! 「時間切れ」になる。そこで、
//!
//! - コピーパスは失敗したら**その先を読み飛ばして**先へ進み、飛ばした範囲は
//!   同じパスの次の掃きで拾い直す(PLAN.md 11.2c)
//! - スクレイプ/リトライは、失敗が続くほど**探索単位を広げる**(PLAN.md 11.2b)
//!
//! ようにしてある。粘るのは全体を確保したあと。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ofr_device::{Device, DeviceError};
use tracing::{debug, info, warn};

use crate::blocks::{Block, BlockStatus};
use crate::error::{ImageError, Result};
use crate::mapfile::{CurrentStatus, MapFile};
use crate::progress::{ImageSummary, Pass, Progress, ProgressFn};
use crate::writer::ImageWriter;

/// ハンドルの開き直しを試みるまでの連続エラー回数。
const REOPEN_AFTER_ERRORS: u32 = 4;

/// イメージングの設定。
#[derive(Debug, Clone)]
pub struct ImageOptions {
    /// コピーパスの読み込み単位。既定 1MiB(PLAN.md 5.7)。
    pub chunk_size: u64,
    /// コピーパスで失敗したときに読み飛ばす量の下限(PLAN.md 11.2c)。
    ///
    /// 読み飛ばした範囲は未試行のまま残り、同じパスの次の掃きで読み直す。
    pub skip_size_min: u64,
    /// 読み飛ばす量の上限。失敗が続く間は下限から倍々でここまで広げる。
    ///
    /// 0 なら読み飛ばさない(失敗しても次の読み込み単位へ順に進む)。
    pub skip_size_max: u64,
    /// スクレイプ/リトライの探索単位の上限(PLAN.md 11.2b)。
    ///
    /// これ以下の広さの不良域はセクタ単位で総当たりする。これより広い死に領域では、
    /// 失敗が続く間だけ探索単位を倍々でここまで広げる。広げた単位で失敗した範囲は
    /// まとめて不良として記録するので、その中に読める部分が混ざっていると
    /// 取りこぼす。セクタサイズと同じ値にすると、全域が総当たりになる。
    pub probe_size_max: u64,
    /// トリム/スクレイプの粒度。`None` ならデバイスのブロックサイズ。
    pub sector_size: Option<u32>,
    /// リトライパスの試行回数。既定 3(PLAN.md 5.2)。
    pub retries: u32,
    /// リトライの初回待ち時間。以降は指数バックオフ。
    pub retry_delay: Duration,
    /// バックオフの上限。
    pub max_retry_delay: Duration,
    /// エラーが続いたときにデバイスハンドルを開き直すか。
    pub reopen_on_error: bool,
    /// トリムパスを実行するか。
    pub trim: bool,
    /// スクレイプパスを実行するか。
    pub scrape: bool,
    /// リトライパスを実行するか。
    pub retry: bool,
    /// 進捗イベントの最短間隔。既定 100ms(PLAN.md 5.7)。
    pub progress_interval: Duration,
    /// mapfile の自動保存間隔。
    pub map_save_interval: Duration,
}

impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            chunk_size: 1 << 20,
            skip_size_min: 64 << 10,
            skip_size_max: 16 << 20,
            probe_size_max: 64 << 10,
            sector_size: None,
            retries: 3,
            retry_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_secs(2),
            reopen_on_error: true,
            trim: true,
            scrape: true,
            retry: true,
            progress_interval: Duration::from_millis(100),
            map_save_interval: Duration::from_secs(10),
        }
    }
}

/// イメージングジョブ。
pub struct Imager<'a> {
    device: &'a dyn Device,
    options: ImageOptions,
    cancel: Arc<AtomicBool>,
    progress: Option<ProgressFn>,
}

impl<'a> Imager<'a> {
    /// 既定の設定でジョブを作る。
    pub fn new(device: &'a dyn Device) -> Self {
        Self {
            device,
            options: ImageOptions::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: None,
        }
    }

    /// 設定を差し替える。
    pub fn with_options(mut self, options: ImageOptions) -> Self {
        self.options = options;
        self
    }

    /// キャンセルフラグを共有する。真になった時点で安全な区切りで中断する。
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }

    /// 進捗コールバックを登録する。呼び出し間隔は
    /// [`ImageOptions::progress_interval`] に間引かれる。
    pub fn with_progress(mut self, f: impl FnMut(&Progress) + Send + 'static) -> Self {
        self.progress = Some(Box::new(f));
        self
    }

    /// イメージングを実行する。
    ///
    /// `map_path` を指定し、そこに既存の mapfile があれば再開になる
    /// (取得済み領域は読み直さない)。
    pub fn run(&mut self, image_path: &Path, map_path: Option<&Path>) -> Result<ImageSummary> {
        let device_len = self.device.len();
        if device_len == 0 {
            return Err(ImageError::InvalidOptions(
                "デバイスサイズが 0 のためイメージングできない".to_string(),
            ));
        }
        let sector = u64::from(
            self.options
                .sector_size
                .unwrap_or_else(|| self.device.block_size())
                .max(1),
        );
        if self.options.chunk_size < sector {
            return Err(ImageError::InvalidOptions(format!(
                "読み込み単位 {} がセクタサイズ {sector} より小さい",
                self.options.chunk_size
            )));
        }

        // 既存の mapfile があれば再開する。
        let existing = map_path.filter(|p| p.exists());
        let map = match existing {
            Some(p) => {
                let map = MapFile::load(p)?;
                if map.blocks.total() != device_len {
                    return Err(ImageError::MapMismatch {
                        map_total: map.blocks.total(),
                        device_len,
                    });
                }
                info!(
                    rescued = map.blocks.rescued(),
                    total = device_len,
                    "mapfile から再開する"
                );
                map
            }
            None => MapFile::new(device_len),
        };
        let writer = ImageWriter::create(image_path, device_len, existing.is_some())?;

        let now = Instant::now();
        let mut run = Run {
            device: self.device,
            opts: &self.options,
            cancel: &self.cancel,
            progress: self.progress.as_mut(),
            writer,
            map,
            map_path: map_path.map(Path::to_path_buf),
            sector,
            buf: Vec::new(),
            started: now,
            last_progress: now,
            last_save: now,
            rescued_this_run: 0,
            errors: 0,
            timeouts: 0,
            reopens: 0,
            pass: Pass::Copy,
            pass_number: 1,
            position: 0,
            cancelled: false,
        };
        run.run()
    }
}

/// 1 回の実行の状態。
struct Run<'r> {
    device: &'r dyn Device,
    opts: &'r ImageOptions,
    cancel: &'r AtomicBool,
    progress: Option<&'r mut ProgressFn>,
    writer: ImageWriter,
    map: MapFile,
    map_path: Option<PathBuf>,
    sector: u64,
    buf: Vec<u8>,
    started: Instant,
    last_progress: Instant,
    last_save: Instant,
    rescued_this_run: u64,
    errors: u64,
    timeouts: u64,
    reopens: u32,
    pass: Pass,
    pass_number: u32,
    position: u64,
    cancelled: bool,
}

impl Run<'_> {
    fn run(&mut self) -> Result<ImageSummary> {
        let outcome = self.run_passes();
        // 成否によらず mapfile を書き出す。中断しても再開できるようにするため。
        let finalized = self.finalize();
        outcome?;
        finalized?;
        Ok(self.summary())
    }

    fn run_passes(&mut self) -> Result<()> {
        self.copy_pass()?;
        if self.opts.trim && !self.cancelled {
            self.trim_pass()?;
        }
        if self.opts.scrape && !self.cancelled {
            self.scrape_pass()?;
        }
        if self.opts.retry && !self.cancelled {
            self.retry_pass()?;
        }
        Ok(())
    }

    /// パス 1: 大きめのブロックで、読める領域を先に全部確保する。
    ///
    /// 失敗した範囲は「未トリム」にし、**さらにその先を読み飛ばして**進む
    /// (PLAN.md 11.2c)。読み飛ばした範囲は未試行のまま残すので、次の掃きで
    /// 読み直す。フラッシュの不良はまとまって出るため、死んだ領域を
    /// 読み込み単位ごとに叩き続けるより、飛び越したほうが読める領域を早く確保できる。
    ///
    /// 未試行が残らなくなるか、掃いても減らなくなるまで繰り返す。1 回の掃きで
    /// 必ず 1 回分以上は状態が確定する(読めれば取得済み、読めなければ未トリム)ので、
    /// 繰り返しは必ず止まる。
    fn copy_pass(&mut self) -> Result<()> {
        let mut sweep = 1;
        loop {
            let before = self.map.blocks.bytes_with(BlockStatus::NonTried);
            if before == 0 {
                return Ok(());
            }
            self.begin_pass(Pass::Copy, CurrentStatus::Copying, sweep);
            self.copy_sweep()?;
            if self.cancelled {
                return Ok(());
            }
            let after = self.map.blocks.bytes_with(BlockStatus::NonTried);
            if after == 0 || after >= before {
                return Ok(());
            }
            sweep += 1;
        }
    }

    /// 未試行の領域を頭から 1 回舐める。
    fn copy_sweep(&mut self) -> Result<()> {
        let chunk = self.align_chunk(self.opts.chunk_size);
        let mut skip = 0u64;
        let mut error_streak = 0u32;

        for block in self.map.blocks.ranges_with(BlockStatus::NonTried) {
            let mut pos = block.pos;
            while pos < block.end() {
                if self.check_cancel() {
                    return Ok(());
                }
                let len = chunk.min(block.end() - pos);
                match self.rescue(pos, len)? {
                    Some(n) => {
                        pos += n;
                        skip = 0;
                        error_streak = 0;
                    }
                    None => {
                        // 読めなかった範囲は「未トリム」にして端を詰める仕事に回す。
                        self.map.blocks.mark(pos, len, BlockStatus::NonTrimmed);
                        pos += len;
                        // その先も道連れで死んでいる公算が大きいので読み飛ばす。
                        // 飛ばした範囲は未試行のまま残るので、次の掃きで読み直す。
                        skip = self.next_skip(skip);
                        pos += skip;
                        error_streak += 1;
                        // エラーが続くならデバイスの応答が悪い。ハンドルを開き直す。
                        if error_streak >= REOPEN_AFTER_ERRORS {
                            self.try_reopen();
                            error_streak = 0;
                        }
                    }
                }
                self.advance_to(pos.min(block.end()));
            }
        }
        Ok(())
    }

    /// 次に読み飛ばす量。失敗が続く間は倍々に広げる。
    fn next_skip(&self, current: u64) -> u64 {
        if self.opts.skip_size_max == 0 {
            return 0;
        }
        let min = self.align_chunk(self.opts.skip_size_min.max(1));
        let max = self.align_chunk(self.opts.skip_size_max).max(min);
        if current == 0 {
            min
        } else {
            current.saturating_mul(2).min(max)
        }
    }

    /// パス 2: 不良域の端をセクタ単位で詰めて、本当に読めない範囲を狭める。
    fn trim_pass(&mut self) -> Result<()> {
        let targets = self.map.blocks.ranges_with(BlockStatus::NonTrimmed);
        if targets.is_empty() {
            return Ok(());
        }
        self.begin_pass(Pass::Trim, CurrentStatus::Trimming, 1);

        for block in targets {
            if self.check_cancel() {
                return Ok(());
            }
            // 前から
            let mut start = block.pos;
            while start < block.end() {
                let len = self.sector.min(block.end() - start);
                match self.rescue(start, len)? {
                    Some(n) => start += n,
                    None => break,
                }
                self.advance_to(start);
            }
            // 後ろから
            let mut end = block.end();
            while end > start {
                let len = self.sector.min(end - start);
                let pos = end - len;
                match self.rescue(pos, len)? {
                    Some(_) => end = pos,
                    None => break,
                }
                self.advance_to(pos);
            }
            if start < end {
                self.map
                    .blocks
                    .mark(start, end - start, BlockStatus::NonScraped);
            }
        }
        Ok(())
    }

    /// パス 3: 残った不良域を、失敗が続くほど粗い単位で舐める。
    fn scrape_pass(&mut self) -> Result<()> {
        let targets = self.map.blocks.ranges_with(BlockStatus::NonScraped);
        if targets.is_empty() {
            return Ok(());
        }
        self.begin_pass(Pass::Scrape, CurrentStatus::Scraping, 1);

        for block in targets {
            self.probe_sweep(block)?;
            if self.cancelled {
                return Ok(());
            }
        }
        Ok(())
    }

    /// パス 4: 不良セクタを指数バックオフを挟んでリトライする。
    ///
    /// 探索の仕方はスクレイプと同じ([`Run::probe_sweep`])。狭い不良域は
    /// セクタ単位で当たり直すので、時間をおいて読めるようになったセクタは
    /// ここで拾える。
    fn retry_pass(&mut self) -> Result<()> {
        for attempt in 1..=self.opts.retries {
            let bad = self.map.blocks.ranges_with(BlockStatus::BadSector);
            if bad.is_empty() {
                break;
            }
            self.begin_pass(Pass::Retry, CurrentStatus::Retrying, attempt);

            let delay = backoff(self.opts.retry_delay, self.opts.max_retry_delay, attempt);
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            if self.opts.reopen_on_error {
                self.try_reopen();
            }

            for block in bad {
                self.probe_sweep(block)?;
                if self.cancelled {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// 不良域を舐める(PLAN.md 11.2b)。
    ///
    /// 探索単位はセクタから始める。範囲が
    /// [`ImageOptions::probe_size_max`] 以下なら最後までセクタ単位で総当たりし、
    /// それより大きい範囲では失敗が続くたびに単位を倍にする(成功したら
    /// セクタ単位へ戻して、読める島を細かく拾う)。
    ///
    /// 不良セクタが数個という普通のケースは範囲が小さいので、従来どおり
    /// 1 セクタまで正確に絞られる。数 MiB が丸ごと死んでいるケースだけ、
    /// セクタ数に比例した失敗を踏まずに抜ける。
    ///
    /// 代償は、広げた単位で失敗した範囲に読める部分が混ざっていると
    /// 巻き添えで不良として記録すること。取りこぼしを許さないときは
    /// `probe_size_max` をセクタサイズにすれば、全域が総当たりになる。
    fn probe_sweep(&mut self, block: Block) -> Result<()> {
        let max_probe = self.align_chunk(self.opts.probe_size_max).max(self.sector);
        // 小さい不良域は総当たりで足りる。粗くしていいのは死に領域が広いときだけ。
        let widen = block.size > max_probe;
        let mut probe = self.sector;
        let mut pos = block.pos;

        while pos < block.end() {
            if self.check_cancel() {
                return Ok(());
            }
            let len = probe.min(block.end() - pos);
            match self.rescue(pos, len)? {
                Some(n) => {
                    pos += n;
                    probe = self.sector;
                }
                None => {
                    self.map.blocks.mark(pos, len, BlockStatus::BadSector);
                    pos += len;
                    if widen {
                        probe = probe.saturating_mul(2).min(max_probe);
                    }
                }
            }
            self.advance_to(pos);
        }
        Ok(())
    }

    /// 指定範囲を読んでイメージへ書き、取得済みにする。
    ///
    /// 読めたバイト数を返す。読めなければ `None`(不良のマークは呼び出し側の役目。
    /// パスによって付ける状態が違うため)。
    fn rescue(&mut self, pos: u64, len: u64) -> Result<Option<u64>> {
        let len = len as usize;
        // device と buf を同時に借りられないので、一時的に取り出す。
        let mut buf = std::mem::take(&mut self.buf);
        if buf.len() < len {
            buf.resize(len, 0);
        }
        let result = self.device.read_at(pos, &mut buf[..len]);

        let out = match result {
            Ok(0) => {
                self.errors += 1;
                None
            }
            Ok(n) => {
                self.writer.write_at(pos, &buf[..n])?;
                self.map.blocks.mark(pos, n as u64, BlockStatus::Finished);
                self.rescued_this_run += n as u64;
                Some(n as u64)
            }
            Err(e) if is_fatal(&e) => {
                self.buf = buf;
                return Err(e.into());
            }
            Err(e) => {
                if matches!(e, DeviceError::Timeout { .. }) {
                    // 応答が返らずに見捨てた読み込み。エラーとは別に数えて、
                    // 「不良セクタ」と「固まったメディア」を取り違えないようにする。
                    self.timeouts += 1;
                    warn!(pos, len, error = %e, "読み込みが返らなかった");
                } else {
                    debug!(pos, len, error = %e, "読み込み失敗");
                }
                self.errors += 1;
                None
            }
        };
        self.buf = buf;
        Ok(out)
    }

    /// USB コントローラが固まったケースに備えてハンドルを開き直す(PLAN.md 5.2)。
    fn try_reopen(&mut self) {
        match self.device.reopen() {
            Ok(true) => {
                self.reopens += 1;
                warn!("エラーが続いたのでデバイスハンドルを開き直した");
            }
            Ok(false) => {}
            Err(e) => warn!(error = %e, "デバイスハンドルの開き直しに失敗"),
        }
    }

    fn begin_pass(&mut self, pass: Pass, status: CurrentStatus, number: u32) {
        self.pass = pass;
        self.pass_number = number;
        self.map.current_status = status;
        debug!(pass = %pass, number, "パス開始");
        self.tick(true);
    }

    fn advance_to(&mut self, pos: u64) {
        self.position = pos;
        self.map.current_pos = pos;
        self.tick(false);
        // 保存失敗はイメージング自体を止めるほどではないので、記録だけして続ける。
        if let Err(e) = self.save_map_if_due() {
            warn!(error = %e, "mapfile の保存に失敗");
        }
    }

    fn check_cancel(&mut self) -> bool {
        if !self.cancelled && self.cancel.load(Ordering::Relaxed) {
            info!("キャンセルされたので中断する");
            self.cancelled = true;
        }
        self.cancelled
    }

    fn align_chunk(&self, value: u64) -> u64 {
        let sector = self.sector;
        (value / sector).max(1) * sector
    }

    fn tick(&mut self, force: bool) {
        if !force && self.last_progress.elapsed() < self.opts.progress_interval {
            return;
        }
        self.last_progress = Instant::now();

        let Some(callback) = self.progress.as_deref_mut() else {
            return;
        };
        let elapsed = self.started.elapsed();
        let secs = elapsed.as_secs_f64();
        let rate = if secs > 0.0 {
            (self.rescued_this_run as f64 / secs) as u64
        } else {
            0
        };
        let blocks = &self.map.blocks;
        let bad = blocks.bytes_with(BlockStatus::BadSector);
        let pending = blocks.remaining() - bad;
        let eta = (rate > 0).then(|| Duration::from_secs_f64(pending as f64 / rate as f64));

        let progress = Progress {
            pass: self.pass,
            pass_number: self.pass_number,
            position: self.position,
            total: blocks.total(),
            rescued: blocks.rescued(),
            bad,
            pending,
            errors: self.errors,
            elapsed,
            rate,
            eta,
            map: blocks.downsample(crate::progress::MAP_SEGMENTS),
        };
        callback(&progress);
    }

    fn save_map_if_due(&mut self) -> Result<()> {
        if self.last_save.elapsed() < self.opts.map_save_interval {
            return Ok(());
        }
        self.save_map()
    }

    fn save_map(&mut self) -> Result<()> {
        if let Some(path) = &self.map_path {
            self.map.save(path)?;
            self.last_save = Instant::now();
        }
        Ok(())
    }

    fn finalize(&mut self) -> Result<()> {
        self.map.current_status = if self.map.blocks.is_complete() {
            CurrentStatus::Finished
        } else {
            CurrentStatus::Copying
        };
        self.tick(true);
        let sync = self.writer.sync();
        self.save_map()?;
        sync
    }

    fn summary(&self) -> ImageSummary {
        ImageSummary::from_blocks(
            &self.map.blocks,
            self.errors,
            self.timeouts,
            self.reopens,
            self.started.elapsed(),
            self.cancelled,
        )
    }
}

/// 続行できない種類のデバイスエラーか。
///
/// メディア不良や一過性の IO エラー、応答が返らずに見捨てた読み込みは
/// 「不良として記録して先へ進む」対象なので偽。権限不足やデバイス消失、
/// 応答しない読み込みが上限に達した場合は、先へ進んでも無駄なので真。
fn is_fatal(error: &DeviceError) -> bool {
    matches!(
        error,
        DeviceError::PermissionDenied { .. }
            | DeviceError::NotFound(_)
            | DeviceError::Unsupported(_)
            | DeviceError::OutOfRange { .. }
            | DeviceError::Unaligned { .. }
            | DeviceError::Unresponsive { .. }
    )
}

/// 指数バックオフ。`attempt` は 1 始まり。
fn backoff(base: Duration, max: Duration, attempt: u32) -> Duration {
    if base.is_zero() {
        return Duration::ZERO;
    }
    let factor = 1u32 << attempt.saturating_sub(1).min(16);
    base.saturating_mul(factor).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_saturates() {
        let base = Duration::from_millis(100);
        let max = Duration::from_secs(2);
        assert_eq!(backoff(base, max, 1), Duration::from_millis(100));
        assert_eq!(backoff(base, max, 2), Duration::from_millis(200));
        assert_eq!(backoff(base, max, 3), Duration::from_millis(400));
        assert_eq!(backoff(base, max, 9), max);
        assert_eq!(backoff(Duration::ZERO, max, 3), Duration::ZERO);
    }

    #[test]
    fn fatal_errors_stop_the_run() {
        assert!(is_fatal(&DeviceError::Unsupported("x".into())));
        assert!(is_fatal(&DeviceError::Unresponsive { abandoned: 8 }));
        assert!(!is_fatal(&DeviceError::Media {
            offset: 0,
            len: 512
        }));
        // 1 回のタイムアウトは「読めなかった領域」。記録して先へ進む。
        assert!(!is_fatal(&DeviceError::Timeout {
            offset: 0,
            len: 512,
            waited: Duration::from_secs(30),
        }));
    }
}
