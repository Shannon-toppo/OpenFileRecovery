//! 走査結果から吸い出す範囲を作る通し確認(PLAN.md 11.4)。
//!
//! CLI でいうと `ofr scan --domain-out` → `ofr image --domain` の 2 手。
//! 「一度スキャンして、選んだファイルのぶんだけ取り直す」が本当に成り立つか、
//! つまり**限定して取ったイメージがそのまま解析できて、中身も一致するか**を見る。

use std::path::Path;

use ofr_core::domain::{entries_domain, metadata_plan};
use ofr_core::source;
use ofr_device::{Device, FileDevice, SliceDevice};
use ofr_fs::{RecoveredEntry, ScanOptions, extract};
use ofr_image::{ImageOptions, Imager};
use ofr_testfs::scenarios;

/// 対象のファイル。1 つに絞れば、限定できていない実装との差がはっきり出る。
const WANTED: &str = "/DCIM/100MSDCF/DSC00001.JPG";

fn read(device: &dyn Device, entry: &RecoveredEntry) -> Vec<u8> {
    let mut out = Vec::new();
    extract::extract_to_writer(device, entry, &mut out, &extract::ExtractOptions::default())
        .expect("復元できること");
    out
}

/// イメージを走査して、選んだ項目とボリュームの位置を返す。
fn scan_for(path: &Path, name: &str) -> (u64, RecoveredEntry, Vec<RecoveredEntry>) {
    let device = FileDevice::open(path).expect("イメージを開けること");
    let volume = source::locate(&device, None, None).expect("ボリュームが見つかること");
    let region = SliceDevice::new(&device, volume.offset, volume.len).unwrap();
    let fs = source::open_filesystem(&region, volume.kind).expect("開けること");
    let tree = fs
        .scan(&ScanOptions::default(), None)
        .expect("走査できること");

    let wanted = tree
        .entries()
        .iter()
        .find(|e| e.path == name)
        .unwrap_or_else(|| panic!("{name} が見つからない"))
        .clone();
    let dirs = tree
        .entries()
        .iter()
        .filter(|e| e.is_dir())
        .cloned()
        .collect();
    (volume.offset, wanted, dirs)
}

#[test]
fn images_only_the_selected_file_and_can_still_read_it() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("usb.img");
    let out_path = dir.path().join("selected.img");
    let map_path = dir.path().join("selected.img.map");

    let scenario = scenarios::fat32_deleted();
    let expected = scenario.file(WANTED).expect("シナリオにある").data.clone();
    std::fs::write(&source_path, &scenario.image).unwrap();

    // 1 手目: 走査して、選んだファイル + ディレクトリ + メタデータの範囲を出す。
    let (volume_offset, wanted, dirs) = scan_for(&source_path, WANTED);
    let device = FileDevice::open(&source_path).unwrap();
    let plan = metadata_plan(&device);
    assert!(!plan.is_empty(), "メタデータの位置を割り出せていない");

    let selected = entries_domain(std::iter::once(&wanted).chain(dirs.iter()), volume_offset);
    let domain = ofr_image::Domain::new(
        plan.domain
            .regions()
            .iter()
            .chain(selected.regions())
            .map(|r| (r.pos, r.size)),
    );
    assert!(
        domain.total() < device.len() / 4,
        "限定できていない: {} / {}",
        domain.total(),
        device.len()
    );

    // 2 手目: その範囲だけを吸い出す。
    let summary = Imager::new(&device)
        .with_options(ImageOptions {
            domain: Some(domain.clone()),
            priority: Some(plan.domain.clone()),
            ..ImageOptions::default()
        })
        .run(&out_path, Some(&map_path))
        .expect("吸い出せること");
    assert!(summary.is_complete());
    assert_eq!(
        summary.total,
        domain.total(),
        "分母が対象範囲になっていない"
    );

    // 取ったイメージはそのまま解析できて、中身も元と一致する。
    let (_, restored, _) = scan_for(&out_path, WANTED);
    let out = FileDevice::open(&out_path).unwrap();
    let volume = source::locate(&out, None, None).unwrap();
    let region = SliceDevice::new(&out, volume.offset, volume.len).unwrap();
    assert_eq!(read(&region, &restored), expected, "中身が一致しない");

    // 対象外の範囲は読んでいないので、ゼロのまま残る
    // (読み込みはセクタ単位なので、範囲はセクタ境界まで広がる)。
    let read_back = domain.aligned(u64::from(device.block_size()), device.len());
    let image = std::fs::read(&out_path).unwrap();
    assert_eq!(image.len(), scenario.image.len(), "全長は元と同じにする");
    let outside = image
        .iter()
        .enumerate()
        .filter(|(pos, b)| **b != 0 && !read_back.contains(*pos as u64))
        .count();
    assert_eq!(outside, 0, "対象範囲の外に書き込んでいる");

    // 対象範囲の中は、元のイメージとバイト単位で一致する。
    for region in read_back.regions() {
        let range = region.pos as usize..region.end() as usize;
        assert_eq!(
            &image[range.clone()],
            &scenario.image[range],
            "{} からの中身が違う",
            region.pos
        );
    }
}

/// mapfile を経由しても同じ範囲が復元できること(CLI が渡すのはこの形)。
#[test]
fn round_trips_the_domain_through_a_mapfile() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("usb.img");
    let domain_path = dir.path().join("selected.map");

    let scenario = scenarios::exfat_deleted();
    std::fs::write(&source_path, &scenario.image).unwrap();

    let (volume_offset, wanted, _) = scan_for(&source_path, WANTED);
    let domain = entries_domain(std::iter::once(&wanted), volume_offset);
    assert!(!domain.is_empty());

    let device = FileDevice::open(&source_path).unwrap();
    domain
        .to_map(device.len())
        .save(&domain_path)
        .expect("書き出せること");

    let loaded = ofr_image::Domain::load(&domain_path).expect("読み直せること");
    assert_eq!(loaded, domain);
}
