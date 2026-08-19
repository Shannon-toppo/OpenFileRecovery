//! ボリューム全体の走査。
//!
//! 段取りは FAT32 版と同じ(ルートから辿る → データ領域を舐めて孤立
//! ディレクトリを拾う)。exFAT ならではの点は 2 つ:
//!
//! - `NoFatChain` が立っているファイルは連続配置が確定しているので、削除後でも
//!   「仮定」ではなく確定として回収できる
//! - アロケーションビットマップを見れば、拾ったクラスタが今も誰かに使われて
//!   いるかが分かる。削除済みファイルの領域が上書きされている可能性を
//!   [`EntryQuality::conflicting_clusters`](ofr_fs::EntryQuality) で伝える

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use ofr_device::Device;
use ofr_fs::fat::Fat32Table;
use ofr_fs::{
    EntryId, EntryKind, EntryQuality, EntryStatus, Extent, FileSystem, FileTree, FsKind,
    RecoveredEntry, Result, ScanOptions, ScanPhase, ScanProgress, ScanProgressFn, VolumeInfo,
};

use crate::boot::ExfatBoot;
use crate::dir::{self, ExfatEntry};

/// exFAT の FAT は 32bit 全部を使う。
const EXFAT_MASK: u32 = 0xFFFF_FFFF;
/// 1 ディレクトリで辿るクラスタ数の上限。
const MAX_DIR_CLUSTERS: usize = 65_536;
/// 大きさの分からないディレクトリを、連続配置と仮定して辿る上限。
const MAX_ASSUMED_DIR_CLUSTERS: usize = 16;
/// 1 ファイルで辿るクラスタ数の上限。
const MAX_FILE_CLUSTERS: usize = 1 << 22;
/// メモリに読み込むビットマップの上限(これを超える巨大ボリュームでは使わない)。
const MAX_BITMAP_BYTES: u64 = 16 << 20;

/// アロケーションビットマップ。クラスタ 2 が bit0。
struct Bitmap {
    bits: Vec<u8>,
}

impl Bitmap {
    fn is_allocated(&self, cluster: u32) -> bool {
        let index = (cluster as usize).saturating_sub(2);
        match self.bits.get(index / 8) {
            Some(byte) => byte & (1 << (index % 8)) != 0,
            None => false,
        }
    }
}

/// exFAT ボリューム。
pub struct ExfatFs<'a> {
    device: &'a dyn Device,
    boot: ExfatBoot,
    fat: Fat32Table<'a>,
    bitmap: Option<Bitmap>,
    volume: VolumeInfo,
}

impl<'a> ExfatFs<'a> {
    /// ボリュームを開く。
    pub fn open(device: &'a dyn Device) -> Result<Self> {
        let boot = ExfatBoot::probe(device)?;

        let fat_offset = boot.fat_offset(0);
        let fat_bytes = boot
            .fat_bytes()
            .min(device.len().saturating_sub(fat_offset));
        let fat = Fat32Table::new(device, fat_offset, fat_bytes, EXFAT_MASK);

        let mut notes = boot.notes.clone();
        if boot.total_bytes() > device.len() {
            notes.push(format!(
                "ブートセクタはボリュームを {} バイトと言っているが、デバイスは {} バイトしかない。\
                 末尾は切り詰めて扱う",
                boot.total_bytes(),
                device.len()
            ));
        }

        let mut fs = Self {
            device,
            volume: VolumeInfo {
                kind: FsKind::ExFat,
                label: None,
                serial: boot.volume_serial,
                bytes_per_sector: boot.bytes_per_sector,
                bytes_per_cluster: boot.cluster_size(),
                cluster_count: boot.cluster_count,
                total_bytes: boot.total_bytes().min(device.len()),
                data_offset: boot.heap_offset(),
                boot_source: boot.source,
                notes,
            },
            boot,
            fat,
            bitmap: None,
        };

        // ルートからラベルとビットマップの位置を拾う。どちらも無くても走査はできる。
        let root = fs.read_directory(fs.boot.root_cluster, 0, false);
        let contents = dir::parse_directory(&root, 0);
        fs.volume.label = contents.volume_label;
        if let Some((cluster, len)) = contents.bitmap {
            fs.bitmap = fs.load_bitmap(cluster, len);
        }
        if fs.bitmap.is_none() {
            fs.volume.notes.push(
                "アロケーションビットマップを読めなかった。クラスタの使用状況は判定しない".into(),
            );
        }

        Ok(fs)
    }

    /// 先頭が exFAT のブートセクタか(素早い判定)。
    pub fn probe(device: &dyn Device) -> bool {
        let mut sector = vec![0u8; 512];
        device.read_exact_at(0, &mut sector).is_ok() && ExfatBoot::parse(&sector).is_some()
    }

    /// ブートセクタから読んだジオメトリ。
    pub fn boot(&self) -> &ExfatBoot {
        &self.boot
    }

    fn load_bitmap(&self, first_cluster: u32, len: u64) -> Option<Bitmap> {
        if !self.is_valid_cluster(first_cluster) || len == 0 || len > MAX_BITMAP_BYTES {
            return None;
        }
        let cluster_size = self.boot.cluster_size() as u64;
        let needed = len.div_ceil(cluster_size) as usize;
        let chain = self.fat.chain(first_cluster, needed);
        let clusters = if chain.clusters.len() >= needed {
            chain.clusters
        } else {
            (first_cluster..)
                .take(needed)
                .take_while(|&c| self.is_valid_cluster(c))
                .collect()
        };

        let mut bits = Vec::with_capacity(len as usize);
        for cluster in clusters {
            let want = (len - bits.len() as u64).min(cluster_size) as usize;
            let mut buf = vec![0u8; want];
            if self
                .device
                .read_exact_at(self.boot.cluster_offset(cluster), &mut buf)
                .is_err()
            {
                buf.fill(0);
            }
            bits.extend_from_slice(&buf);
            if bits.len() as u64 >= len {
                break;
            }
        }
        (!bits.is_empty()).then_some(Bitmap { bits })
    }

    fn is_valid_cluster(&self, cluster: u32) -> bool {
        cluster >= 2 && cluster <= self.boot.last_cluster()
    }

    fn read_cluster(&self, cluster: u32) -> Option<Vec<u8>> {
        let size = self.boot.cluster_size() as usize;
        let offset = self.boot.cluster_offset(cluster);
        if offset.saturating_add(size as u64) > self.device.len() {
            return None;
        }
        let mut buf = vec![0u8; size];
        self.device.read_exact_at(offset, &mut buf).ok()?;
        Some(buf)
    }

    /// ディレクトリのクラスタを並べる。
    ///
    /// `size` が分かっていればその分だけ、分からなければ(孤立ディレクトリ)
    /// ディレクトリらしいクラスタが続く限り辿る。
    fn directory_clusters(&self, first: u32, size: u64, contiguous: bool) -> Vec<u32> {
        let cluster_size = self.boot.cluster_size() as u64;
        if cluster_size == 0 || !self.is_valid_cluster(first) {
            return Vec::new();
        }

        if !contiguous {
            let max = if size > 0 {
                size.div_ceil(cluster_size).min(MAX_DIR_CLUSTERS as u64) as usize
            } else {
                MAX_DIR_CLUSTERS
            };
            let chain = self.fat.chain(first, max);
            if !chain.broken && !chain.clusters.is_empty() {
                return chain.clusters;
            }
        }

        if size > 0 {
            let needed = size.div_ceil(cluster_size).min(MAX_DIR_CLUSTERS as u64) as usize;
            return (first..)
                .take(needed)
                .take_while(|&c| self.is_valid_cluster(c))
                .collect();
        }

        // 大きさが分からないので、ディレクトリの終端 (型バイト 0x00 のエントリ)
        // まで辿る。止めないと、隣に置かれた別のディレクトリのクラスタまで
        // 自分の中身として取り込んでしまう。
        let mut clusters = Vec::new();
        let mut next = first;
        while clusters.len() < MAX_ASSUMED_DIR_CLUSTERS && self.is_valid_cluster(next) {
            let Some(data) = self.read_cluster(next) else {
                break;
            };
            if !clusters.is_empty() && !dir::looks_like_directory_data(&data) {
                break;
            }
            clusters.push(next);
            if dir::has_end_marker(&data) {
                break;
            }
            next += 1;
        }
        if clusters.is_empty() {
            clusters.push(first);
        }
        clusters
    }

    /// ディレクトリの中身を読む。
    fn read_directory(&self, first: u32, size: u64, contiguous: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for cluster in self.directory_clusters(first, size, contiguous) {
            match self.read_cluster(cluster) {
                Some(data) => out.extend_from_slice(&data),
                None => break,
            }
        }
        out
    }

    /// ファイル本体の位置を割り出す。
    fn file_extents(&self, entry: &ExfatEntry, chain_lost: bool) -> (Vec<Extent>, EntryQuality) {
        let mut quality = EntryQuality::default();
        let cluster_size = self.boot.cluster_size() as u64;
        if entry.size == 0 {
            return (Vec::new(), quality);
        }
        if !self.is_valid_cluster(entry.first_cluster) || cluster_size == 0 {
            quality.truncated = true;
            return (Vec::new(), quality);
        }

        let needed = entry
            .size
            .div_ceil(cluster_size)
            .min(MAX_FILE_CLUSTERS as u64) as usize;

        let mut clusters = Vec::new();
        if !entry.no_fat_chain && !chain_lost {
            let chain = self.fat.chain(entry.first_cluster, needed);
            if !chain.broken || chain.clusters.len() >= needed {
                clusters = chain.clusters;
            }
        }

        if clusters.len() < needed {
            // NoFatChain のファイルは連続配置が確定しているので仮定ではない。
            // それ以外は「削除でチェーンが解放された」ケースとして連続配置を仮定する。
            if !entry.no_fat_chain {
                quality.contiguous_assumed = true;
            }
            let mut next = clusters.last().map_or(entry.first_cluster, |c| c + 1);
            while clusters.len() < needed && self.is_valid_cluster(next) {
                clusters.push(next);
                next += 1;
            }
        }

        // 削除済みの領域が今も使用中なら、上書きされている可能性がある。
        if (entry.deleted || chain_lost)
            && let Some(bitmap) = &self.bitmap
        {
            quality.conflicting_clusters = clusters
                .iter()
                .filter(|&&c| bitmap.is_allocated(c))
                .count()
                .min(u32::MAX as usize) as u32;
        }
        if clusters.len() < needed {
            quality.truncated = true;
        }

        (self.merge_runs(&clusters), quality)
    }

    fn merge_runs(&self, clusters: &[u32]) -> Vec<Extent> {
        let cluster_size = self.boot.cluster_size() as u64;
        let device_len = self.device.len();
        let mut extents: Vec<Extent> = Vec::new();

        for &cluster in clusters {
            let offset = self.boot.cluster_offset(cluster);
            if offset >= device_len {
                continue;
            }
            let len = cluster_size.min(device_len - offset);
            match extents.last_mut() {
                Some(last) if last.end() == offset => last.len += len,
                _ => extents.push(Extent { offset, len }),
            }
        }
        extents
    }
}

impl FileSystem for ExfatFs<'_> {
    fn volume(&self) -> &VolumeInfo {
        &self.volume
    }

    fn scan(&self, options: &ScanOptions, progress: Option<ScanProgressFn>) -> Result<FileTree> {
        Walker::new(self, options, progress).run()
    }
}

/// 走査待ちのディレクトリ。
struct Pending {
    cluster: u32,
    size: u64,
    contiguous: bool,
    parent: Option<EntryId>,
    depth: u32,
    status: EntryStatus,
    chain_lost: bool,
}

struct Walker<'a, 'dev> {
    fs: &'a ExfatFs<'dev>,
    options: &'a ScanOptions,
    progress: Option<ScanProgressFn>,
    tree: FileTree,
    queue: VecDeque<Pending>,
    visited: HashSet<u32>,
    orphan_roots: HashMap<u32, EntryId>,
    lost_found: Option<EntryId>,
    started: Instant,
    last_progress: Instant,
}

impl<'a, 'dev> Walker<'a, 'dev> {
    fn new(
        fs: &'a ExfatFs<'dev>,
        options: &'a ScanOptions,
        progress: Option<ScanProgressFn>,
    ) -> Self {
        let now = Instant::now();
        Self {
            fs,
            options,
            progress,
            tree: FileTree::new(),
            queue: VecDeque::new(),
            visited: HashSet::new(),
            orphan_roots: HashMap::new(),
            lost_found: None,
            started: now,
            last_progress: now,
        }
    }

    fn run(mut self) -> Result<FileTree> {
        let root = self.fs.boot.root_cluster;
        if self.fs.is_valid_cluster(root) {
            self.queue.push_back(Pending {
                cluster: root,
                size: 0,
                contiguous: false,
                parent: None,
                depth: 0,
                status: EntryStatus::Intact,
                chain_lost: false,
            });
            self.drain_queue();
        } else {
            self.tree.warn(format!(
                "ルートクラスタ {root} が範囲外なので、ツリー走査を飛ばす"
            ));
        }

        if self.options.orphans && !self.stop() {
            self.scan_orphans();
        }

        self.tree.stats.cancelled = self.options.is_cancelled();
        self.tree.stats.elapsed = self.started.elapsed();
        if self.fs.fat.read_failures() > 0 {
            self.tree.warn(format!(
                "FAT 表の {} か所を読めなかった",
                self.fs.fat.read_failures()
            ));
        }
        Ok(self.tree)
    }

    fn stop(&self) -> bool {
        self.options.is_cancelled() || self.tree.len() >= self.options.max_entries
    }

    fn drain_queue(&mut self) {
        while let Some(pending) = self.queue.pop_front() {
            if self.stop() {
                break;
            }
            self.process_dir(pending);
        }
        if self.tree.len() >= self.options.max_entries {
            self.tree.stats.truncated = true;
        }
    }

    fn process_dir(&mut self, pending: Pending) {
        if !self.visited.insert(pending.cluster) {
            return;
        }
        if pending.depth > self.options.max_depth {
            self.tree.warn(format!(
                "深さ上限を超えたのでクラスタ {} で止めた",
                pending.cluster
            ));
            return;
        }

        let clusters =
            self.fs
                .directory_clusters(pending.cluster, pending.size, pending.contiguous);
        self.tree.stats.clusters_scanned += clusters.len() as u64;

        let mut data = Vec::new();
        for &cluster in &clusters {
            self.visited.insert(cluster);
            match self.fs.read_cluster(cluster) {
                Some(bytes) => data.extend_from_slice(&bytes),
                None => {
                    self.tree
                        .warn(format!("ディレクトリのクラスタ {cluster} を読めない"));
                    break;
                }
            }
        }
        // エントリセットはクラスタ境界をまたぐことがあるので、
        // クラスタごとではなくまとめて解析する。
        for entry in dir::parse_directory(&data, 0).entries {
            if self.stop() {
                return;
            }
            if entry.deleted && !self.options.deleted {
                continue;
            }
            self.add_entry(entry, &pending);
        }
        self.report(ScanPhase::Directories, 0, self.fs.volume.total_bytes);
    }

    fn add_entry(&mut self, entry: ExfatEntry, parent: &Pending) {
        let status = if entry.deleted {
            EntryStatus::Deleted
        } else {
            parent.status
        };
        let chain_lost = entry.deleted || parent.chain_lost;

        if entry.is_dir() {
            self.add_directory(entry, parent, status, chain_lost);
        } else {
            self.add_file(entry, parent, status, chain_lost);
        }
    }

    fn add_directory(
        &mut self,
        entry: ExfatEntry,
        parent: &Pending,
        status: EntryStatus,
        chain_lost: bool,
    ) {
        let cluster = entry.first_cluster;

        // 先に孤立ディレクトリとして拾っていた枝なら、名前と場所をここで直す。
        if let Some(id) = self.orphan_roots.remove(&cluster) {
            self.tree.rename(id, entry.name);
            if let Some(parent_id) = parent.parent {
                self.tree.reparent(id, parent_id);
            }
            if let Some(node) = self.tree.get_mut(id) {
                node.times = entry.times;
                node.quality.name_partial = false;
                if status == EntryStatus::Deleted {
                    node.status = EntryStatus::Deleted;
                }
            }
            return;
        }

        let mut node = RecoveredEntry::new(entry.name, EntryKind::Dir, status);
        node.times = entry.times;
        node.first_cluster = Some(cluster);
        if !entry.checksum_ok {
            node.status = EntryStatus::Damaged;
        }
        if !self.fs.is_valid_cluster(cluster) {
            node.status = EntryStatus::Damaged;
        }
        let id = self.tree.push(node, parent.parent);

        if self.fs.is_valid_cluster(cluster) && !self.visited.contains(&cluster) {
            self.queue.push_back(Pending {
                cluster,
                size: entry.size,
                contiguous: entry.no_fat_chain || chain_lost,
                parent: Some(id),
                depth: parent.depth + 1,
                status,
                chain_lost,
            });
        }
    }

    fn add_file(
        &mut self,
        entry: ExfatEntry,
        parent: &Pending,
        status: EntryStatus,
        chain_lost: bool,
    ) {
        let (extents, quality) = self.fs.file_extents(&entry, chain_lost);

        let mut node = RecoveredEntry::new(entry.name.clone(), EntryKind::File, status);
        node.size = entry.size;
        node.times = entry.times;
        node.first_cluster = Some(entry.first_cluster);
        node.extents = extents;
        node.quality = quality;
        if !entry.checksum_ok || (node.quality.truncated && node.status == EntryStatus::Intact) {
            node.status = EntryStatus::Damaged;
        }
        self.tree.push(node, parent.parent);
    }

    /// データ領域を舐めて、ルートから辿れないディレクトリを拾う。
    fn scan_orphans(&mut self) {
        let cluster_size = self.fs.boot.cluster_size() as u64;
        let last = self.fs.boot.last_cluster();
        if cluster_size == 0 || last < 2 {
            return;
        }

        let per_chunk = (self.options.scan_chunk / cluster_size).max(1);
        let mut buf = vec![0u8; (per_chunk * cluster_size) as usize];
        let mut cluster = 2u32;

        while cluster <= last && !self.stop() {
            let count = per_chunk.min((last - cluster + 1) as u64) as usize;
            let offset = self.fs.boot.cluster_offset(cluster);
            let want = (count as u64 * cluster_size)
                .min(self.fs.device.len().saturating_sub(offset)) as usize;
            if want == 0 {
                break;
            }

            if self
                .fs
                .device
                .read_exact_at(offset, &mut buf[..want])
                .is_err()
            {
                self.tree
                    .warn(format!("クラスタ {cluster} 付近の {want} バイトを読めない"));
                cluster = cluster.saturating_add(count as u32);
                continue;
            }

            for i in 0..count {
                let current = cluster + i as u32;
                self.tree.stats.clusters_scanned += 1;
                if self.visited.contains(&current) || self.stop() {
                    continue;
                }
                let start = i * cluster_size as usize;
                let Some(head) = buf.get(start..start + cluster_size as usize) else {
                    break;
                };
                if dir::looks_like_directory(head) {
                    self.adopt_orphan(current);
                }
            }

            cluster = cluster.saturating_add(count as u32);
            self.report(
                ScanPhase::Orphans,
                self.fs.boot.cluster_offset(cluster),
                self.fs.volume.total_bytes,
            );
        }
    }

    fn adopt_orphan(&mut self, cluster: u32) {
        let parent = self.lost_found();
        let mut node = RecoveredEntry::new(
            format!("dir_{cluster:08}"),
            EntryKind::Dir,
            EntryStatus::Orphaned,
        );
        node.first_cluster = Some(cluster);
        // exFAT のディレクトリは自分の名前を持たない(親のエントリ側にある)。
        node.quality.name_partial = true;
        let id = self.tree.push(node, Some(parent));
        self.orphan_roots.insert(cluster, id);

        self.queue.push_back(Pending {
            cluster,
            size: 0,
            contiguous: true,
            parent: Some(id),
            depth: 1,
            status: EntryStatus::Orphaned,
            chain_lost: true,
        });
        self.drain_queue();
    }

    fn lost_found(&mut self) -> EntryId {
        match self.lost_found {
            Some(id) => id,
            None => {
                let node = RecoveredEntry::new("Lost+Found", EntryKind::Dir, EntryStatus::Orphaned);
                let id = self.tree.push(node, None);
                self.lost_found = Some(id);
                id
            }
        }
    }

    fn report(&mut self, phase: ScanPhase, position: u64, total: u64) {
        if self.progress.is_none() || self.last_progress.elapsed() < self.options.progress_interval
        {
            return;
        }
        self.last_progress = Instant::now();
        let event = ScanProgress {
            phase,
            position,
            total,
            found: self.tree.len(),
            elapsed: self.started.elapsed(),
        };
        if let Some(f) = self.progress.as_mut() {
            f(&event);
        }
    }
}
