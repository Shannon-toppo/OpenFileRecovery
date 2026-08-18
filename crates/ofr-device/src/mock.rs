//! エラー注入できるテスト用デバイス。
//!
//! 壊れた USB メモリは CI に置けないので、リトライ・パス制御・不良領域の記録は
//! すべてこのモックで検証する(PLAN.md 9章)。注入できるのは:
//!
//! - **恒久不良**: その範囲に触れる読み込みは常に失敗する。
//! - **一過性不良**: N 回失敗したあと成功する(USB コントローラの一時的な固まり)。
//! - **低速領域**: 読めるが遅い(Copy pass の速度閾値スキップの検証用)。
//! - **ハンドル固着**: [`Device::reopen`] を呼ぶまで失敗し続ける
//!   (USB コントローラが固まり、開き直すと復帰するケース)。
//! - **未整列拒否**: セクタ境界に整列していない読み込みを拒否する(Windows 非バッファIO 相当)。
//!
//! 読み込みの記録(オフセットと長さの列)も取れるので、
//! 「シーケンシャルに読んでいるか」「読み込み単位を縮小したか」を検証できる。

use std::ops::Range;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use crate::align::is_aligned;
use crate::device::{Device, DeviceInfo, DeviceKind, clamp_read};
use crate::error::{DeviceError, Result};

/// 注入する不良の種類。
#[derive(Debug)]
enum FaultKind {
    /// 常に失敗する。
    Hard,
    /// `fail_count` 回失敗したあとは成功する。
    Transient {
        fail_count: u32,
        attempts: AtomicU32,
    },
    /// 成功するが遅い。
    Slow { delay: Duration },
    /// [`Device::reopen`] が呼ばれるまで失敗し続ける。
    UntilReopen { healed: AtomicBool },
}

#[derive(Debug)]
struct Fault {
    range: Range<u64>,
    kind: FaultKind,
}

/// [`MockDevice`] の統計スナップショット。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MockStats {
    /// `read_at` が呼ばれた回数。
    pub read_calls: u64,
    /// 成功して返したバイト数の合計。
    pub bytes_read: u64,
    /// エラーを返した回数。
    pub failed_reads: u64,
    /// [`Device::reopen`] が呼ばれた回数。
    pub reopen_calls: u64,
}

/// テスト用の合成デバイス。
#[derive(Debug)]
pub struct MockDevice {
    data: Vec<u8>,
    info: DeviceInfo,
    faults: Vec<Fault>,
    read_delay: Option<Duration>,
    require_alignment: bool,
    max_read_len: Option<usize>,
    read_calls: AtomicU64,
    bytes_read: AtomicU64,
    failed_reads: AtomicU64,
    reopen_calls: AtomicU64,
    read_log: Option<Mutex<Vec<(u64, usize)>>>,
}

impl MockDevice {
    /// ビルダを作る。`size` はデバイスの全長(バイト)。
    pub fn builder(size: u64) -> MockDeviceBuilder {
        MockDeviceBuilder::new(size)
    }

    /// 全域がゼロで、不良のない健全なデバイスを作る。
    pub fn zeroed(size: u64) -> Self {
        MockDeviceBuilder::new(size).build()
    }

    /// 決定的なパターンで埋めた健全なデバイスを作る。
    ///
    /// 各バイトの期待値は [`MockDevice::pattern_byte`] で計算できるので、
    /// 復元結果の照合にそのまま使える。
    pub fn patterned(size: u64) -> Self {
        MockDeviceBuilder::new(size).pattern().build()
    }

    /// [`MockDevice::patterned`] が書き込むオフセット `offset` のバイト値。
    pub fn pattern_byte(offset: u64) -> u8 {
        // 単純な混ぜ方で十分。オフセットごとに違う値になればよい。
        let x = offset.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        ((x >> 33) ^ x) as u8
    }

    /// 中身への参照(期待値の作成用)。
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// 統計のスナップショット。
    pub fn stats(&self) -> MockStats {
        MockStats {
            read_calls: self.read_calls.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            failed_reads: self.failed_reads.load(Ordering::Relaxed),
            reopen_calls: self.reopen_calls.load(Ordering::Relaxed),
        }
    }

    /// 統計を 0 に戻す。パスごとの読み込み回数を測るときに使う。
    pub fn reset_stats(&self) {
        self.read_calls.store(0, Ordering::Relaxed);
        self.bytes_read.store(0, Ordering::Relaxed);
        self.failed_reads.store(0, Ordering::Relaxed);
        self.reopen_calls.store(0, Ordering::Relaxed);
    }

    /// 記録された読み込み `(offset, len)` の列。
    ///
    /// [`MockDeviceBuilder::record_reads`] を有効にしていない場合は空。
    pub fn reads(&self) -> Vec<(u64, usize)> {
        self.read_log
            .as_ref()
            .map(|log| log.lock().expect("read_log poisoned").clone())
            .unwrap_or_default()
    }

    /// 記録した読み込み履歴を消す。
    pub fn clear_reads(&self) {
        if let Some(log) = &self.read_log {
            log.lock().expect("read_log poisoned").clear();
        }
    }

    fn overlapping(&self, range: &Range<u64>) -> impl Iterator<Item = &Fault> {
        self.faults
            .iter()
            .filter(move |f| f.range.start < range.end && range.start < f.range.end)
    }
}

impl Device for MockDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        self.read_calls.fetch_add(1, Ordering::Relaxed);
        if let Some(log) = &self.read_log {
            log.lock()
                .expect("read_log poisoned")
                .push((offset, buf.len()));
        }

        let block_size = self.info.block_size;
        if self.require_alignment
            && (!is_aligned(offset, block_size) || !is_aligned(buf.len() as u64, block_size))
        {
            self.failed_reads.fetch_add(1, Ordering::Relaxed);
            return Err(DeviceError::Unaligned {
                offset,
                len: buf.len(),
                block_size,
            });
        }

        if let Some(max) = self.max_read_len
            && buf.len() > max
        {
            self.failed_reads.fetch_add(1, Ordering::Relaxed);
            return Err(DeviceError::Io {
                offset,
                len: buf.len(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("読み込み単位 {} が上限 {max} を超えている", buf.len()),
                ),
            });
        }

        let Some(want) = clamp_read(offset, buf.len(), self.len()) else {
            return Ok(0);
        };
        let range = offset..offset + want as u64;

        if let Some(d) = self.read_delay {
            std::thread::sleep(d);
        }

        // 低速領域の遅延を先に適用してから、不良判定を行う。
        for fault in self.overlapping(&range) {
            if let FaultKind::Slow { delay } = &fault.kind {
                std::thread::sleep(*delay);
            }
        }
        for fault in self.overlapping(&range) {
            match &fault.kind {
                FaultKind::Hard => {
                    self.failed_reads.fetch_add(1, Ordering::Relaxed);
                    return Err(DeviceError::Media { offset, len: want });
                }
                FaultKind::Transient {
                    fail_count,
                    attempts,
                } => {
                    let seen = attempts.fetch_add(1, Ordering::Relaxed);
                    if seen < *fail_count {
                        self.failed_reads.fetch_add(1, Ordering::Relaxed);
                        return Err(DeviceError::Media { offset, len: want });
                    }
                }
                FaultKind::UntilReopen { healed } => {
                    if !healed.load(Ordering::Relaxed) {
                        self.failed_reads.fetch_add(1, Ordering::Relaxed);
                        return Err(DeviceError::Media { offset, len: want });
                    }
                }
                FaultKind::Slow { .. } => {}
            }
        }

        let start = offset as usize;
        buf[..want].copy_from_slice(&self.data[start..start + want]);
        self.bytes_read.fetch_add(want as u64, Ordering::Relaxed);
        Ok(want)
    }

    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn block_size(&self) -> u32 {
        self.info.block_size
    }

    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn reopen(&self) -> Result<bool> {
        self.reopen_calls.fetch_add(1, Ordering::Relaxed);
        for fault in &self.faults {
            if let FaultKind::UntilReopen { healed } = &fault.kind {
                healed.store(true, Ordering::Relaxed);
            }
        }
        Ok(true)
    }
}

/// [`MockDevice`] のビルダ。
#[derive(Debug)]
pub struct MockDeviceBuilder {
    size: u64,
    block_size: u32,
    data: Option<Vec<u8>>,
    pattern: bool,
    faults: Vec<Fault>,
    read_delay: Option<Duration>,
    require_alignment: bool,
    max_read_len: Option<usize>,
    record_reads: bool,
    name: String,
    removable: bool,
    is_system_disk: bool,
}

impl MockDeviceBuilder {
    /// `size` バイトのデバイスを組み立てるビルダ。
    pub fn new(size: u64) -> Self {
        Self {
            size,
            block_size: 512,
            data: None,
            pattern: false,
            faults: Vec::new(),
            read_delay: None,
            require_alignment: false,
            max_read_len: None,
            record_reads: false,
            name: "mock".to_string(),
            removable: true,
            is_system_disk: false,
        }
    }

    /// 中身を指定する。デバイスサイズはこのデータ長になる。
    pub fn data(mut self, data: Vec<u8>) -> Self {
        self.size = data.len() as u64;
        self.data = Some(data);
        self
    }

    /// 決定的なパターンで埋める([`MockDevice::pattern_byte`])。
    pub fn pattern(mut self) -> Self {
        self.pattern = true;
        self
    }

    /// 論理ブロックサイズ。既定は 512。
    pub fn block_size(mut self, block_size: u32) -> Self {
        assert!(block_size > 0, "block_size は 1 以上");
        self.block_size = block_size;
        self
    }

    /// 恒久的な不良領域。この範囲に触れる読み込みは常に [`DeviceError::Media`]。
    pub fn bad_range(mut self, offset: u64, len: u64) -> Self {
        self.faults.push(Fault {
            range: offset..offset + len,
            kind: FaultKind::Hard,
        });
        self
    }

    /// `fail_count` 回失敗したあと成功する領域。
    pub fn transient_range(mut self, offset: u64, len: u64, fail_count: u32) -> Self {
        self.faults.push(Fault {
            range: offset..offset + len,
            kind: FaultKind::Transient {
                fail_count,
                attempts: AtomicU32::new(0),
            },
        });
        self
    }

    /// 読めるが遅い領域。
    pub fn slow_range(mut self, offset: u64, len: u64, delay: Duration) -> Self {
        self.faults.push(Fault {
            range: offset..offset + len,
            kind: FaultKind::Slow { delay },
        });
        self
    }

    /// [`Device::reopen`] を呼ぶまで失敗し続ける領域。
    ///
    /// USB コントローラが一時的に固まり、ハンドルを開き直すと復帰する挙動を模す。
    pub fn stuck_until_reopen(mut self, offset: u64, len: u64) -> Self {
        self.faults.push(Fault {
            range: offset..offset + len,
            kind: FaultKind::UntilReopen {
                healed: AtomicBool::new(false),
            },
        });
        self
    }

    /// 全読み込みに一律で挟む遅延。
    pub fn read_delay(mut self, delay: Duration) -> Self {
        self.read_delay = Some(delay);
        self
    }

    /// セクタ境界に整列していない読み込みを拒否する(Windows 非バッファIO 相当)。
    pub fn require_alignment(mut self, require: bool) -> Self {
        self.require_alignment = require;
        self
    }

    /// 1回の読み込みの上限。超える要求はエラーにする。
    pub fn max_read_len(mut self, max: usize) -> Self {
        self.max_read_len = Some(max);
        self
    }

    /// 読み込み `(offset, len)` の履歴を記録する。
    pub fn record_reads(mut self, record: bool) -> Self {
        self.record_reads = record;
        self
    }

    /// 表示名。
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// リムーバブル扱いにするか。既定は真。
    pub fn removable(mut self, removable: bool) -> Self {
        self.removable = removable;
        self
    }

    /// 起動ディスク扱いにするか(選択不可判定のテスト用)。既定は偽。
    pub fn system_disk(mut self, is_system_disk: bool) -> Self {
        self.is_system_disk = is_system_disk;
        self
    }

    /// 組み立てる。
    pub fn build(self) -> MockDevice {
        let data = match self.data {
            Some(d) => d,
            None if self.pattern => (0..self.size).map(MockDevice::pattern_byte).collect(),
            None => vec![0u8; self.size as usize],
        };

        let mut info = DeviceInfo::new(
            format!("mock:{}", self.name),
            self.name,
            DeviceKind::Mock,
            data.len() as u64,
            self.block_size,
        );
        info.removable = self.removable;
        info.is_system_disk = self.is_system_disk;

        MockDevice {
            data,
            info,
            faults: self.faults,
            read_delay: self.read_delay,
            require_alignment: self.require_alignment,
            max_read_len: self.max_read_len,
            read_calls: AtomicU64::new(0),
            bytes_read: AtomicU64::new(0),
            failed_reads: AtomicU64::new(0),
            reopen_calls: AtomicU64::new(0),
            read_log: self.record_reads.then(|| Mutex::new(Vec::new())),
        }
    }
}
