//! カービングのジョブ本体。
//!
//! デバイス(または取得済みイメージ)を先頭から走査し、シグネチャに当たった所を
//! バリデータに掛けて、本物だったものを出力先へ切り出す(PLAN.md 5.4)。
//!
//! 進行は 1 本道:
//!
//! 1. [`crate::scanner::Scanner`] が窓ごとにマジックバイトの位置を集める。
//! 2. 各候補をバリデータに渡し、本物か確かめて終端を求める。
//! 3. 終端が求まらなかったものは「次のシグネチャの手前」か最大サイズで切る。
//! 4. 切り出したファイルの範囲は走査対象から外す(Exif 内のサムネイル JPEG や
//!    ZIP 内の画像を二重に拾わないため)。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ofr_device::Device;
use tracing::debug;

use crate::error::{CarveError, Result};
use crate::format::{Confidence, FileFormat};
use crate::output::{Writer, file_name};
use crate::progress::{CarveProgress, FoundFn, ProgressFn};
use crate::reader::Reader;
use crate::result::{CarveReport, CarveSummary, CarvedFile};
use crate::scanner::{Hit, Scanner};
use crate::signature::SIGNATURES;

/// カービングの設定。
#[derive(Debug, Clone)]
pub struct CarveOptions {
    /// ファイル先頭を探す境界。既定 512(セクタ境界)。
    ///
    /// FAT / exFAT ではファイル先頭は必ずクラスタ境界に来るので、
    /// クラスタサイズ(4096 など)を指定すると誤検出が減り走査も速くなる。
    /// 1 にすると全バイトを候補にする(最も遅く、最も拾う)。
    pub align: u64,
    /// 走査窓の大きさ。既定 4MiB。
    pub chunk_size: usize,
    /// バリデータが使う読み出し窓の大きさ。既定 1MiB。
    pub window_size: usize,
    /// 対象形式。`None` なら全形式。
    pub formats: Option<Vec<FileFormat>>,
    /// 1 ファイルの最大サイズ。形式ごとの上限とのうち小さい方が使われる。
    pub max_file_size: u64,
    /// これより小さい切り出しは雑音とみなして捨てる。
    pub min_file_size: u64,
    /// 走査開始位置。
    pub start_offset: u64,
    /// 走査終了位置。`None` ならデバイス末尾。
    pub end_offset: Option<u64>,
    /// 終端を確定できなかったファイルも出力するか。
    pub include_truncated: bool,
    /// 進捗イベントの最短間隔。既定 100ms(PLAN.md 5.7)。
    pub progress_interval: Duration,
}

impl Default for CarveOptions {
    fn default() -> Self {
        Self {
            align: 512,
            chunk_size: 4 << 20,
            window_size: 1 << 20,
            formats: None,
            max_file_size: 4 << 30,
            min_file_size: 64,
            start_offset: 0,
            end_offset: None,
            include_truncated: true,
            progress_interval: Duration::from_millis(100),
        }
    }
}

/// カービングジョブ。
///
/// ```no_run
/// use std::path::Path;
/// use ofr_device::FileDevice;
/// use ofr_carve::{CarveOptions, Carver};
///
/// let device = FileDevice::open("usb.img")?;
/// let report = Carver::new(&device)
///     .with_options(CarveOptions { align: 4096, ..CarveOptions::default() })
///     .with_found(|f| println!("{} ({} バイト)", f.file_name, f.size))
///     .run(Some(Path::new("recovered")))?;
///
/// println!("{} 件", report.summary.found);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct Carver<'a> {
    device: &'a dyn Device,
    options: CarveOptions,
    cancel: Arc<AtomicBool>,
    progress: Option<ProgressFn>,
    found: Option<FoundFn>,
}

impl<'a> Carver<'a> {
    /// 既定の設定でジョブを作る。
    pub fn new(device: &'a dyn Device) -> Self {
        Self {
            device,
            options: CarveOptions::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: None,
            found: None,
        }
    }

    /// 設定を差し替える。
    pub fn with_options(mut self, options: CarveOptions) -> Self {
        self.options = options;
        self
    }

    /// キャンセルフラグを共有する。真になった時点で安全な区切りで中断する。
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }

    /// 進捗コールバックを登録する。呼び出し間隔は
    /// [`CarveOptions::progress_interval`] に間引かれる。
    pub fn with_progress(mut self, f: impl FnMut(&CarveProgress) + Send + 'static) -> Self {
        self.progress = Some(Box::new(f));
        self
    }

    /// ファイル発見コールバックを登録する。見つかった順にそのまま流す。
    pub fn with_found(mut self, f: impl FnMut(&CarvedFile) + Send + 'static) -> Self {
        self.found = Some(Box::new(f));
        self
    }

    /// 走査だけ行い、ファイルは書き出さない。
    pub fn scan(&mut self) -> Result<CarveReport> {
        self.run(None)
    }

    /// 走査して、見つけたファイルを `dest` へ書き出す。
    ///
    /// `dest` が `None` なら書き出さずに一覧だけを作る。
    pub fn run(&mut self, dest: Option<&Path>) -> Result<CarveReport> {
        let device_len = self.device.len();
        let opts = self.options.clone();
        if opts.align == 0 {
            return Err(CarveError::InvalidOptions(
                "整列幅は 1 以上でなければならない".to_string(),
            ));
        }
        if opts.max_file_size == 0 {
            return Err(CarveError::InvalidOptions(
                "最大ファイルサイズは 1 以上でなければならない".to_string(),
            ));
        }

        let end = opts.end_offset.unwrap_or(device_len).min(device_len);
        // 走査開始位置も境界に揃える。
        let start = (opts.start_offset / opts.align) * opts.align;
        if start >= end {
            return Ok(CarveReport::default());
        }

        let mut scanner = Scanner::new(opts.formats.as_deref(), opts.align, opts.chunk_size);
        if !scanner.is_active() {
            return Err(CarveError::InvalidOptions(
                "対象形式が 1 つも選ばれていない".to_string(),
            ));
        }
        let writer = match dest {
            Some(path) => Some(Writer::create(path, false)?),
            None => None,
        };
        let mut reader = Reader::new(self.device, opts.window_size);

        debug!(
            start,
            end,
            align = opts.align,
            dest = ?writer.as_ref().map(Writer::root),
            "カービングを開始する"
        );

        let started = Instant::now();
        let mut last_progress = started;
        let mut report = CarveReport::default();
        let mut hits: Vec<Hit> = Vec::new();
        let mut pos = start;
        // これより前の候補は処理済み。切り出したファイルの内側もここで飛ばす。
        let mut resume = start;
        let mut cancelled = false;

        while pos < end {
            if self.cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let filled = scanner.scan_window(self.device, pos, end, &mut hits);
            if filled == 0 {
                break;
            }

            for i in 0..hits.len() {
                if self.cancel.load(Ordering::Relaxed) {
                    cancelled = true;
                    break;
                }
                let hit = hits[i];
                if hit.file_start < resume {
                    continue;
                }
                // 次のシグネチャ位置。終端を確定できなかったときの上限に使う。
                let next_signature = hits[i + 1..]
                    .iter()
                    .map(|h| h.file_start)
                    .find(|s| *s > hit.file_start);

                resume = hit.file_start + 1;
                let Some(mut file) = self.probe(
                    &mut reader,
                    &hit,
                    next_signature,
                    end,
                    report.files.len() as u64,
                ) else {
                    continue;
                };
                if let Some(w) = &writer {
                    w.write(self.device, &mut file)?;
                } else {
                    file.bytes_written = file.size;
                }

                resume = file.end();
                report.summary.found += 1;
                report.summary.bytes_recovered += file.size;
                report.summary.bad_bytes += file.bad_bytes;
                if file.confidence.is_exact() {
                    report.summary.exact += 1;
                }
                if let Some(cb) = &mut self.found {
                    cb(&file);
                }
                debug!(
                    offset = file.offset,
                    size = file.size,
                    format = %file.format,
                    confidence = %file.confidence,
                    "切り出した"
                );
                report.files.push(file);
            }
            if cancelled {
                break;
            }

            // 窓の継ぎ目にまたがるシグネチャを落とさないよう重ねて進む。
            let advance = filled.saturating_sub(scanner.overlap()).max(1) as u64;
            pos = (pos + advance).max(resume.min(end));

            let now = Instant::now();
            if now.duration_since(last_progress) >= opts.progress_interval {
                last_progress = now;
                self.emit_progress(pos, start, end, &report.summary, &scanner, &reader, started);
            }
        }

        report.summary.scanned = pos.min(end).saturating_sub(start);
        report.summary.read_errors = scanner.read_errors() + reader.read_errors();
        report.summary.elapsed = started.elapsed();
        report.summary.cancelled = cancelled;
        self.emit_progress(
            pos.min(end),
            start,
            end,
            &report.summary,
            &scanner,
            &reader,
            started,
        );
        debug!(
            found = report.summary.found,
            bytes = report.summary.bytes_recovered,
            cancelled,
            "カービングを終了する"
        );
        Ok(report)
    }

    /// 1 つの候補をバリデータに掛けて、切り出す 1 ファイルに仕上げる。
    fn probe(
        &self,
        reader: &mut Reader<'_>,
        hit: &Hit,
        next_signature: Option<u64>,
        end: u64,
        found_so_far: u64,
    ) -> Option<CarvedFile> {
        let sig = &SIGNATURES[hit.signature];
        let cap = sig.max_size.min(self.options.max_file_size);
        let limit = hit.file_start.saturating_add(cap).min(end);
        if limit <= hit.file_start {
            return None;
        }

        let candidate = (sig.probe)(reader, hit.file_start, limit)?;
        // 1 つのシグネチャが複数の形式を生むことがある(ISO-BMFF → mp4 / mov / heic)。
        // 実際の形式が決まったここでもう一度絞り込む。
        if let Some(list) = &self.options.formats
            && !list.contains(&candidate.format)
        {
            return None;
        }
        if candidate.confidence == Confidence::Truncated && !self.options.include_truncated {
            return None;
        }

        // 終端が確定していないものは、次のシグネチャの手前までに切り詰める
        // (PLAN.md 5.4)。確定分より短くはしない。
        let mut size = candidate.size;
        if candidate.confidence == Confidence::Truncated
            && let Some(next) = next_signature
        {
            size = size.min(next - hit.file_start).max(candidate.min_size);
        }
        let size = size.min(end - hit.file_start);
        if size < self.options.min_file_size {
            return None;
        }

        let index = found_so_far + 1;
        Some(CarvedFile {
            index,
            offset: hit.file_start,
            size,
            format: candidate.format,
            extension: candidate.extension,
            confidence: candidate.confidence,
            file_name: file_name(index, candidate.extension, &candidate.metadata),
            metadata: candidate.metadata,
            bytes_written: 0,
            bad_bytes: 0,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_progress(
        &mut self,
        pos: u64,
        start: u64,
        end: u64,
        summary: &CarveSummary,
        scanner: &Scanner,
        reader: &Reader<'_>,
        started: Instant,
    ) {
        let Some(cb) = &mut self.progress else {
            return;
        };
        let elapsed = started.elapsed();
        let scanned = pos.saturating_sub(start);
        let secs = elapsed.as_secs_f64();
        let rate = if secs > 0.0 {
            (scanned as f64 / secs) as u64
        } else {
            0
        };
        let eta = (rate > 0).then(|| Duration::from_secs(end.saturating_sub(pos) / rate));
        cb(&CarveProgress {
            position: pos,
            start,
            end,
            found: summary.found,
            bytes_recovered: summary.bytes_recovered,
            read_errors: scanner.read_errors() + reader.read_errors(),
            elapsed,
            rate,
            eta,
        });
    }
}
