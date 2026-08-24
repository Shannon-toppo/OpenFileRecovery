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

use ofr_device::{Device, DeviceError, Route};
use tracing::{debug, info, warn};

use crate::blocks::{Block, BlockStatus};
use crate::domain::Domain;
use crate::error::{ImageError, Result};
use crate::mapfile::{CurrentStatus, MapFile};
use crate::progress::{ImageSummary, Pass, Progress, ProgressFn, Stall, StallAction, StallFn};
use crate::writer::ImageWriter;

/// ハンドルの開き直しを試みるまでの連続エラー回数。
const REOPEN_AFTER_ERRORS: u32 = 4;

/// 転送単位を変えて読み直すときの単位(PLAN.md 11.5)。
///
/// セクタ単位で駄目なら 4KiB 単位、4KiB 単位で駄目なら 512B 単位で当たり直す。
const ALT_UNIT: u64 = 4096;
/// 同上、細かいほうの単位。
const SECTOR_UNIT: u64 = 512;

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
    /// 既に読めなかった領域だけを対象にする(PLAN.md 11.3)。
    ///
    /// コピーパスを飛ばすので、未試行の領域には触らない。時間をおいて、
    /// 冷ましてから、別のリーダーで、といった条件を変えた再挑戦に使う。
    /// 既存の mapfile が要る。
    pub retry_only: bool,
    /// 開始前に、不良として記録されている領域を未試行に戻す(PLAN.md 11.3)。
    ///
    /// 抜き差しで電源が入り直った後など、前回の判定をやり直したいときに使う。
    /// 取得済みの領域は読み直さない。
    pub reset_bad: bool,
    /// 読む範囲を限定する(PLAN.md 11.4)。`None` なら全域。
    ///
    /// 範囲の外は読まないし、mapfile 上も触らない(未試行のまま残る)。
    /// 走査で選んだファイルのクラスタ範囲だけを取り直すときに使う。
    /// 進捗とサマリの分母も、限定した範囲の合計になる。
    pub domain: Option<Domain>,
    /// コピーパスの最初に確保しておく範囲(PLAN.md 11.4)。
    ///
    /// ブートセクタ、FAT / アロケーションビットマップ、ディレクトリ領域を先に
    /// 取っておくと、途中で読めなくなっても構造だけは残る。
    /// [`domain`](Self::domain) を指定した場合は、その中に限られる。
    pub priority: Option<Domain>,
    /// 末尾から手前へ向かって読む(PLAN.md 11.5)。
    ///
    /// ブリッジの先読みが不良 LBA を巻き込んで手前まで道連れにすることがあり、
    /// 反対側から詰めると 1 個ぶん多く取れる。リトライパスは、この指定に関係なく
    /// 試行ごとに向きを入れ替える(同じ向きで粘っても結果が変わらないため)。
    pub reverse: bool,
    /// 読めなかったセクタを別経路でも読み直す(PLAN.md 11.5)。
    ///
    /// macOS はバッファ付きのブロックデバイス、Windows は
    /// `FILE_FLAG_NO_BUFFERING` を外したハンドル。カーネル側で別のリトライが
    /// 走るので、通常経路で駄目でも通ることがある。リトライパスでだけ使う。
    pub alt_route: bool,
    /// リトライパスで読めた範囲を、何回読んで多数決を取るか(PLAN.md 11.5)。
    ///
    /// 0 と 1 は「多数決なし」。2 以上にすると、リトライで拾えた範囲を最大その回数
    /// まで読み直し、一番多く一致した内容を採る。一度も一致しなければ最初の内容を
    /// 残したうえで「不確か」として記録する([`ImageSummary::uncertain`])。
    ///
    /// 全域をやると時間が単純に N 倍になるので、対象はリトライパスに限ってある。
    /// 内容が揺れるのは、そもそも読めたり読めなかったりする領域だから。
    pub verify: u32,
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
            retry_only: false,
            reset_bad: false,
            domain: None,
            priority: None,
            reverse: false,
            alt_route: true,
            verify: 0,
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
    stall: Option<StallFn>,
}

impl<'a> Imager<'a> {
    /// 既定の設定でジョブを作る。
    pub fn new(device: &'a dyn Device) -> Self {
        Self {
            device,
            options: ImageOptions::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: None,
            stall: None,
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

    /// デバイスが応答しなくなったときに呼ばれるコールバックを登録する(PLAN.md 11.3)。
    ///
    /// 登録しない場合、応答しない読み込みが続くとその場で打ち切る
    /// ([`ofr_device::DeviceError::Unresponsive`])。登録した場合は
    /// コールバックの判断に従い、[`StallAction::Resume`] なら
    /// デバイスの復帰を伝えて同じ範囲から読み直す。
    ///
    /// 抜き差しを待つ処理はコールバックの中で行う(ここでブロックしてよい)。
    pub fn with_stall_handler(
        mut self,
        f: impl FnMut(&Stall) -> StallAction + Send + 'static,
    ) -> Self {
        self.stall = Some(Box::new(f));
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
            None => {
                if self.options.retry_only {
                    return Err(ImageError::InvalidOptions(
                        "再試行だけを行うには、前回の記録 (mapfile) が要る".to_string(),
                    ));
                }
                MapFile::new(device_len)
            }
        };
        // 読む範囲の限定と、先に確保する範囲(PLAN.md 11.4)。読み込みはセクタ単位なので
        // 外側へ丸める。優先範囲は限定範囲の中に閉じ込める。
        let domain = match &self.options.domain {
            Some(d) => {
                let d = d.aligned(sector, device_len);
                if d.is_empty() {
                    return Err(ImageError::InvalidOptions(
                        "指定された範囲がデバイス上にない".to_string(),
                    ));
                }
                Some(d)
            }
            None => None,
        };
        let priority = self.options.priority.as_ref().and_then(|p| {
            let p = p.aligned(sector, device_len);
            let p = match &domain {
                Some(d) => p.intersect(d),
                None => p,
            };
            (!p.is_empty()).then_some(p)
        });

        let mut map = map;
        if self.options.reset_bad {
            // 前回の「読めなかった」判定を捨てる。取得済みの領域はそのまま。
            let stale: Vec<Block> = map
                .blocks
                .blocks()
                .iter()
                .filter(|b| !b.status.is_rescued())
                .copied()
                .collect();
            // 範囲を限定しているなら、その外の記録には触らない。
            let stale = match &domain {
                Some(d) => d.clip_all(&stale),
                None => stale,
            };
            let bytes: u64 = stale.iter().map(|b| b.size).sum();
            for b in stale {
                map.blocks.mark(b.pos, b.size, BlockStatus::NonTried);
            }
            info!(bytes, "不良の記録を消して読み直す");
        }
        let writer = ImageWriter::create(image_path, device_len, existing.is_some())?;
        // 前回の実行で「不確か」と記録した範囲は引き継ぐ(PLAN.md 11.5)。
        let uncertain = map.uncertain.total();

        let now = Instant::now();
        let mut run = Run {
            device: self.device,
            opts: &self.options,
            cancel: &self.cancel,
            progress: self.progress.as_mut(),
            stall: self.stall.as_mut(),
            writer,
            map,
            map_path: map_path.map(Path::to_path_buf),
            domain,
            priority,
            sector,
            buf: Vec::new(),
            started: now,
            last_progress: now,
            last_save: now,
            rescued_this_run: 0,
            alt_route: self.options.alt_route && self.device.has_alternate(),
            alt_rescues: 0,
            resized_rescues: 0,
            uncertain,
            errors: 0,
            timeouts: 0,
            reopens: 0,
            stalls: 0,
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
    stall: Option<&'r mut StallFn>,
    writer: ImageWriter,
    map: MapFile,
    map_path: Option<PathBuf>,
    /// 読む範囲の限定(PLAN.md 11.4)。`None` なら全域。
    domain: Option<Domain>,
    /// コピーパスの最初に確保する範囲。
    priority: Option<Domain>,
    sector: u64,
    buf: Vec<u8>,
    started: Instant,
    last_progress: Instant,
    last_save: Instant,
    rescued_this_run: u64,
    /// 別経路を持っているか(PLAN.md 11.5)。開いた時点で分かるので控えておく。
    alt_route: bool,
    /// 別経路で拾えたバイト数(PLAN.md 11.5)。
    alt_rescues: u64,
    /// 転送単位を変えて拾えたバイト数(PLAN.md 11.5)。
    resized_rescues: u64,
    /// 多数決で内容が揃わなかったバイト数(PLAN.md 11.5)。
    uncertain: u64,
    errors: u64,
    timeouts: u64,
    reopens: u32,
    stalls: u32,
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
        if !self.opts.retry_only {
            self.copy_pass()?;
        }
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
        self.priority_pass()?;
        if self.cancelled {
            return Ok(());
        }

        let mut sweep = 1;
        loop {
            let before = self.bytes_with(BlockStatus::NonTried);
            if before == 0 {
                return Ok(());
            }
            self.begin_pass(Pass::Copy, CurrentStatus::Copying, sweep);
            self.copy_sweep(self.ranges_with(BlockStatus::NonTried))?;
            if self.cancelled {
                return Ok(());
            }
            let after = self.bytes_with(BlockStatus::NonTried);
            if after == 0 || after >= before {
                return Ok(());
            }
            sweep += 1;
        }
    }

    /// パス 0: メタデータなど、先に確保しておく範囲だけを掃く(PLAN.md 11.4)。
    ///
    /// ブートセクタ、FAT / アロケーションビットマップ、ディレクトリ領域が
    /// これに当たる。全体を舐める前にここを取っておけば、途中で読めなくなっても
    /// 構造は残る(どこに何があったかが分かる)。掃き方はコピーパスと同じ。
    fn priority_pass(&mut self) -> Result<()> {
        let Some(priority) = self.priority.clone() else {
            return Ok(());
        };
        let targets = priority.clip_all(&self.ranges_with(BlockStatus::NonTried));
        if targets.is_empty() {
            return Ok(());
        }
        let bytes: u64 = targets.iter().map(|b| b.size).sum();
        info!(bytes, regions = targets.len(), "メタデータを先に確保する");
        self.begin_pass(Pass::Priority, CurrentStatus::Copying, 1);
        self.copy_sweep(targets)
    }

    /// 渡された未試行の範囲を 1 回舐める。
    ///
    /// 向きは [`ImageOptions::reverse`](crate::ImageOptions::reverse)(PLAN.md 11.5)。
    /// 逆向きでも、読める所を先に確保して不良域を飛ばす作りは変わらない。
    fn copy_sweep(&mut self, targets: Vec<Block>) -> Result<()> {
        let chunk = self.align_chunk(self.opts.chunk_size);
        let reverse = self.opts.reverse;
        let mut skip = 0u64;
        let mut error_streak = 0u32;

        for block in order(targets, reverse) {
            // 前向きなら「まだ読んでいない先頭」、逆向きなら「まだ読んでいない末尾」。
            let mut cursor = if reverse { block.end() } else { block.pos };
            while remaining(cursor, block, reverse) {
                if self.check_cancel() {
                    return Ok(());
                }
                let (pos, len) = take(cursor, chunk, block, reverse);
                match self.rescue(pos, len)? {
                    Some(n) => {
                        cursor = step(cursor, n, reverse);
                        skip = 0;
                        error_streak = 0;
                    }
                    None => {
                        // 読めなかった範囲は「未トリム」にして端を詰める仕事に回す。
                        self.map.blocks.mark(pos, len, BlockStatus::NonTrimmed);
                        // その先も道連れで死んでいる公算が大きいので読み飛ばす。
                        // 飛ばした範囲は未試行のまま残るので、次の掃きで読み直す。
                        skip = self.next_skip(skip);
                        cursor = step(cursor, len + skip, reverse);
                        error_streak += 1;
                        // エラーが続くならデバイスの応答が悪い。ハンドルを開き直す。
                        if error_streak >= REOPEN_AFTER_ERRORS {
                            self.try_reopen();
                            error_streak = 0;
                        }
                    }
                }
                self.advance_to(cursor.clamp(block.pos, block.end()));
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
        let targets = self.ranges_with(BlockStatus::NonTrimmed);
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
        let targets = self.ranges_with(BlockStatus::NonScraped);
        if targets.is_empty() {
            return Ok(());
        }
        self.begin_pass(Pass::Scrape, CurrentStatus::Scraping, 1);

        for block in order(targets, self.opts.reverse) {
            self.probe_sweep(block, self.opts.reverse, false)?;
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
    ///
    /// ここだけは手を変えて粘る(PLAN.md 11.5)。同じ向き・同じ経路・同じ転送単位で
    /// 繰り返しても、返ってくる答えは変わらないため。
    ///
    /// - **向き**: 試行ごとに前後を入れ替える。ブリッジの先読みが不良 LBA を
    ///   巻き込んで手前まで道連れにする個体では、反対側から詰めると 1 個ぶん多く取れる
    /// - **経路**: 読めなければ別経路([`ofr_device::Route::Alternate`])でもう一度
    /// - **転送単位**: それでも駄目なら、整列と長さを変えて読む
    fn retry_pass(&mut self) -> Result<()> {
        for attempt in 1..=self.opts.retries {
            let bad = self.ranges_with(BlockStatus::BadSector);
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

            // 試行ごとに向きを入れ替える。1 回目は設定どおりの向きから。
            let reverse = self.opts.reverse ^ (attempt % 2 == 0);
            for block in order(bad, reverse) {
                self.probe_sweep(block, reverse, true)?;
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
    /// `hard` が真なら、読めなかったセクタに経路変えと転送単位変えを重ねる
    /// (PLAN.md 11.5)。リトライパスだけが真で渡す。
    fn probe_sweep(&mut self, block: Block, reverse: bool, hard: bool) -> Result<()> {
        let max_probe = self.align_chunk(self.opts.probe_size_max).max(self.sector);
        // 小さい不良域は総当たりで足りる。粗くしていいのは死に領域が広いときだけ。
        let widen = block.size > max_probe;
        let mut probe = self.sector;
        let mut cursor = if reverse { block.end() } else { block.pos };

        while remaining(cursor, block, reverse) {
            if self.check_cancel() {
                return Ok(());
            }
            let (pos, len) = take(cursor, probe, block, reverse);
            let read = if hard {
                self.rescue_hard(pos, len)?
            } else {
                self.rescue(pos, len)?
            };
            match read {
                Some(n) => {
                    cursor = step(cursor, n, reverse);
                    probe = self.sector;
                }
                None => {
                    self.map.blocks.mark(pos, len, BlockStatus::BadSector);
                    cursor = step(cursor, len, reverse);
                    if widen {
                        probe = probe.saturating_mul(2).min(max_probe);
                    }
                }
            }
            self.advance_to(cursor.clamp(block.pos, block.end()));
        }
        Ok(())
    }

    /// 指定範囲を読んでイメージへ書き、取得済みにする。
    ///
    /// 読めたバイト数を返す。読めなければ `None`(不良のマークは呼び出し側の役目。
    /// パスによって付ける状態が違うため)。
    fn rescue(&mut self, pos: u64, len: u64) -> Result<Option<u64>> {
        match self.read_attempt(Route::Primary, pos, len as usize)? {
            Some(n) => {
                self.store(pos, n)?;
                Ok(Some(n as u64))
            }
            None => Ok(None),
        }
    }

    /// 読めなかったセクタに手を変えて当たり直す(PLAN.md 11.5)。
    ///
    /// 通常経路 → 別経路 → 転送単位を変える、の順。どれも安いので、
    /// リトライパスの最後の手当てとして順に挟む。読めた範囲は
    /// [`ImageOptions::verify`](crate::ImageOptions::verify) の指定があれば
    /// 多数決にかける。
    fn rescue_hard(&mut self, pos: u64, len: u64) -> Result<Option<u64>> {
        let len = len as usize;

        // 1. 普段どおり読む。
        if let Some(n) = self.read_attempt(Route::Primary, pos, len)? {
            return self.accept(Route::Primary, pos, n).map(Some);
        }
        if self.cancelled {
            return Ok(None);
        }

        // 2. 別経路。カーネル側で別のリトライが走るので、通ることがある。
        if self.alt_route
            && let Some(n) = self.read_attempt(Route::Alternate, pos, len)?
        {
            debug!(pos, len, "別経路で読めた");
            self.alt_rescues += n as u64;
            return self.accept(Route::Alternate, pos, n).map(Some);
        }
        if self.cancelled {
            return Ok(None);
        }

        // 3. 転送単位と整列を変える。ブリッジによっては、要求の形が変わるだけで通る。
        self.rescue_resized(pos, len)
    }

    /// 整列と転送単位を変えて読み直す(PLAN.md 11.5)。
    ///
    /// セクタ単位で駄目なら 4KiB 単位、4KiB 単位で駄目なら 512B 単位、というように
    /// 要求の形を変えて 1 回だけ試す。読めても**元の要求ぶんだけ**を採る。
    /// ついでに広く読めた部分まで信じると、次の掃きで読み直す機会を失うため。
    fn rescue_resized(&mut self, pos: u64, len: usize) -> Result<Option<u64>> {
        let unit = if self.sector == ALT_UNIT {
            SECTOR_UNIT
        } else {
            ALT_UNIT
        };
        let start = pos / unit * unit;
        let end = (pos + len as u64).div_ceil(unit) * unit;
        let end = end.min(self.map.blocks.total());
        if start >= end || (start == pos && end == pos + len as u64) {
            return Ok(None); // 形が変わらないなら試すだけ無駄。
        }

        let want = (end - start) as usize;
        let Some(n) = self.read_attempt(Route::Primary, start, want)? else {
            return Ok(None);
        };
        // 要求された範囲が読めた分に収まっていなければ、採れるものがない。
        let offset = (pos - start) as usize;
        if offset + len > n {
            return Ok(None);
        }
        debug!(pos, len, unit, "転送単位を変えたら読めた");
        self.resized_rescues += len as u64;
        self.buf.copy_within(offset..offset + len, 0);
        self.accept(Route::Primary, pos, len).map(Some)
    }

    /// 読めた内容を確かめて(必要なら多数決)、イメージへ書く(PLAN.md 11.5)。
    fn accept(&mut self, route: Route, pos: u64, n: usize) -> Result<u64> {
        if self.opts.verify >= 2 && n > 0 && !self.majority(route, pos, n)? {
            warn!(pos, len = n, "読むたびに内容が変わる。不確かとして記録する");
            self.mark_uncertain(pos, n as u64);
        }
        self.store(pos, n)?;
        Ok(n as u64)
    }

    /// 同じ範囲を何度か読んで多数決を取る(PLAN.md 11.5)。
    ///
    /// 一番多く一致した内容を [`Run::buf`] に残して `true`。どの 2 回も一致
    /// しなければ、最初に読めた内容を残して `false`(= 不確か)。
    fn majority(&mut self, route: Route, pos: u64, n: usize) -> Result<bool> {
        // (内容, 回数)。読み直す範囲はセクタ〜探索単位なので、持っても小さい。
        let mut tally: Vec<(Vec<u8>, u32)> = vec![(self.buf[..n].to_vec(), 1)];

        for _ in 1..self.opts.verify {
            if self.check_cancel() {
                break;
            }
            let Some(m) = self.read_attempt(route, pos, n)? else {
                continue; // 読めなかった回は数に入れない。
            };
            let got = &self.buf[..m.min(n)];
            match tally.iter_mut().find(|(data, _)| data == got) {
                Some((_, count)) => *count += 1,
                None => tally.push((got.to_vec(), 1)),
            }
        }

        let (winner, count) = tally
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .expect("最初の読み込みが必ず入っている");
        self.buf[..winner.len()].copy_from_slice(&winner);
        Ok(count > 1)
    }

    /// 内容が揺れた範囲を記録する(PLAN.md 11.5)。
    fn mark_uncertain(&mut self, pos: u64, len: u64) {
        let before = self.map.uncertain.total();
        self.map.uncertain.insert(pos, len);
        self.uncertain += self.map.uncertain.total() - before;
    }

    /// 読めた内容をイメージへ書いて、取得済みにする。
    fn store(&mut self, pos: u64, n: usize) -> Result<()> {
        self.writer.write_at(pos, &self.buf[..n])?;
        self.map.blocks.mark(pos, n as u64, BlockStatus::Finished);
        self.rescued_this_run += n as u64;
        Ok(())
    }

    /// 指定範囲を 1 回読む。読めたバイト数を返し、中身は [`Run::buf`] に残す。
    ///
    /// マップもイメージも触らない(状態の付け方はパスによって違うため)。
    /// デバイスが応答しなくなった場合だけ、足踏みハンドラの判断を仰いで
    /// 同じ範囲を読み直すことがある(PLAN.md 11.3)。
    fn read_attempt(&mut self, route: Route, pos: u64, len: usize) -> Result<Option<usize>> {
        loop {
            // device と buf を同時に借りられないので、一時的に取り出す。
            let mut buf = std::mem::take(&mut self.buf);
            if buf.len() < len {
                buf.resize(len, 0);
            }
            let result = self.device.read_at_via(route, pos, &mut buf[..len]);

            let out = match result {
                Ok(0) => {
                    self.errors += 1;
                    None
                }
                Ok(n) => Some(n),
                Err(DeviceError::Unresponsive { abandoned }) => {
                    self.buf = buf;
                    self.errors += 1;
                    self.timeouts += 1;
                    // 抜き差ししてもらえるなら、同じ範囲をもう一度読む。
                    if self.handle_stall(pos, abandoned)? {
                        continue;
                    }
                    if self.cancelled {
                        // 待っている間に中断された。エラーではなく中断として畳む。
                        return Ok(None);
                    }
                    return Err(DeviceError::Unresponsive { abandoned }.into());
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
            return Ok(out);
        }
    }

    /// デバイスが応答しなくなったときの後始末(PLAN.md 11.3)。
    ///
    /// 読み直してよければ `true`。ハンドラが登録されていない、打ち切りを選ばれた、
    /// キャンセルされた、開き直しに失敗した場合は `false`。
    fn handle_stall(&mut self, pos: u64, abandoned: u32) -> Result<bool> {
        self.stalls += 1;
        let stall = Stall {
            attempt: self.stalls,
            position: pos,
            timeouts: self.timeouts,
        };
        warn!(
            pos,
            abandoned,
            attempt = stall.attempt,
            "デバイスが応答しない"
        );

        let Some(handler) = self.stall.as_deref_mut() else {
            return Ok(false);
        };
        // 呼ばれた側は、抜き差しされるまでここでブロックしてよい。
        let action = handler(&stall);
        // 待っている間に中断されたかもしれないので、判断より先に見る。
        if self.check_cancel() || action == StallAction::Abort {
            return Ok(false);
        }
        // 電源が入り直った前提で、打ち切り判断からやり直す。
        match self.device.recovered() {
            Ok(true) => {
                self.reopens += 1;
                info!(pos, "デバイスが戻ったので読み直す");
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(e) => {
                warn!(error = %e, "デバイスを開き直せなかった");
                Ok(false)
            }
        }
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

    /// その状態のブロックのうち、対象範囲(PLAN.md 11.4)に入っている部分。
    ///
    /// 範囲を限定していなければマップのブロックそのまま。パス実行中はマップを
    /// 書き換えるので、パス開始時のスナップショットに対して処理を進める。
    fn ranges_with(&self, status: BlockStatus) -> Vec<Block> {
        let all = self.map.blocks.ranges_with(status);
        match &self.domain {
            Some(domain) => domain.clip_all(&all),
            None => all,
        }
    }

    /// 対象範囲に入っている、その状態の合計バイト数。
    fn bytes_with(&self, status: BlockStatus) -> u64 {
        match &self.domain {
            Some(_) => self.ranges_with(status).iter().map(|b| b.size).sum(),
            None => self.map.blocks.bytes_with(status),
        }
    }

    /// 対象範囲の全長。範囲を限定していなければデバイス全長。
    fn scope_total(&self) -> u64 {
        match &self.domain {
            Some(domain) => domain.total(),
            None => self.map.blocks.total(),
        }
    }

    /// 対象範囲のうち取得できたバイト数。
    fn scope_rescued(&self) -> u64 {
        self.bytes_with(BlockStatus::Finished)
    }

    /// 対象範囲を取り切ったか。
    fn scope_complete(&self) -> bool {
        self.scope_rescued() == self.scope_total()
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

        if self.progress.is_none() {
            return;
        }
        let elapsed = self.started.elapsed();
        let secs = elapsed.as_secs_f64();
        let rate = if secs > 0.0 {
            (self.rescued_this_run as f64 / secs) as u64
        } else {
            0
        };
        let total = self.scope_total();
        let rescued = self.scope_rescued();
        let bad = self.bytes_with(BlockStatus::BadSector);
        let pending = total - rescued - bad;
        let eta = (rate > 0).then(|| Duration::from_secs_f64(pending as f64 / rate as f64));

        let progress = Progress {
            pass: self.pass,
            pass_number: self.pass_number,
            position: self.position,
            total,
            rescued,
            bad,
            pending,
            errors: self.errors,
            elapsed,
            rate,
            eta,
            map: self.map.blocks.downsample(crate::progress::MAP_SEGMENTS),
        };
        if let Some(callback) = self.progress.as_deref_mut() {
            callback(&progress);
        }
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
        self.map.current_status = if self.scope_complete() {
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
        let total = self.scope_total();
        let rescued = self.scope_rescued();
        ImageSummary {
            total,
            rescued,
            bad: self.bytes_with(BlockStatus::BadSector),
            remaining: total - rescued,
            errors: self.errors,
            timeouts: self.timeouts,
            reopens: self.reopens,
            stalls: self.stalls,
            alt_rescues: self.alt_rescues,
            resized_rescues: self.resized_rescues,
            uncertain: self.uncertain,
            elapsed: self.started.elapsed(),
            cancelled: self.cancelled,
        }
    }
}

/// 掃く向きに合わせてブロックを並べ替える(PLAN.md 11.5)。
fn order(mut blocks: Vec<Block>, reverse: bool) -> Vec<Block> {
    if reverse {
        blocks.reverse();
    }
    blocks
}

/// この向きで、まだ読む所が残っているか。
///
/// `cursor` は前向きなら「次に読む位置」、逆向きなら「まだ読んでいない末尾」。
fn remaining(cursor: u64, block: Block, reverse: bool) -> bool {
    if reverse {
        cursor > block.pos
    } else {
        cursor < block.end()
    }
}

/// 次に読む範囲 `(位置, 長さ)`。
fn take(cursor: u64, unit: u64, block: Block, reverse: bool) -> (u64, u64) {
    if reverse {
        let len = unit.min(cursor - block.pos);
        (cursor - len, len)
    } else {
        (cursor, unit.min(block.end() - cursor))
    }
}

/// カーソルを進める。逆向きなら手前へ戻る。
fn step(cursor: u64, by: u64, reverse: bool) -> u64 {
    if reverse {
        cursor.saturating_sub(by)
    } else {
        cursor.saturating_add(by)
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
