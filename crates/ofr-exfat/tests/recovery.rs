//! Phase 2 の完了条件のうち exFAT 側:
//! 「削除 → 復元」「クイックフォーマット → 復元」がファイル内容一致で成功すること。

use ofr_device::{Device, MockDevice};
use ofr_exfat::ExfatFs;
use ofr_fs::{EntryStatus, FileSystem, FileTree, RecoveredEntry, ScanOptions, extract};
use ofr_testfs::{Scenario, scenarios};

fn device(image: Vec<u8>) -> MockDevice {
    MockDevice::builder(image.len() as u64).data(image).build()
}

fn scan(device: &MockDevice) -> FileTree {
    let fs = ExfatFs::open(device).expect("exFAT として開けること");
    fs.scan(&ScanOptions::default(), None)
        .expect("走査できること")
}

fn by_name<'a>(tree: &'a FileTree, name: &str) -> &'a RecoveredEntry {
    tree.entries()
        .iter()
        .find(|e| e.name == name)
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
    let scenario = scenarios::exfat_deleted();
    let device = device(scenario.image.clone());
    let fs = ExfatFs::open(&device).unwrap();

    assert_eq!(fs.volume().label.as_deref(), Some("OFRTEST"));
    assert_eq!(fs.volume().bytes_per_cluster, 4096);
    assert_eq!(fs.volume().kind.label(), "exFAT");

    let tree = fs.scan(&ScanOptions::default(), None).unwrap();
    let readme = by_name(&tree, "README.TXT");
    assert_eq!(readme.path, "/README.TXT");
    assert_eq!(readme.status, EntryStatus::Intact);
    assert_eq!(readme.size, 31);
    assert_eq!(
        readme.times.modified.unwrap().to_string(),
        "2026-08-19 12:34:56"
    );
    assert_eq!(
        by_name(&tree, "DSC00001.JPG").path,
        "/DCIM/100MSDCF/DSC00001.JPG"
    );
}

#[test]
fn restores_deleted_files_byte_for_byte() {
    let scenario = scenarios::exfat_deleted();
    let device = device(scenario.image.clone());
    let tree = scan(&device);

    for name in ["DSC00002.JPG", "長い名前の写真.jpg", "NOTES.TXT"] {
        let entry = by_name(&tree, name);
        assert_eq!(entry.status, EntryStatus::Deleted, "{name}");
        // exFAT は名前を 1 文字も落とさない。
        assert!(!entry.quality.name_partial, "{name}");
        // NoFatChain が立っていたので、連続配置は仮定ではなく確定。
        assert!(!entry.quality.contiguous_assumed, "{name}");
        // 削除でビットマップから外れているので、衝突もない。
        assert_eq!(entry.quality.conflicting_clusters, 0, "{name}");
    }

    check_all_contents(&scenario, &device, &tree);
}

#[test]
fn restores_files_after_a_quick_format() {
    let scenario = scenarios::exfat_quick_format();
    let device = device(scenario.image.clone());
    let tree = scan(&device);

    let dcim_child = by_name(&tree, "100MSDCF");
    assert!(
        dcim_child.path.starts_with("/Lost+Found/"),
        "{}",
        dcim_child.path
    );
    assert_eq!(dcim_child.status, EntryStatus::Orphaned);

    // exFAT のディレクトリは自分の名前を持たないので、親の名前は分からない。
    let parent = tree.get(dcim_child.parent.unwrap()).unwrap();
    assert!(parent.name.starts_with("dir_"), "{}", parent.name);

    check_all_contents(&scenario, &device, &tree);
}

#[test]
fn flags_fragmented_files_instead_of_pretending_they_are_fine() {
    let scenario = scenarios::exfat_fragmented();
    let device = device(scenario.image.clone());
    let tree = scan(&device);

    let entry = by_name(&tree, "FRAG.BIN");
    assert_eq!(entry.status, EntryStatus::Deleted);
    // 断片化していたので NoFatChain は立っていない = 連続配置は仮定になる。
    assert!(entry.quality.contiguous_assumed);
    assert!(
        entry.quality.conflicting_clusters > 0,
        "使用中クラスタを掴んでいることを検出できていない"
    );
    assert_ne!(read(&device, entry), scenario.files[0].data);
}

#[test]
fn recovers_when_the_boot_sector_is_destroyed() {
    let mut image = scenarios::exfat_deleted().image;
    image[0..512].fill(0);
    let device = device(image);

    let fs = ExfatFs::open(&device).expect("バックアップから開けること");
    assert_eq!(fs.volume().boot_source.label(), "バックアップブートセクタ");

    let tree = fs.scan(&ScanOptions::default(), None).unwrap();
    assert_eq!(by_name(&tree, "README.TXT").size, 31);
}

#[test]
fn scans_without_the_orphan_pass_when_asked() {
    let device = device(scenarios::exfat_quick_format().image);
    let fs = ExfatFs::open(&device).unwrap();

    let options = ScanOptions {
        orphans: false,
        ..ScanOptions::default()
    };
    let tree = fs.scan(&options, None).unwrap();
    assert!(tree.is_empty(), "{:?}", paths(&tree));
}

#[test]
fn does_not_panic_on_garbage() {
    // ブートセクタだけ本物にして、中身は全部でたらめにする。
    let mut image = scenarios::exfat_deleted().image;
    let garbage = ofr_testfs::pattern_data(31, image.len() - 4096);
    image[4096..].copy_from_slice(&garbage);

    let device = device(image);
    let fs = ExfatFs::open(&device).expect("ブートセクタは通ること");
    let tree = fs
        .scan(&ScanOptions::default(), None)
        .expect("落ちないこと");
    let _ = tree.len();
}
