//! FAT32 イメージの組み立て。
//!
//! 実際のフォーマットと同じ構造を書く。削除は「エントリの先頭バイトを 0xE5 に
//! して FAT チェーンを解放する」、クイックフォーマットは「FAT 表とルート
//! ディレクトリだけ消す」という、OS がやるのと同じ操作で再現する。

use std::collections::{HashMap, HashSet};

use crate::tree::FsTree;
use crate::{TEST_DATE, TEST_TIME};

const BYTES_PER_SECTOR: u64 = 512;
const RESERVED_SECTORS: u64 = 32;
const NUM_FATS: u64 = 2;
const ENTRY_SIZE: usize = 32;
const EOC: u32 = 0x0FFF_FFFF;

/// FAT32 イメージビルダ。
#[derive(Debug, Clone)]
pub struct Fat32Image {
    size: u64,
    sectors_per_cluster: u32,
    label: String,
    tree: FsTree,
    quick_formatted: bool,
}

impl Fat32Image {
    /// 指定サイズのボリュームを作る。
    ///
    /// FAT32 は 65525 クラスタ以上が本来の条件なので、既定の 512 バイト
    /// クラスタなら 32MiB 以上にすること。
    pub fn new(size: u64) -> Self {
        Self {
            size: size / BYTES_PER_SECTOR * BYTES_PER_SECTOR,
            sectors_per_cluster: 1,
            label: "OFRTEST".to_string(),
            tree: FsTree::new(),
            quick_formatted: false,
        }
    }

    /// クラスタあたりのセクタ数。
    pub fn sectors_per_cluster(mut self, n: u32) -> Self {
        self.sectors_per_cluster = n.max(1);
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

    /// 最後にクイックフォーマットする(FAT 表とルートだけ消す)。
    pub fn quick_format(mut self) -> Self {
        self.quick_formatted = true;
        self
    }

    /// クラスタサイズ(バイト)。
    pub fn cluster_size(&self) -> u64 {
        self.sectors_per_cluster as u64 * BYTES_PER_SECTOR
    }

    /// イメージを組み立てる。
    pub fn build(&self) -> Vec<u8> {
        let geometry = Geometry::new(self.size, self.sectors_per_cluster as u64);
        let mut tree = self.tree.clone();

        // 断片化配置に使う穴埋めファイル。穴を「他のファイルが使っている
        // クラスタ」にしておかないと、連続配置を仮定した復元が壊れたことに
        // ならない(空きクラスタだと結果的に読めてしまう)。
        let needs_filler = tree.nodes().iter().any(|n| n.fragmented);
        let filler = needs_filler.then(|| tree.file("/_FILLER.BIN", Vec::new()));

        let mut layout = Layout::new(&geometry);
        layout.assign(&mut tree, filler);

        let mut image = vec![0u8; geometry.total_sectors as usize * BYTES_PER_SECTOR as usize];
        self.write_boot_sectors(&mut image, &geometry);
        self.write_fats(&mut image, &geometry, &layout);
        self.write_directories(&mut image, &geometry, &tree, &mut layout);
        write_file_data(&mut image, &geometry, &tree, &layout);
        apply_deletions(&mut image, &geometry, &tree, &layout);

        if self.quick_formatted {
            quick_format(&mut image, &geometry);
        }
        image
    }

    fn write_boot_sectors(&self, image: &mut [u8], geometry: &Geometry) {
        let mut boot = vec![0u8; BYTES_PER_SECTOR as usize];
        boot[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
        boot[3..11].copy_from_slice(b"MSDOS5.0");
        boot[11..13].copy_from_slice(&(BYTES_PER_SECTOR as u16).to_le_bytes());
        boot[13] = self.sectors_per_cluster as u8;
        boot[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
        boot[16] = NUM_FATS as u8;
        boot[21] = 0xF8;
        boot[24..26].copy_from_slice(&63u16.to_le_bytes());
        boot[26..28].copy_from_slice(&255u16.to_le_bytes());
        boot[32..36].copy_from_slice(&(geometry.total_sectors as u32).to_le_bytes());
        boot[36..40].copy_from_slice(&(geometry.fat_sectors as u32).to_le_bytes());
        boot[44..48].copy_from_slice(&2u32.to_le_bytes()); // ルートクラスタ
        boot[48..50].copy_from_slice(&1u16.to_le_bytes()); // FSInfo
        boot[50..52].copy_from_slice(&6u16.to_le_bytes()); // バックアップブートセクタ
        boot[64] = 0x80;
        boot[66] = 0x29;
        boot[67..71].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        boot[71..82].copy_from_slice(&pad_label(&self.label));
        boot[82..90].copy_from_slice(b"FAT32   ");
        boot[510] = 0x55;
        boot[511] = 0xAA;

        let mut fsinfo = vec![0u8; BYTES_PER_SECTOR as usize];
        fsinfo[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
        fsinfo[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
        fsinfo[488..492].copy_from_slice(&u32::MAX.to_le_bytes());
        fsinfo[492..496].copy_from_slice(&u32::MAX.to_le_bytes());
        fsinfo[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());

        // 本体 (0, 1) と、そのバックアップ (6, 7)。
        for &lba in &[0u64, 6] {
            let at = lba as usize * BYTES_PER_SECTOR as usize;
            image[at..at + boot.len()].copy_from_slice(&boot);
        }
        for &lba in &[1u64, 7] {
            let at = lba as usize * BYTES_PER_SECTOR as usize;
            image[at..at + fsinfo.len()].copy_from_slice(&fsinfo);
        }
    }

    fn write_fats(&self, image: &mut [u8], geometry: &Geometry, layout: &Layout) {
        let mut fat = vec![0u8; geometry.fat_bytes() as usize];
        set_fat(&mut fat, 0, 0x0FFF_FFF8);
        set_fat(&mut fat, 1, EOC);

        for clusters in layout.clusters.values() {
            for (i, &cluster) in clusters.iter().enumerate() {
                let next = clusters.get(i + 1).copied().unwrap_or(EOC);
                set_fat(&mut fat, cluster, next);
            }
        }

        for index in 0..NUM_FATS {
            let at = (geometry.fat_offset(index)) as usize;
            image[at..at + fat.len()].copy_from_slice(&fat);
        }
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
            let clusters = layout.clusters.get(&id).cloned().unwrap_or_default();
            let mut data = Vec::new();

            if id == 0 {
                // ルートにはボリュームラベルのエントリを置く。
                let mut entry = vec![0u8; ENTRY_SIZE];
                entry[0..11].copy_from_slice(&pad_label(&self.label));
                entry[11] = 0x08;
                data.extend(entry);
            } else {
                let parent = tree.node(id).parent.unwrap_or(0);
                let parent_cluster = if parent == 0 {
                    0 // ".." がルートを指すときは 0
                } else {
                    layout.first_cluster(parent)
                };
                data.extend(dot_entry(b".          ", layout.first_cluster(id)));
                data.extend(dot_entry(b"..         ", parent_cluster));
            }

            let mut used_short_names = HashSet::new();
            for &child in &tree.node(id).children {
                let node = tree.node(child);
                let short = short_name(&node.name, &mut used_short_names);
                let entries = build_entry_set(
                    &node.name,
                    &short,
                    node.is_dir,
                    layout.first_cluster(child),
                    node.data.len() as u32,
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

            write_clusters(image, geometry, &clusters, &data);
        }
    }
}

/// ジオメトリ。
struct Geometry {
    total_sectors: u64,
    sectors_per_cluster: u64,
    fat_sectors: u64,
    cluster_count: u64,
}

impl Geometry {
    fn new(size: u64, sectors_per_cluster: u64) -> Self {
        let total_sectors = size / BYTES_PER_SECTOR;
        let mut fat_sectors = 1u64;
        loop {
            let data_sectors = total_sectors
                .saturating_sub(RESERVED_SECTORS)
                .saturating_sub(NUM_FATS * fat_sectors);
            let clusters = data_sectors / sectors_per_cluster;
            let needed = ((clusters + 2) * 4).div_ceil(BYTES_PER_SECTOR);
            if needed <= fat_sectors {
                break;
            }
            fat_sectors = needed;
        }
        let data_sectors = total_sectors - RESERVED_SECTORS - NUM_FATS * fat_sectors;
        Self {
            total_sectors,
            sectors_per_cluster,
            fat_sectors,
            cluster_count: data_sectors / sectors_per_cluster,
        }
    }

    fn cluster_size(&self) -> u64 {
        self.sectors_per_cluster * BYTES_PER_SECTOR
    }

    fn fat_offset(&self, index: u64) -> u64 {
        (RESERVED_SECTORS + index * self.fat_sectors) * BYTES_PER_SECTOR
    }

    fn fat_bytes(&self) -> u64 {
        self.fat_sectors * BYTES_PER_SECTOR
    }

    fn data_offset(&self) -> u64 {
        (RESERVED_SECTORS + NUM_FATS * self.fat_sectors) * BYTES_PER_SECTOR
    }

    fn cluster_offset(&self, cluster: u32) -> u64 {
        self.data_offset() + (cluster as u64 - 2) * self.cluster_size()
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
    next_free: u32,
    cluster_size: u64,
    cluster_count: u64,
}

impl Layout {
    fn new(geometry: &Geometry) -> Self {
        Self {
            clusters: HashMap::new(),
            entry_positions: HashMap::new(),
            next_free: 2,
            cluster_size: geometry.cluster_size(),
            cluster_count: geometry.cluster_count,
        }
    }

    fn first_cluster(&self, id: usize) -> u32 {
        self.clusters
            .get(&id)
            .and_then(|c| c.first().copied())
            .unwrap_or(0)
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

    /// ディレクトリ → ファイルの順に領域を割り当てる。
    fn assign(&mut self, tree: &mut FsTree, filler: Option<usize>) {
        // ディレクトリは幅優先。ルートが必ずクラスタ 2 になる。
        let mut queue = vec![0usize];
        while let Some(id) = queue.pop() {
            let entries = directory_entry_count(tree, id);
            let bytes = (entries * ENTRY_SIZE) as u64;
            let clusters = bytes.div_ceil(self.cluster_size).max(1) as usize;
            let allocated = self.alloc(clusters);
            self.clusters.insert(id, allocated);
            for &child in tree.node(id).children.iter().rev() {
                if tree.node(child).is_dir {
                    queue.push(child);
                }
            }
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
                // 1 クラスタおきに配置し、間は穴埋めファイルに持たせる。
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
            if let Some(node) = tree.find("/_FILLER.BIN") {
                set_data(tree, node, data);
            }
        }
    }
}

fn set_data(tree: &mut FsTree, id: usize, data: Vec<u8>) {
    let path = tree.path_of(id);
    tree.file(&path, data);
}

/// ディレクトリが必要とするエントリ数。
fn directory_entry_count(tree: &FsTree, id: usize) -> usize {
    let mut count = if id == 0 { 1 } else { 2 }; // ラベル or "." ".."
    let mut used = HashSet::new();
    for &child in &tree.node(id).children {
        let name = &tree.node(child).name;
        let short = short_name(name, &mut used);
        count += 1 + lfn_entry_count(name, &short);
    }
    count
}

fn lfn_entry_count(name: &str, short: &[u8; 11]) -> usize {
    if is_plain_short_name(name, short) {
        0
    } else {
        name.encode_utf16().count().div_ceil(13)
    }
}

/// 8.3 名だけで表せる名前か。
fn is_plain_short_name(name: &str, short: &[u8; 11]) -> bool {
    format_short(short) == name
}

fn format_short(raw: &[u8; 11]) -> String {
    let base = String::from_utf8_lossy(&raw[0..8]).trim_end().to_string();
    let ext = String::from_utf8_lossy(&raw[8..11]).trim_end().to_string();
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

/// 8.3 名を作る。同じディレクトリ内で重複しないよう `~N` を振る。
fn short_name(name: &str, used: &mut HashSet<[u8; 11]>) -> [u8; 11] {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => (name, ""),
    };
    let clean = |s: &str, max: usize| -> Vec<u8> {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric() || "-_~!#$%&@^".contains(*c))
            .map(|c| c.to_ascii_uppercase() as u8)
            .take(max)
            .collect()
    };

    let base = clean(stem, 8);
    let ext_bytes = clean(ext, 3);
    let plain_ok = base.len() == stem.chars().count()
        && ext_bytes.len() == ext.chars().count()
        && stem.chars().all(|c| !c.is_ascii_lowercase())
        && ext.chars().all(|c| !c.is_ascii_lowercase());

    let mut candidate = [b' '; 11];
    if plain_ok && !base.is_empty() {
        candidate[0..base.len()].copy_from_slice(&base);
        candidate[8..8 + ext_bytes.len()].copy_from_slice(&ext_bytes);
        if used.insert(candidate) {
            return candidate;
        }
    }

    for n in 1..=99u32 {
        let suffix = format!("~{n}");
        let keep = 8 - suffix.len();
        let mut stem_bytes: Vec<u8> = base.iter().copied().take(keep).collect();
        while stem_bytes.len() < keep.min(base.len().max(1)) {
            stem_bytes.push(b'X');
        }
        stem_bytes.extend(suffix.bytes());
        let mut candidate = [b' '; 11];
        candidate[0..stem_bytes.len()].copy_from_slice(&stem_bytes);
        candidate[8..8 + ext_bytes.len()].copy_from_slice(&ext_bytes);
        if used.insert(candidate) {
            return candidate;
        }
    }
    candidate
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

fn checksum(raw: &[u8; 11]) -> u8 {
    let mut sum = 0u8;
    for &b in raw.iter() {
        sum = sum.rotate_right(1).wrapping_add(b);
    }
    sum
}

/// LFN + 8.3 のエントリ列を作る。
fn build_entry_set(name: &str, short: &[u8; 11], is_dir: bool, cluster: u32, size: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let lfn_count = lfn_entry_count(name, short);

    if lfn_count > 0 {
        let sum = checksum(short);
        let mut units: Vec<u16> = name.encode_utf16().collect();
        units.push(0);
        while units.len() % 13 != 0 {
            units.push(0xFFFF);
        }
        // 物理的には「最後の断片が先」の順に並べる。
        for seq in (1..=lfn_count).rev() {
            let chunk = &units[(seq - 1) * 13..seq * 13];
            let mut entry = vec![0u8; ENTRY_SIZE];
            entry[0] = seq as u8 | if seq == lfn_count { 0x40 } else { 0 };
            entry[11] = 0x0F;
            entry[13] = sum;
            let positions = [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
            for (i, &pos) in positions.iter().enumerate() {
                entry[pos..pos + 2].copy_from_slice(&chunk[i].to_le_bytes());
            }
            out.extend(entry);
        }
    }

    let mut entry = vec![0u8; ENTRY_SIZE];
    entry[0..11].copy_from_slice(short);
    entry[11] = if is_dir { 0x10 } else { 0x20 };
    entry[14..16].copy_from_slice(&TEST_TIME.to_le_bytes());
    entry[16..18].copy_from_slice(&TEST_DATE.to_le_bytes());
    entry[18..20].copy_from_slice(&TEST_DATE.to_le_bytes());
    entry[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    entry[22..24].copy_from_slice(&TEST_TIME.to_le_bytes());
    entry[24..26].copy_from_slice(&TEST_DATE.to_le_bytes());
    entry[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    entry[28..32].copy_from_slice(&if is_dir { 0 } else { size }.to_le_bytes());
    out.extend(entry);
    out
}

fn dot_entry(name: &[u8; 11], cluster: u32) -> Vec<u8> {
    let mut entry = vec![0u8; ENTRY_SIZE];
    entry[0..11].copy_from_slice(name);
    entry[11] = 0x10;
    entry[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    entry[22..24].copy_from_slice(&TEST_TIME.to_le_bytes());
    entry[24..26].copy_from_slice(&TEST_DATE.to_le_bytes());
    entry[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    entry
}

fn pad_label(label: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    for (i, b) in label.bytes().take(11).enumerate() {
        out[i] = b.to_ascii_uppercase();
    }
    out
}

fn set_fat(fat: &mut [u8], cluster: u32, value: u32) {
    let at = cluster as usize * 4;
    if at + 4 <= fat.len() {
        fat[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
}

fn write_clusters(image: &mut [u8], geometry: &Geometry, clusters: &[u32], data: &[u8]) {
    let cluster_size = geometry.cluster_size() as usize;
    for (i, &cluster) in clusters.iter().enumerate() {
        let src =
            &data[(i * cluster_size).min(data.len())..((i + 1) * cluster_size).min(data.len())];
        if src.is_empty() {
            break;
        }
        let at = geometry.cluster_offset(cluster) as usize;
        image[at..at + src.len()].copy_from_slice(src);
    }
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

/// 削除を適用する。OS がやるのと同じく、エントリに削除マークを付けて
/// FAT チェーンを解放するだけ。データ本体はそのまま残す。
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
                let cluster_index = (offset_in_dir / geometry.cluster_size()) as usize;
                let Some(&cluster) = dir_clusters.get(cluster_index) else {
                    continue;
                };
                let at = geometry.cluster_offset(cluster) + offset_in_dir % geometry.cluster_size();
                image[at as usize] = 0xE5;
            }
        }

        if let Some(clusters) = layout.clusters.get(&id) {
            for index in 0..NUM_FATS {
                let fat_at = geometry.fat_offset(index) as usize;
                let fat_len = geometry.fat_bytes() as usize;
                let fat = &mut image[fat_at..fat_at + fat_len];
                for &cluster in clusters {
                    set_fat(fat, cluster, 0);
                }
            }
        }
    }
}

/// クイックフォーマット: FAT 表とルートディレクトリだけ消す。
/// サブディレクトリのクラスタとファイル本体はそのまま残る。
fn quick_format(image: &mut [u8], geometry: &Geometry) {
    for index in 0..NUM_FATS {
        let at = geometry.fat_offset(index) as usize;
        let len = geometry.fat_bytes() as usize;
        let fat = &mut image[at..at + len];
        fat.fill(0);
        set_fat(fat, 0, 0x0FFF_FFF8);
        set_fat(fat, 1, EOC);
        set_fat(fat, 2, EOC); // 新しい空のルート
    }
    let root_at = geometry.cluster_offset(2) as usize;
    let root_len = geometry.cluster_size() as usize;
    image[root_at..root_at + root_len].fill(0);
}
