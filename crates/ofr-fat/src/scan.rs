//! ボリューム全体の走査。
//!
//! 手順は 2 段階:
//!
//! 1. ルートディレクトリから普通にツリーを辿る。ここで生きている項目と、
//!    ディレクトリに残っている削除済みエントリを拾う
//! 2. データ領域を頭から舐めて、`.` と `..` で始まるクラスタ(= ディレクトリの
//!    先頭)を探す。ルートから辿れなかったものは `Lost+Found` の下にぶら下げる
//!
//! 2 で先に見つけた枝の親が、後から 1 の側で見つかることがある。その場合は
//! [`FileTree::reparent`](ofr_fs::FileTree::reparent) で枝ごと正しい位置へ移し、
//! 名前も本来のものに直す。

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use ofr_device::Device;
use ofr_fs::fat::Fat32Table;
use ofr_fs::{
    EntryId, EntryKind, EntryQuality, EntryStatus, Extent, FileSystem, FileTree, FsKind,
    RecoveredEntry, Result, ScanOptions, ScanPhase, ScanProgress, ScanProgressFn, VolumeInfo,
};

use crate::bpb::Fat32Bpb;
use crate::dir::{self, DirEntry};

/// FAT32 の有効ビット。上位 4bit は予約。
const FAT32_MASK: u32 = 0x0FFF_FFFF;
/// 1 ディレクトリで辿るクラスタ数の上限。
const MAX_DIR_CLUSTERS: usize = 65_536;
/// チェーンが失われたディレクトリを、連続配置と仮定して辿る上限。
const MAX_ASSUMED_DIR_CLUSTERS: usize = 16;
/// 1 ファイルで辿るクラスタ数の上限。
const MAX_FILE_CLUSTERS: usize = 1 << 22;

/// FAT32 ボリューム。
pub struct Fat32Fs<'a> {
    device: &'a dyn Device,
    bpb: Fat32Bpb,
    fat: Fat32Table<'a>,
    volume: VolumeInfo,
}

impl<'a> Fat32Fs<'a> {
    /// ボリュームを開く。ブートセクタが壊れていればバックアップと推定で補う。
    pub fn open(device: &'a dyn Device) -> Result<Self> {
        let bpb = Fat32Bpb::probe(device)?;

        let fat_offset = bpb.fat_offset(0);
        let fat_bytes = bpb.fat_bytes().min(device.len().saturating_sub(fat_offset));
        let fat = Fat32Table::new(device, fat_offset, fat_bytes, FAT32_MASK);

        let mut notes = bpb.notes.clone();
        if bpb.total_bytes() > device.len() {
            notes.push(format!(
                "ブートセクタはボリュームを {} バイトと言っているが、デバイスは {} バイトしかない。\
                 末尾は切り詰めて扱う",
                bpb.total_bytes(),
                device.len()
            ));
        }

        let volume = VolumeInfo {
            kind: FsKind::Fat32,
            label: root_label(device, &bpb).or_else(|| bpb.volume_label.clone()),
            serial: bpb.volume_serial,
            bytes_per_sector: bpb.bytes_per_sector,
            bytes_per_cluster: bpb.cluster_size(),
            cluster_count: bpb.cluster_count(),
            total_bytes: bpb.total_bytes().min(device.len()),
            data_offset: bpb.data_offset(),
            boot_source: bpb.source,
            notes,
        };

        Ok(Self {
            device,
            bpb,
            fat,
            volume,
        })
    }

    /// 先頭セクタが FAT32 のブートセクタか(素早い判定)。
    ///
    /// 偽でも [`Fat32Fs::open`] は成功しうる(バックアップや推定で開けるため)。
    pub fn probe(device: &dyn Device) -> bool {
        let mut sector = vec![0u8; 512];
        device.read_exact_at(0, &mut sector).is_ok() && Fat32Bpb::parse(&sector).is_some()
    }

    /// ブートセクタから読んだジオメトリ。
    pub fn bpb(&self) -> &Fat32Bpb {
        &self.bpb
    }

    /// クラスタ 1 個を読む。読めなければ `None`。
    fn read_cluster(&self, cluster: u32) -> Option<Vec<u8>> {
        let size = self.bpb.cluster_size() as usize;
        let offset = self.bpb.cluster_offset(cluster);
        if offset.saturating_add(size as u64) > self.device.len() {
            return None;
        }
        let mut buf = vec![0u8; size];
        self.device.read_exact_at(offset, &mut buf).ok()?;
        Some(buf)
    }

    fn is_valid_cluster(&self, cluster: u32) -> bool {
        cluster >= 2 && cluster <= self.bpb.last_cluster()
    }

    /// ディレクトリが使っているクラスタを並べる。
    ///
    /// FAT チェーンが生きていればそれを使う。削除済み・孤立ディレクトリは
    /// チェーンが解放されているので、ディレクトリらしいクラスタが続く限り
    /// 連続配置とみなして辿る。
    fn directory_clusters(&self, first: u32, chain_lost: bool) -> Vec<u32> {
        if !chain_lost {
            let chain = self.fat.chain(first, MAX_DIR_CLUSTERS);
            if !chain.broken && !chain.clusters.is_empty() {
                return chain.clusters;
            }
        }

        // 連続配置を仮定して辿る。ディレクトリの終端 (先頭バイト 0x00 のエントリ)
        // が出たらそこで止める。止めないと、隣に置かれた別のディレクトリの
        // クラスタまで自分の中身として取り込んでしまう。
        let mut clusters = Vec::new();
        let mut next = first;
        while clusters.len() < MAX_ASSUMED_DIR_CLUSTERS && self.is_valid_cluster(next) {
            // 既に他のファイルに割り当てられているクラスタは続きではない。
            if !clusters.is_empty() && !self.fat.is_free(next) && !chain_lost {
                break;
            }
            let Some(data) = self.read_cluster(next) else {
                break;
            };
            // 2 つ目以降が `.` / `..` で始まっていれば、それは別のディレクトリの先頭。
            if !clusters.is_empty()
                && (dir::looks_like_directory(&data) || !dir::looks_like_directory_data(&data))
            {
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

    /// ファイル本体の位置を割り出す。
    fn file_extents(&self, first: u32, size: u64, chain_lost: bool) -> (Vec<Extent>, EntryQuality) {
        let mut quality = EntryQuality::default();
        let cluster_size = self.bpb.cluster_size() as u64;
        if size == 0 {
            return (Vec::new(), quality);
        }
        if !self.is_valid_cluster(first) || cluster_size == 0 {
            quality.truncated = true;
            return (Vec::new(), quality);
        }

        let needed = size.div_ceil(cluster_size).min(MAX_FILE_CLUSTERS as u64) as usize;
        let mut clusters = if chain_lost {
            Vec::new()
        } else {
            let chain = self.fat.chain(first, needed);
            if chain.broken && chain.clusters.len() < needed {
                Vec::new()
            } else {
                chain.clusters
            }
        };

        if clusters.len() < needed {
            // 削除でチェーンが解放されている場合の主経路。開始クラスタから
            // 連続配置と仮定して回収する(PLAN.md 5.3)。断片化していたファイルは
            // ここで壊れるので、修復モジュール (Phase 5) 行きになる。
            quality.contiguous_assumed = true;
            let mut next = clusters.last().map_or(first, |c| c + 1);
            while clusters.len() < needed && self.is_valid_cluster(next) {
                clusters.push(next);
                next += 1;
            }
        }

        if quality.contiguous_assumed {
            quality.conflicting_clusters = clusters
                .iter()
                .filter(|&&c| !self.fat.is_free(c))
                .count()
                .min(u32::MAX as usize) as u32;
        }
        if clusters.len() < needed {
            quality.truncated = true;
        }

        (self.merge_runs(&clusters), quality)
    }

    /// 連番のクラスタをまとめて領域にする。
    fn merge_runs(&self, clusters: &[u32]) -> Vec<Extent> {
        let cluster_size = self.bpb.cluster_size() as u64;
        let device_len = self.device.len();
        let mut extents: Vec<Extent> = Vec::new();

        for &cluster in clusters {
            let offset = self.bpb.cluster_offset(cluster);
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

impl FileSystem for Fat32Fs<'_> {
    fn volume(&self) -> &VolumeInfo {
        &self.volume
    }

    fn scan(&self, options: &ScanOptions, progress: Option<ScanProgressFn>) -> Result<FileTree> {
        Walker::new(self, options, progress).run()
    }
}

/// ボリュームラベルはルートディレクトリの volume-id エントリのほうが新しい。
fn root_label(device: &dyn Device, bpb: &Fat32Bpb) -> Option<String> {
    let size = bpb.cluster_size() as usize;
    let offset = bpb.cluster_offset(bpb.root_cluster);
    if offset.saturating_add(size as u64) > device.len() {
        return None;
    }
    let mut buf = vec![0u8; size];
    device.read_exact_at(offset, &mut buf).ok()?;
    dir::parse_directory(&buf, 0).volume_label
}

/// 走査待ちのディレクトリ。
struct Pending {
    cluster: u32,
    parent: Option<EntryId>,
    depth: u32,
    status: EntryStatus,
    /// FAT チェーンが失われている前提で辿るか。
    chain_lost: bool,
}

struct Walker<'a, 'dev> {
    fs: &'a Fat32Fs<'dev>,
    options: &'a ScanOptions,
    progress: Option<ScanProgressFn>,
    tree: FileTree,
    queue: VecDeque<Pending>,
    /// 走査済みのディレクトリクラスタ。
    visited: HashSet<u32>,
    /// 孤立ディレクトリとして拾った枝(あとで本来の親へ移す候補)。
    orphan_roots: HashMap<u32, EntryId>,
    lost_found: Option<EntryId>,
    started: Instant,
    last_progress: Instant,
}

impl<'a, 'dev> Walker<'a, 'dev> {
    fn new(
        fs: &'a Fat32Fs<'dev>,
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
        let root = self.fs.bpb.root_cluster;
        if self.fs.is_valid_cluster(root) {
            self.queue.push_back(Pending {
                cluster: root,
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
                "FAT 表の {} か所を読めなかった。チェーンを辿れなかったファイルは\
                 連続配置と仮定して回収している",
                self.fs.fat.read_failures()
            ));
        }
        Ok(self.tree)
    }

    /// 打ち切るべきか(キャンセル or 上限)。
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

        let clusters = self
            .fs
            .directory_clusters(pending.cluster, pending.chain_lost);
        self.tree.stats.clusters_scanned += clusters.len() as u64;

        for (index, &cluster) in clusters.iter().enumerate() {
            if self.stop() {
                return;
            }
            let Some(data) = self.fs.read_cluster(cluster) else {
                self.tree
                    .warn(format!("ディレクトリのクラスタ {cluster} を読めない"));
                continue;
            };
            self.visited.insert(cluster);

            let base = (index as u64) * self.fs.bpb.cluster_size() as u64;
            let contents = dir::parse_directory(&data, base);
            for entry in contents.entries {
                if self.stop() {
                    return;
                }
                if entry.deleted && !self.options.deleted {
                    continue;
                }
                self.add_entry(entry, &pending);
            }
        }
        self.report(ScanPhase::Directories, 0, self.fs.volume.total_bytes);
    }

    fn add_entry(&mut self, entry: DirEntry, parent: &Pending) {
        let status = if entry.deleted {
            EntryStatus::Deleted
        } else {
            parent.status
        };
        // 削除されるとその項目の FAT チェーンは解放される。親が既にチェーンを
        // 失っている場合(削除済み・孤立ディレクトリ)も同じ扱いにする。
        let chain_lost = entry.deleted || parent.chain_lost;

        if entry.is_dir() {
            self.add_directory(entry, parent, status, chain_lost);
        } else {
            self.add_file(entry, parent, status, chain_lost);
        }
    }

    fn add_directory(
        &mut self,
        entry: DirEntry,
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
                node.quality.name_partial = entry.name_partial;
                if status == EntryStatus::Deleted {
                    node.status = EntryStatus::Deleted;
                }
            }
            return;
        }

        let mut node = RecoveredEntry::new(entry.name, EntryKind::Dir, status);
        node.times = entry.times;
        node.first_cluster = Some(cluster);
        node.quality.name_partial = entry.name_partial;
        if !self.fs.is_valid_cluster(cluster) {
            node.status = EntryStatus::Damaged;
        }
        let id = self.tree.push(node, parent.parent);

        if self.fs.is_valid_cluster(cluster) && !self.visited.contains(&cluster) {
            self.queue.push_back(Pending {
                cluster,
                parent: Some(id),
                depth: parent.depth + 1,
                status,
                chain_lost,
            });
        }
    }

    fn add_file(
        &mut self,
        entry: DirEntry,
        parent: &Pending,
        status: EntryStatus,
        chain_lost: bool,
    ) {
        let size = entry.size as u64;
        let (extents, quality) = self.fs.file_extents(entry.first_cluster, size, chain_lost);

        let mut node = RecoveredEntry::new(entry.name, EntryKind::File, status);
        node.size = size;
        node.times = entry.times;
        node.first_cluster = Some(entry.first_cluster);
        node.extents = extents;
        node.quality = quality;
        node.quality.name_partial |= entry.name_partial;
        if node.quality.truncated && node.status == EntryStatus::Intact {
            node.status = EntryStatus::Damaged;
        }
        self.tree.push(node, parent.parent);
    }

    /// データ領域を舐めて、ルートから辿れないディレクトリを拾う。
    fn scan_orphans(&mut self) {
        let cluster_size = self.fs.bpb.cluster_size() as u64;
        let last = self.fs.bpb.last_cluster();
        if cluster_size == 0 || last < 2 {
            return;
        }

        let per_chunk = (self.options.scan_chunk / cluster_size).max(1);
        let mut buf = vec![0u8; (per_chunk * cluster_size) as usize];
        let mut cluster = 2u32;

        while cluster <= last && !self.stop() {
            let count = per_chunk.min((last - cluster + 1) as u64) as usize;
            let offset = self.fs.bpb.cluster_offset(cluster);
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
                // 読めない範囲は諦めて次へ。1 か所で止めない(PLAN.md 5.2)。
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
                let head = match buf.get(start..start + cluster_size as usize) {
                    Some(head) => head,
                    None => break,
                };
                if dir::looks_like_directory(head) {
                    self.adopt_orphan(current);
                }
            }

            cluster = cluster.saturating_add(count as u32);
            self.report(
                ScanPhase::Orphans,
                self.fs.bpb.cluster_offset(cluster),
                self.fs.volume.total_bytes,
            );
        }
    }

    /// 孤立ディレクトリを `Lost+Found` の下にぶら下げて、中身を走査する。
    fn adopt_orphan(&mut self, cluster: u32) {
        let parent = self.lost_found();
        let mut node = RecoveredEntry::new(
            format!("dir_{cluster:08}"),
            EntryKind::Dir,
            EntryStatus::Orphaned,
        );
        node.first_cluster = Some(cluster);
        // 名前は親ディレクトリのエントリ側にあるので、この時点では分からない。
        node.quality.name_partial = true;
        let id = self.tree.push(node, Some(parent));
        self.orphan_roots.insert(cluster, id);

        self.queue.push_back(Pending {
            cluster,
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
