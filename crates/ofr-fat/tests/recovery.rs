//! Phase 2 の完了条件のうち FAT32 側:
//! 「削除 → 復元」「クイックフォーマット → 復元」がファイル内容一致で成功すること。
//!
//! テストイメージは `ofr-testfs` が Rust だけで組み立てる。実際に macOS /
//! Windows がマウントできる本物の FAT32 なので、生成側が間違っていれば
//! 手動確認 (`cargo run -p ofr-testfs`) ですぐ分かる。

use ofr_device::{Device, MockDevice};
use ofr_fat::Fat32Fs;
use ofr_fs::{EntryStatus, FileSystem, FileTree, RecoveredEntry, ScanOptions, extract};
use ofr_testfs::{Scenario, scenarios};

fn device(image: Vec<u8>) -> MockDevice {
    MockDevice::builder(image.len() as u64).data(image).build()
}

fn scan(device: &MockDevice) -> FileTree {
    let fs = Fat32Fs::open(device).expect("FAT32 として開けること");
    fs.scan(&ScanOptions::default(), None)
        .expect("走査できること")
}

/// 末尾のファイル名で項目を探す。
///
/// LFN のない 8.3 名は削除で先頭 1 文字が失われる (`DSC00002.JPG` →
/// `_SC00002.JPG`) ので、その形も同じ項目として扱う。
fn by_name<'a>(tree: &'a FileTree, name: &str) -> &'a RecoveredEntry {
    tree.entries()
        .iter()
        .find(|e| e.name == name || (e.quality.name_partial && e.name.get(1..) == name.get(1..)))
        .unwrap_or_else(|| panic!("{name} が見つからない: {:?}", paths(tree)))
}

fn paths(tree: &FileTree) -> Vec<&str> {
    tree.entries().iter().map(|e| e.path.as_str()).collect()
}

fn read(device: &dyn Device, entry: &RecoveredEntry) -> Vec<u8> {
    let mut out = Vec::new();
    extract::extract_to_writer(device, entry, &mut out, &extract::ExtractOptions::default())
        .expect("復元できること");
    out
}

fn check_all_contents(scenario: &Scenario, device: &MockDevice, tree: &FileTree) {
    for expected in &scenario.files {
        let name = expected.path.rsplit('/').next().unwrap();
        let entry = by_name(tree, name);
        assert_eq!(
            read(device, entry),
            expected.data,
            "{} の中身が一致しない",
            expected.path
        );
    }
}

#[test]
fn reads_the_intact_tree() {
    let scenario = scenarios::fat32_deleted();
    let device = device(scenario.image.clone());
    let fs = Fat32Fs::open(&device).unwrap();

    assert_eq!(fs.volume().label.as_deref(), Some("OFRTEST"));
    assert_eq!(fs.volume().bytes_per_cluster, 512);
    assert_eq!(fs.volume().kind.label(), "FAT32");

    let tree = fs.scan(&ScanOptions::default(), None).unwrap();
    let readme = by_name(&tree, "README.TXT");
    assert_eq!(readme.path, "/README.TXT");
    assert_eq!(readme.status, EntryStatus::Intact);
    assert_eq!(readme.size, 31);
    assert_eq!(
        readme.times.modified.unwrap().to_string(),
        "2026-08-19 12:34:56"
    );

    // 生きているファイルはディレクトリ構造もそのまま。
    assert_eq!(
        by_name(&tree, "DSC00001.JPG").path,
        "/DCIM/100MSDCF/DSC00001.JPG"
    );
}

#[test]
fn restores_deleted_files_byte_for_byte() {
    let scenario = scenarios::fat32_deleted();
    let device = device(scenario.image.clone());
    let tree = scan(&device);

    // 削除された 3 つが削除済みとして見つかること。
    for name in ["DSC00002.JPG", "長い名前の写真.jpg", "NOTES.TXT"] {
        let entry = by_name(&tree, name);
        assert_eq!(entry.status, EntryStatus::Deleted, "{name}");
        // FAT チェーンは解放済みなので連続配置の仮定で拾っている。
        assert!(entry.quality.contiguous_assumed, "{name}");
        // 削除直後なので、他のファイルに奪われたクラスタはない。
        assert_eq!(entry.quality.conflicting_clusters, 0, "{name}");
    }

    // 長い名前は LFN が残っているので完全に戻る。
    let long = by_name(&tree, "長い名前の写真.jpg");
    assert_eq!(long.name, "長い名前の写真.jpg");
    assert!(!long.quality.name_partial);

    // LFN のない 8.3 名は先頭 1 文字が失われる。FAT の仕様上どうにもならないので、
    // `_` で埋めたうえで「名前が不完全」と印を付ける。
    let short = by_name(&tree, "DSC00002.JPG");
    assert_eq!(short.name, "_SC00002.JPG");
    assert!(short.quality.name_partial);

    check_all_contents(&scenario, &device, &tree);
}

#[test]
fn restores_files_after_a_quick_format() {
    let scenario = scenarios::fat32_quick_format();
    let device = device(scenario.image.clone());
    let tree = scan(&device);

    // ルートも FAT も消えているので、サブディレクトリは孤立ツリーとして出る。
    let dcim_child = by_name(&tree, "100MSDCF");
    assert!(
        dcim_child.path.starts_with("/Lost+Found/"),
        "{}",
        dcim_child.path
    );
    assert_eq!(dcim_child.status, EntryStatus::Orphaned);

    // 親ディレクトリの名前は親のエントリ側にあるので分からない。
    let parent = tree.get(dcim_child.parent.unwrap()).unwrap();
    assert!(parent.name.starts_with("dir_"), "{}", parent.name);
    assert!(parent.quality.name_partial);

    check_all_contents(&scenario, &device, &tree);
}

#[test]
fn flags_fragmented_files_instead_of_pretending_they_are_fine() {
    let scenario = scenarios::fat32_fragmented();
    let device = device(scenario.image.clone());
    let tree = scan(&device);

    let entry = by_name(&tree, "FRAG.BIN");
    assert_eq!(entry.status, EntryStatus::Deleted);
    assert!(entry.quality.contiguous_assumed);
    // 間に別のファイル (_FILLER.BIN) のクラスタが挟まっているので、
    // 連続配置の仮定は外れる。使用中のクラスタを掴んでいることは検出できる。
    assert!(
        entry.quality.conflicting_clusters > 0,
        "断片化の兆候を検出できていない"
    );
    // 中身は実際に壊れる。PLAN.md 10章のとおり、ここは修復モジュール行き。
    assert_ne!(read(&device, entry), scenario.files[0].data);
}

#[test]
fn recovers_when_the_boot_sector_is_destroyed() {
    let mut image = scenarios::fat32_deleted().image;
    image[0..512].fill(0); // 先頭のブートセクタを潰す
    let device = device(image);

    let fs = Fat32Fs::open(&device).expect("バックアップから開けること");
    assert_eq!(fs.volume().boot_source.label(), "バックアップブートセクタ");

    let tree = fs.scan(&ScanOptions::default(), None).unwrap();
    assert_eq!(by_name(&tree, "README.TXT").size, 31);
}

#[test]
fn scans_without_the_orphan_pass_when_asked() {
    let scenario = scenarios::fat32_quick_format();
    let device = device(scenario.image);
    let fs = Fat32Fs::open(&device).unwrap();

    let options = ScanOptions {
        orphans: false,
        ..ScanOptions::default()
    };
    let tree = fs.scan(&options, None).unwrap();
    // ルートが消えているので、孤立走査なしでは何も見つからない。
    assert!(tree.is_empty(), "{:?}", paths(&tree));
}

#[test]
fn skipping_deleted_entries_leaves_only_live_files() {
    let device = device(scenarios::fat32_deleted().image);
    let fs = Fat32Fs::open(&device).unwrap();

    let options = ScanOptions {
        deleted: false,
        orphans: false,
        ..ScanOptions::default()
    };
    let tree = fs.scan(&options, None).unwrap();
    assert!(
        tree.entries()
            .iter()
            .all(|e| e.status == EntryStatus::Intact)
    );
    assert_eq!(tree.stats.deleted, 0);
    assert_eq!(tree.stats.files, 3);
}

#[test]
fn reports_progress_and_can_be_cancelled() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let device = device(scenarios::fat32_quick_format().image);
    let fs = Fat32Fs::open(&device).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let events = Arc::new(AtomicUsize::new(0));
    let options = ScanOptions {
        progress_interval: std::time::Duration::ZERO,
        cancel: Arc::clone(&cancel),
        ..ScanOptions::default()
    };

    let counter = Arc::clone(&events);
    let flag = Arc::clone(&cancel);
    let tree = fs
        .scan(
            &options,
            Some(Box::new(move |_| {
                if counter.fetch_add(1, Ordering::Relaxed) >= 2 {
                    flag.store(true, Ordering::Relaxed);
                }
            })),
        )
        .unwrap();

    assert!(events.load(Ordering::Relaxed) > 0, "進捗が飛んでいない");
    assert!(tree.stats.cancelled);
}

#[test]
fn does_not_panic_on_garbage() {
    let mut image = ofr_testfs::pattern_data(99, 4 << 20);
    // FAT32 と誤認させるためにブートセクタだけそれらしくする。
    image[11..13].copy_from_slice(&512u16.to_le_bytes());
    image[13] = 1;
    image[14..16].copy_from_slice(&32u16.to_le_bytes());
    image[16] = 2;
    image[17..19].copy_from_slice(&0u16.to_le_bytes());
    image[19..21].copy_from_slice(&0u16.to_le_bytes());
    image[22..24].copy_from_slice(&0u16.to_le_bytes());
    image[32..36].copy_from_slice(&8192u32.to_le_bytes());
    image[36..40].copy_from_slice(&64u32.to_le_bytes());
    image[44..48].copy_from_slice(&2u32.to_le_bytes());
    image[82..90].copy_from_slice(b"FAT32   ");
    image[510] = 0x55;
    image[511] = 0xAA;

    let device = device(image);
    let fs = Fat32Fs::open(&device).expect("ブートセクタは通ること");
    let tree = fs
        .scan(&ScanOptions::default(), None)
        .expect("落ちないこと");
    // 何が見つかっても構わない。panic しないことだけが条件 (PLAN.md 6章 5項)。
    let _ = tree.len();
}
