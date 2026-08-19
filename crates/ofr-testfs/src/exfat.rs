//! exFAT イメージの組み立て。
//!
//! FAT32 版と同じ考え方。削除は「エントリタイプの InUse ビット (bit7) を落として
//! アロケーションビットマップを解放する」、クイックフォーマットは「ビットマップと
//! ルートディレクトリを作り直す」で再現する。

use std::collections::HashMap;

use crate::tree::FsTree;
use crate::{TEST_DATE, TEST_TIME};

const BYTES_PER_SECTOR: u64 = 512;
const BYTES_PER_SECTOR_SHIFT: u8 = 9;
/// ブート領域(本体 12 セクタ + バックアップ 12 セクタ)より後ろに FAT を置く。
const FAT_OFFSET_SECTORS: u64 = 128;
const ENTRY_SIZE: usize = 32;
const UPCASE_BYTES: usize = 128 * 1024;
const EOC: u32 = 0xFFFF_FFFF;

/// exFAT イメージビルダ。
#[derive(Debug, Clone)]
pub struct ExfatImage {
    size: u64,
    cluster_size: u64,
    label: String,
    tree: FsTree,
    quick_formatted: bool,
}

impl ExfatImage {
    /// 指定サイズのボリュームを作る。
    pub fn new(size: u64) -> Self {
        Self {
            size: size / BYTES_PER_SECTOR * BYTES_PER_SECTOR,
            cluster_size: 4096,
            label: "OFRTEST".to_string(),
            tree: FsTree::new(),
            quick_formatted: false,
        }
    }

    /// クラスタサイズ(2 の冪、セクタサイズ以上)。
    pub fn cluster_size(mut self, size: u64) -> Self {
        assert!(size >= BYTES_PER_SECTOR && size.is_power_of_two());
        self.cluster_size = size;
        self
    }

    /// ボリュームラベル。
    pub fn label(mut self, label: &str) -> Self {
        self.label = label.to_string();
        self
    }

    /// 中身。
    pub fn tree(&mut self) -> &mut FsTree {
        &mut self.tree
    }

    /// 最後にクイックフォーマットする。
    pub fn quick_format(mut self) -> Self {
        self.quick_formatted = true;
        self
    }

    /// イメージを組み立てる。
    pub fn build(&self) -> Vec<u8> {
        let geometry = Geometry::new(self.size, self.cluster_size);
        let mut tree = self.tree.clone();

        let needs_filler = tree.nodes().iter().any(|n| n.fragmented);
        let filler = needs_filler.then(|| tree.file("/_FILLER.BIN", Vec::new()));

        let mut layout = Layout::new(&geometry);
        layout.assign(&mut tree, filler);

        let mut image = vec![0u8; geometry.volume_sectors as usize * BYTES_PER_SECTOR as usize];
        self.write_boot_region(&mut image, &geometry);
        write_fat(&mut image, &geometry, &layout, &tree);
        write_upcase_table(&mut image, &geometry, &layout);
        self.write_directories(&mut image, &geometry, &tree, &mut layout);
        write_file_data(&mut image, &geometry, &tree, &layout);
        write_bitmap(&mut image, &geometry, &layout, &tree);
        apply_deletions(&mut image, &geometry, &tree, &layout);

        if self.quick_formatted {
            self.quick_format_image(&mut image, &geometry, &layout);
        }
        image
    }

    fn write_boot_region(&self, image: &mut [u8], geometry: &Geometry) {
        let sector = BYTES_PER_SECTOR as usize;
        let mut boot = vec![0u8; sector];
        boot[0..3].copy_from_slice(&[0xEB, 0x76, 0x90]);
        boot[3..11].copy_from_slice(b"EXFAT   ");
        boot[64..72].copy_from_slice(&0u64.to_le_bytes()); // PartitionOffset
        boot[72..80].copy_from_slice(&geometry.volume_sectors.to_le_bytes());
        boot[80..84].copy_from_slice(&(geometry.fat_offset_sectors as u32).to_le_bytes());
        boot[84..88].copy_from_slice(&(geometry.fat_sectors as u32).to_le_bytes());
        boot[88..92].copy_from_slice(&(geometry.heap_offset_sectors as u32).to_le_bytes());
        boot[92..96].copy_from_slice(&(geometry.cluster_count as u32).to_le_bytes());
        boot[96..100].copy_from_slice(&2u32.to_le_bytes()); // ルートディレクトリのクラスタ
        boot[100..104].copy_from_slice(&0x1234_5678u32.to_le_bytes()); // ボリュームシリアル
        boot[104..106].copy_from_slice(&0x0100u16.to_le_bytes()); // FS リビジョン 1.00
        boot[108] = BYTES_PER_SECTOR_SHIFT;
        boot[109] = geometry.sectors_per_cluster_shift;
        boot[110] = 1; // FAT は 1 本
        boot[111] = 0x80; // ドライブ番号
        boot[112] = 0; // 使用率不明
        boot[510] = 0x55;
        boot[511] = 0xAA;

        // ルートのクラスタは後で確定するので、ここでは 2 としておき、
        // レイアウト側でルートをクラスタ 2 に固定している。
        image[0..sector].copy_from_slice(&boot);

        // 拡張ブートセクタ (1〜8) の署名。
        for lba in 1..9usize {
            let at = lba * sector;
            image[at + sector - 4..at + sector].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
        }

        // セクタ 11 はブート領域のチェックサム。
        let checksum = boot_checksum(&image[0..11 * sector]);
        let at = 11 * sector;
        for i in 0..sector / 4 {
            image[at + i * 4..at + i * 4 + 4].copy_from_slice(&checksum.to_le_bytes());
        }

        // 12〜23 はバックアップ。
        let (main, backup) = image.split_at_mut(12 * sector);
        backup[..12 * sector].copy_from_slice(&main[..12 * sector]);
    }

    fn write_directories(
        &self,
        image: &mut [u8],
        geometry: &Geometry,
        tree: &FsTree,
        layout: &mut Layout,
    ) {
        let dirs: Vec<usize> = (0..tree.nodes().len())
            .filter(|&id| tree.node(id).is_dir)
            .collect();

        for id in dirs {
            let mut data = Vec::new();
            if id == 0 {
                data.extend(self.system_entries(geometry, layout));
            }
            for &child in &tree.node(id).children {
                let node = tree.node(child);
                let clusters = layout.clusters.get(&child).cloned().unwrap_or_default();
                let contiguous = is_contiguous(&clusters);
                let length = if node.is_dir {
                    clusters.len() as u64 * geometry.cluster_size
                } else {
                    node.data.len() as u64
                };
                let entries = build_entry_set(
                    &node.name,
                    node.is_dir,
                    clusters.first().copied().unwrap_or(0),
                    length,
                    contiguous,
                );
                let offset_in_dir = data.len() as u64;
                let count = entries.len() / ENTRY_SIZE;
                data.extend(entries);
                layout.entry_positions.insert(
                    child,
                    EntryPosition {
                        dir: id,
                        offset_in_dir,
                        count,
                    },
                );
            }

            let clusters = layout.clusters.get(&id).cloned().unwrap_or_default();
            write_clusters(image, geometry, &clusters, &data);
        }
    }

    /// ルートディレクトリの先頭に置く 3 つのシステムエントリ。
    fn system_entries(&self, geometry: &Geometry, layout: &Layout) -> Vec<u8> {
        let mut out = Vec::new();

        let mut bitmap = vec![0u8; ENTRY_SIZE];
        bitmap[0] = 0x81;
        bitmap[20..24].copy_from_slice(&layout.bitmap_cluster.to_le_bytes());
        bitmap[24..32].copy_from_slice(&geometry.bitmap_bytes().to_le_bytes());
        out.extend(bitmap);

        let mut upcase = vec![0u8; ENTRY_SIZE];
        upcase[0] = 0x82;
        upcase[4..8].copy_from_slice(&upcase_checksum().to_le_bytes());
        upcase[20..24].copy_from_slice(&layout.upcase_cluster.to_le_bytes());
        upcase[24..32].copy_from_slice(&(UPCASE_BYTES as u64).to_le_bytes());
        out.extend(upcase);

        let mut label = vec![0u8; ENTRY_SIZE];
        label[0] = 0x83;
        let units: Vec<u16> = self.label.encode_utf16().take(11).collect();
        label[1] = units.len() as u8;
        for (i, unit) in units.iter().enumerate() {
            label[2 + i * 2..4 + i * 2].copy_from_slice(&unit.to_le_bytes());
        }
        out.extend(label);
        out
    }

    /// ビットマップとルートを作り直す(= クイックフォーマット)。
    fn quick_format_image(&self, image: &mut [u8], geometry: &Geometry, layout: &Layout) {
        // FAT を初期状態へ。
        let at = geometry.fat_offset() as usize;
        let len = geometry.fat_bytes() as usize;
        image[at..at + len].fill(0);
        set_fat(&mut image[at..at + len], 0, 0xFFFF_FFF8);
        set_fat(&mut image[at..at + len], 1, EOC);

        // ビットマップはシステム領域(ルート・ビットマップ・大文字変換表)だけを
        // 使用中にする。サブディレクトリとファイル本体はそのまま残る。
        let mut bitmap = vec![0u8; geometry.bitmap_bytes() as usize];
        for cluster in layout.system_clusters.iter().copied() {
            let index = (cluster - 2) as usize;
            bitmap[index / 8] |= 1 << (index % 8);
        }
        write_clusters(image, geometry, &layout.bitmap_clusters, &bitmap);

        // ルートディレクトリはシステムエントリだけ。
        let root_clusters = layout.clusters.get(&0).cloned().unwrap_or_default();
        for &cluster in &root_clusters {
            let at = geometry.cluster_offset(cluster) as usize;
            image[at..at + geometry.cluster_size as usize].fill(0);
        }
        let entries = self.system_entries(geometry, layout);
        write_clusters(image, geometry, &root_clusters, &entries);
    }
}

struct Geometry {
    volume_sectors: u64,
    cluster_size: u64,
    sectors_per_cluster_shift: u8,
    fat_offset_sectors: u64,
    fat_sectors: u64,
    heap_offset_sectors: u64,
    cluster_count: u64,
}

impl Geometry {
    fn new(size: u64, cluster_size: u64) -> Self {
        let volume_sectors = size / BYTES_PER_SECTOR;
        let sectors_per_cluster = cluster_size / BYTES_PER_SECTOR;
        let shift = sectors_per_cluster.trailing_zeros() as u8;

        let mut fat_sectors = 1u64;
        let mut heap_offset;
        let mut cluster_count;
        loop {
            heap_offset = (FAT_OFFSET_SECTORS + fat_sectors).div_ceil(sectors_per_cluster)
                * sectors_per_cluster;
            cluster_count = (volume_sectors - heap_offset) / sectors_per_cluster;
            let needed = ((cluster_count + 2) * 4).div_ceil(BYTES_PER_SECTOR);
            if needed <= fat_sectors {
                break;
            }
            fat_sectors = needed;
        }

        Self {
            volume_sectors,
            cluster_size,
            sectors_per_cluster_shift: shift,
            fat_offset_sectors: FAT_OFFSET_SECTORS,
            fat_sectors,
            heap_offset_sectors: heap_offset,
            cluster_count,
        }
    }

    fn fat_offset(&self) -> u64 {
        self.fat_offset_sectors * BYTES_PER_SECTOR
    }

    fn fat_bytes(&self) -> u64 {
        self.fat_sectors * BYTES_PER_SECTOR
    }

    fn heap_offset(&self) -> u64 {
        self.heap_offset_sectors * BYTES_PER_SECTOR
    }

    fn cluster_offset(&self, cluster: u32) -> u64 {
        self.heap_offset() + (cluster as u64 - 2) * self.cluster_size
    }

    fn bitmap_bytes(&self) -> u64 {
        self.cluster_count.div_ceil(8)
    }
}

struct EntryPosition {
    dir: usize,
    offset_in_dir: u64,
    count: usize,
}

struct Layout {
    clusters: HashMap<usize, Vec<u32>>,
    entry_positions: HashMap<usize, EntryPosition>,
    bitmap_cluster: u32,
    bitmap_clusters: Vec<u32>,
    upcase_cluster: u32,
    upcase_clusters: Vec<u32>,
    system_clusters: Vec<u32>,
    next_free: u32,
    cluster_size: u64,
    cluster_count: u64,
}

impl Layout {
    fn new(geometry: &Geometry) -> Self {
        Self {
            clusters: HashMap::new(),
            entry_positions: HashMap::new(),
            bitmap_cluster: 0,
            bitmap_clusters: Vec::new(),
            upcase_cluster: 0,
            upcase_clusters: Vec::new(),
            system_clusters: Vec::new(),
            next_free: 2,
            cluster_size: geometry.cluster_size,
            cluster_count: geometry.cluster_count,
        }
    }

    fn alloc(&mut self, count: usize) -> Vec<u32> {
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            assert!(
                (self.next_free as u64) < self.cluster_count + 2,
                "テストイメージが小さすぎる"
            );
            out.push(self.next_free);
            self.next_free += 1;
        }
        out
    }

    fn assign(&mut self, tree: &mut FsTree, filler: Option<usize>) {
        // ルートを先頭に置く(ブートセクタで 2 と宣言しているため)。
        let root_entries = 3 + directory_entry_count(tree, 0);
        let root_clusters = ((root_entries * ENTRY_SIZE) as u64)
            .div_ceil(self.cluster_size)
            .max(1) as usize;
        let root = self.alloc(root_clusters);
        self.clusters.insert(0, root);

        let bitmap_bytes = self.cluster_count.div_ceil(8);
        self.bitmap_clusters = self.alloc(bitmap_bytes.div_ceil(self.cluster_size).max(1) as usize);
        self.bitmap_cluster = self.bitmap_clusters[0];
        self.upcase_clusters =
            self.alloc((UPCASE_BYTES as u64).div_ceil(self.cluster_size) as usize);
        self.upcase_cluster = self.upcase_clusters[0];
        self.system_clusters = self
            .clusters
            .get(&0)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .chain(self.bitmap_clusters.iter().copied())
            .chain(self.upcase_clusters.iter().copied())
            .collect();

        // ルート以外のディレクトリ。
        let mut queue: Vec<usize> = tree.node(0).children.clone();
        while let Some(id) = queue.pop() {
            if !tree.node(id).is_dir {
                continue;
            }
            let entries = directory_entry_count(tree, id);
            let clusters = ((entries * ENTRY_SIZE) as u64)
                .div_ceil(self.cluster_size)
                .max(1) as usize;
            let allocated = self.alloc(clusters);
            self.clusters.insert(id, allocated);
            queue.extend(tree.node(id).children.iter().copied());
        }

        // ファイル本体。
        let mut filler_clusters = Vec::new();
        let files: Vec<usize> = (0..tree.nodes().len())
            .filter(|&id| !tree.node(id).is_dir && Some(id) != filler)
            .collect();
        for id in files {
            let node = tree.node(id);
            let needed = (node.data.len() as u64).div_ceil(self.cluster_size) as usize;
            if needed == 0 {
                self.clusters.insert(id, Vec::new());
                continue;
            }
            let clusters = if node.fragmented {
                let block = self.alloc(needed * 2 - 1);
                let (mine, gaps) = interleave(block);
                filler_clusters.extend(gaps);
                mine
            } else {
                self.alloc(needed)
            };
            self.clusters.insert(id, clusters);
        }

        if let Some(filler) = filler {
            let len = filler_clusters.len() as u64 * self.cluster_size;
            self.clusters.insert(filler, filler_clusters);
            let data = crate::pattern_data(0xF111, len as usize);
            let path = tree.path_of(filler);
            tree.file(&path, data);
        }
    }
}

/// ディレクトリが必要とするエントリ数(名前の長さで変わる)。
fn directory_entry_count(tree: &FsTree, id: usize) -> usize {
    tree.node(id)
        .children
        .iter()
        .map(|&child| {
            2 + tree
                .node(child)
                .name
                .encode_utf16()
                .count()
                .div_ceil(15)
                .max(1)
        })
        .sum()
}

/// 1 クラスタおきに分ける。前半がファイル本体、後半が穴埋めに回すクラスタ。
fn interleave(block: Vec<u32>) -> (Vec<u32>, Vec<u32>) {
    let mut mine = Vec::new();
    let mut gaps = Vec::new();
    for (i, cluster) in block.into_iter().enumerate() {
        if i % 2 == 0 {
            mine.push(cluster);
        } else {
            gaps.push(cluster);
        }
    }
    (mine, gaps)
}

fn is_contiguous(clusters: &[u32]) -> bool {
    clusters.windows(2).all(|pair| pair[1] == pair[0] + 1)
}

/// FAT を書く。連続配置のファイルは NoFatChain なのでチェーンを持たない。
fn write_fat(image: &mut [u8], geometry: &Geometry, layout: &Layout, tree: &FsTree) {
    let at = geometry.fat_offset() as usize;
    let len = geometry.fat_bytes() as usize;
    let fat = &mut image[at..at + len];
    set_fat(fat, 0, 0xFFFF_FFF8);
    set_fat(fat, 1, EOC);

    let mut write_chain = |clusters: &[u32]| {
        for (i, &cluster) in clusters.iter().enumerate() {
            let next = clusters.get(i + 1).copied().unwrap_or(EOC);
            set_fat(fat, cluster, next);
        }
    };

    // ビットマップと大文字変換表は必ず FAT チェーンを辿る決まり。
    write_chain(&layout.bitmap_clusters);
    write_chain(&layout.upcase_clusters);

    for (&id, clusters) in &layout.clusters {
        // ディレクトリと断片化したファイルはチェーンを持つ。連続配置の
        // ファイルは NoFatChain なので FAT には何も書かない。
        if tree.node(id).is_dir || !is_contiguous(clusters) {
            write_chain(clusters);
        }
    }
}

fn write_upcase_table(image: &mut [u8], geometry: &Geometry, layout: &Layout) {
    let table = upcase_table();
    write_clusters(image, geometry, &layout.upcase_clusters, &table);
}

/// アロケーションビットマップ(クラスタ 2 が bit0)。
fn write_bitmap(image: &mut [u8], geometry: &Geometry, layout: &Layout, tree: &FsTree) {
    let mut bitmap = vec![0u8; geometry.bitmap_bytes() as usize];
    let mut mark = |cluster: u32| {
        let index = (cluster as usize).saturating_sub(2);
        if index / 8 < bitmap.len() {
            bitmap[index / 8] |= 1 << (index % 8);
        }
    };
    for &cluster in layout
        .bitmap_clusters
        .iter()
        .chain(layout.upcase_clusters.iter())
    {
        mark(cluster);
    }
    for (&id, clusters) in &layout.clusters {
        // 削除済みの項目は解放されている。
        if tree.node(id).deleted {
            continue;
        }
        for &cluster in clusters {
            mark(cluster);
        }
    }
    let clusters = layout.bitmap_clusters.clone();
    write_clusters(image, geometry, &clusters, &bitmap);
}

fn write_file_data(image: &mut [u8], geometry: &Geometry, tree: &FsTree, layout: &Layout) {
    for id in 0..tree.nodes().len() {
        let node = tree.node(id);
        if node.is_dir || node.data.is_empty() {
            continue;
        }
        if let Some(clusters) = layout.clusters.get(&id) {
            write_clusters(image, geometry, clusters, &node.data);
        }
    }
}

/// 削除を適用する。エントリの InUse ビットを落とし、FAT チェーンを解放する。
fn apply_deletions(image: &mut [u8], geometry: &Geometry, tree: &FsTree, layout: &Layout) {
    for id in 0..tree.nodes().len() {
        if !tree.node(id).deleted {
            continue;
        }
        if let Some(position) = layout.entry_positions.get(&id) {
            let dir_clusters = layout
                .clusters
                .get(&position.dir)
                .cloned()
                .unwrap_or_default();
            for i in 0..position.count {
                let offset_in_dir = position.offset_in_dir + (i * ENTRY_SIZE) as u64;
                let cluster_index = (offset_in_dir / geometry.cluster_size) as usize;
                let Some(&cluster) = dir_clusters.get(cluster_index) else {
                    continue;
                };
                let at = (geometry.cluster_offset(cluster) + offset_in_dir % geometry.cluster_size)
                    as usize;
                image[at] &= 0x7F; // InUse ビットを落とす
            }
        }
        if let Some(clusters) = layout.clusters.get(&id) {
            let at = geometry.fat_offset() as usize;
            let len = geometry.fat_bytes() as usize;
            let fat = &mut image[at..at + len];
            for &cluster in clusters {
                set_fat(fat, cluster, 0);
            }
        }
    }
}

fn set_fat(fat: &mut [u8], cluster: u32, value: u32) {
    let at = cluster as usize * 4;
    if at + 4 <= fat.len() {
        fat[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
}

fn write_clusters(image: &mut [u8], geometry: &Geometry, clusters: &[u32], data: &[u8]) {
    let cluster_size = geometry.cluster_size as usize;
    for (i, &cluster) in clusters.iter().enumerate() {
        let from = (i * cluster_size).min(data.len());
        let to = ((i + 1) * cluster_size).min(data.len());
        if from >= to {
            break;
        }
        let at = geometry.cluster_offset(cluster) as usize;
        image[at..at + (to - from)].copy_from_slice(&data[from..to]);
    }
}

/// ファイルエントリ + ストリーム拡張 + 名前エントリ。
fn build_entry_set(
    name: &str,
    is_dir: bool,
    first_cluster: u32,
    length: u64,
    contiguous: bool,
) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let name_entries = units.len().div_ceil(15).max(1);
    let secondary_count = 1 + name_entries;

    let mut set = vec![0u8; ENTRY_SIZE * (1 + secondary_count)];

    // ファイルエントリ。
    set[0] = 0x85;
    set[1] = secondary_count as u8;
    let attributes: u16 = if is_dir { 0x10 } else { 0x20 };
    set[4..6].copy_from_slice(&attributes.to_le_bytes());
    let stamp = ((TEST_DATE as u32) << 16) | TEST_TIME as u32;
    set[8..12].copy_from_slice(&stamp.to_le_bytes());
    set[12..16].copy_from_slice(&stamp.to_le_bytes());
    set[16..20].copy_from_slice(&stamp.to_le_bytes());

    // ストリーム拡張。
    let stream = ENTRY_SIZE;
    set[stream] = 0xC0;
    set[stream + 1] = 0x01 | if contiguous { 0x02 } else { 0x00 };
    set[stream + 3] = units.len() as u8;
    set[stream + 4..stream + 6].copy_from_slice(&name_hash(&units).to_le_bytes());
    set[stream + 8..stream + 16].copy_from_slice(&length.to_le_bytes());
    set[stream + 20..stream + 24].copy_from_slice(&first_cluster.to_le_bytes());
    set[stream + 24..stream + 32].copy_from_slice(&length.to_le_bytes());

    // 名前エントリ。
    for i in 0..name_entries {
        let base = ENTRY_SIZE * (2 + i);
        set[base] = 0xC1;
        for j in 0..15 {
            let unit = units.get(i * 15 + j).copied().unwrap_or(0);
            set[base + 2 + j * 2..base + 4 + j * 2].copy_from_slice(&unit.to_le_bytes());
        }
    }

    let checksum = set_checksum(&set);
    set[2..4].copy_from_slice(&checksum.to_le_bytes());
    set
}

/// エントリセットのチェックサム(先頭エントリのチェックサム欄自身は除く)。
pub fn set_checksum(entries: &[u8]) -> u16 {
    let mut sum = 0u16;
    for (i, &b) in entries.iter().enumerate() {
        if i == 2 || i == 3 {
            continue;
        }
        sum = sum.rotate_right(1).wrapping_add(b as u16);
    }
    sum
}

/// ファイル名のハッシュ(大文字化した UTF-16LE のバイト列に対して計算する)。
pub fn name_hash(units: &[u16]) -> u16 {
    let mut hash = 0u16;
    for unit in units {
        let upper = upcase_unit(*unit);
        for b in upper.to_le_bytes() {
            hash = hash.rotate_right(1).wrapping_add(b as u16);
        }
    }
    hash
}

/// 大文字変換表(ASCII だけを変換する簡易版)。
fn upcase_table() -> Vec<u8> {
    let mut table = Vec::with_capacity(UPCASE_BYTES);
    for code in 0..0x10000u32 {
        table.extend_from_slice(&upcase_unit(code as u16).to_le_bytes());
    }
    table
}

fn upcase_unit(unit: u16) -> u16 {
    match char::from_u32(unit as u32) {
        Some(c) if c.is_ascii_lowercase() => c.to_ascii_uppercase() as u16,
        _ => unit,
    }
}

fn upcase_checksum() -> u32 {
    let mut sum = 0u32;
    for b in upcase_table() {
        sum = sum.rotate_right(1).wrapping_add(b as u32);
    }
    sum
}

/// ブート領域のチェックサム。ボリュームフラグと使用率のバイトは除く。
fn boot_checksum(region: &[u8]) -> u32 {
    let mut sum = 0u32;
    for (i, &b) in region.iter().enumerate() {
        if i == 106 || i == 107 || i == 112 {
            continue;
        }
        sum = sum.rotate_right(1).wrapping_add(b as u32);
    }
    sum
}
