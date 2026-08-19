//! Phase 4 の完了条件:
//! 「エラー注入デバイスからのコピーで、読めたファイルが全て宛先に揃い
//!   レポートと一致する」
//!
//! 壊れた USB メモリは CI に置けないので、不良セクタは `MockDevice` で注入する
//! (PLAN.md 9章)。テストイメージは `ofr-testfs` が組み立てる本物の
//! FAT32 / exFAT なので、ここを通れば「イメージを解析 → ツリーをそのまま宛先へ」
//! が一続きで動いたことになる。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use ofr_copy::{Copier, CopyOptions, CopyStatus, ExistingFile, MountSource, TreeSource};
use ofr_device::{Device, MockDevice};
use ofr_exfat::ExfatFs;
use ofr_fat::Fat32Fs;
use ofr_fs::{EntryStatus, FileSystem, FileTree, ScanOptions};
use ofr_testfs::{Scenario, scenarios};

fn device(image: Vec<u8>) -> MockDevice {
    MockDevice::builder(image.len() as u64).data(image).build()
}

fn scan_fat(device: &dyn Device) -> FileTree {
    Fat32Fs::open(device)
        .expect("FAT32 として開けること")
        .scan(&ScanOptions::default(), None)
        .expect("走査できること")
}

fn scan_exfat(device: &dyn Device) -> FileTree {
    ExfatFs::open(device)
        .expect("exFAT として開けること")
        .scan(&ScanOptions::default(), None)
        .expect("走査できること")
}

/// 生きているファイルだけを取り出す(コピーの既定の対象)。
fn intact_files(scenario: &Scenario) -> Vec<&ofr_testfs::ExpectedFile> {
    scenario.files.iter().filter(|f| !f.deleted).collect()
}

/// 復旧元のパスに対応する宛先のパス。
fn mirrored(dest: &Path, path: &str) -> PathBuf {
    let mut out = dest.to_path_buf();
    for component in path.split('/').filter(|s| !s.is_empty()) {
        out.push(component);
    }
    out
}

/// 走査結果からパスで引く。
fn extent_offset(tree: &FileTree, path: &str, at: u64) -> u64 {
    let entry = tree
        .entries()
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("{path} が見つからない"));
    let extent = entry
        .extents
        .iter()
        .find(|e| e.len > at)
        .expect("ファイルの途中を含む領域があること");
    extent.offset + at
}

#[test]
fn image_expansion_mirrors_the_whole_tree() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("mirror");
    let scenario = scenarios::fat32_deleted();
    let device = device(scenario.image.clone());
    let tree = scan_fat(&device);

    let source = TreeSource::new(&device, &tree).with_label("fat32.img");
    let report = Copier::new(&source, &dest).run().unwrap();

    // 生きているファイルが構造そのままで、中身も一致して揃っている。
    for expected in intact_files(&scenario) {
        let output = mirrored(&dest, &expected.path);
        assert!(output.is_file(), "{} が無い", output.display());
        assert_eq!(
            std::fs::read(&output).unwrap(),
            expected.data,
            "{} の中身が一致しない",
            expected.path
        );
    }
    // フォルダは空でも作る。
    assert!(dest.join("DCIM/100MSDCF").is_dir());
    assert!(dest.join("DOCS").is_dir());

    // 削除済みファイルは既定では対象外(それは `ofr restore` の仕事)。
    assert!(!mirrored(&dest, "/DOCS/NOTES.TXT").exists());

    assert_eq!(report.source, "fat32.img");
    assert_eq!(report.summary.files, intact_files(&scenario).len() as u64);
    assert_eq!(report.summary.copied, report.summary.files);
    assert_eq!(report.summary.partial, 0);
    assert_eq!(report.summary.failed, 0);
    assert!(report.summary.is_complete());
}

/// Phase 4 の完了条件そのもの。
#[test]
fn copies_everything_readable_from_a_failing_device() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("mirror");
    let scenario = scenarios::fat32_deleted();

    // まず健全なデバイスで走査して、壊す位置(あるファイルの 4096 バイト目)を決める。
    let healthy = device(scenario.image.clone());
    let bad_at = extent_offset(&scan_fat(&healthy), "/DCIM/100MSDCF/DSC00001.JPG", 4096);

    // 恒久不良を 1 か所、一過性不良(2 回目で読める)を 1 か所注入する。
    let broken = MockDevice::builder(0)
        .data(scenario.image.clone())
        .bad_range(bad_at, 512)
        .transient_range(bad_at + 4096, 512, 1)
        .build();

    let tree = scan_fat(&broken);
    let source = TreeSource::new(&broken, &tree);
    let report = Copier::new(&source, &dest)
        .with_options(CopyOptions {
            // 読み込み単位を小さくすると、不良セクタで捨てる量もその分で済む。
            chunk_size: 512,
            retries: 2,
            retry_delay: std::time::Duration::ZERO,
            ..CopyOptions::default()
        })
        .run()
        .unwrap();

    // 1. 読めたファイルは全て宛先に揃っている。
    for expected in intact_files(&scenario) {
        let output = mirrored(&dest, &expected.path);
        assert!(output.is_file(), "{} が無い", output.display());
    }

    // 2. 不良セクタに当たったファイルは、読めた分がそのまま入っていて、
    //    読めなかった 512 バイトだけがゼロで埋まっている。
    let damaged = std::fs::read(mirrored(&dest, "/DCIM/100MSDCF/DSC00001.JPG")).unwrap();
    let original = &scenario.file("/DCIM/100MSDCF/DSC00001.JPG").unwrap().data;
    assert_eq!(damaged.len(), original.len());
    assert_eq!(&damaged[..4096], &original[..4096]);
    assert_eq!(&damaged[4096..4608], &[0u8; 512], "不良部分はゼロ埋め");
    assert_eq!(&damaged[4608..], &original[4608..], "その先は読めている");

    // 3. 一過性不良はリトライで読めるので、欠けは 512 バイトだけ。
    assert_eq!(report.summary.bytes_missing, 512);
    assert_eq!(report.summary.partial, 1);
    assert_eq!(report.summary.failed, 0);
    assert_eq!(report.summary.copied, report.summary.files - 1);
    assert!(!report.summary.is_complete());

    // 4. レポートが宛先の実物と一致する。
    for file in &report.files {
        let on_disk = std::fs::metadata(&file.output)
            .unwrap_or_else(|e| panic!("{} が無い: {e}", file.output.display()));
        assert_eq!(
            on_disk.len(),
            file.written,
            "{} の大きさがレポートと違う",
            file.source
        );
        assert_eq!(
            file.status,
            if file.missing == 0 {
                CopyStatus::Copied
            } else {
                CopyStatus::Partial
            }
        );
    }
    let damaged_result = report
        .incomplete_files()
        .next()
        .expect("欠けたファイルが記録されていること");
    assert_eq!(damaged_result.source, "/DCIM/100MSDCF/DSC00001.JPG");
    assert_eq!(damaged_result.missing, 512);
    assert!(damaged_result.read_errors >= 1);

    // 5. レポートを宛先に書き出せる(JSON + 人間向けサマリ)。
    let (json, text) = report.write_to_dir(&dest).unwrap();
    let json = std::fs::read_to_string(json).unwrap();
    assert!(json.contains("\"status\": \"partial\""), "{json}");
    assert!(json.contains("\"bytes_missing\": 512"), "{json}");
    let text = std::fs::read_to_string(text).unwrap();
    assert!(text.contains("一部欠け:   1 件"), "{text}");
    assert!(text.contains("DSC00001.JPG"), "{text}");
}

#[test]
fn exfat_images_expand_the_same_way() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("mirror");
    let scenario = scenarios::exfat_deleted();
    let device = device(scenario.image.clone());
    let tree = scan_exfat(&device);

    let source = TreeSource::new(&device, &tree);
    let report = Copier::new(&source, &dest).run().unwrap();

    for expected in intact_files(&scenario) {
        assert_eq!(
            std::fs::read(mirrored(&dest, &expected.path)).unwrap(),
            expected.data,
            "{} の中身が一致しない",
            expected.path
        );
    }
    assert!(report.summary.is_complete());
}

#[test]
fn deleted_entries_can_be_included() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("mirror");
    let scenario = scenarios::exfat_deleted();
    let device = device(scenario.image.clone());
    let tree = scan_exfat(&device);

    let source = TreeSource::new(&device, &tree).with_all_statuses();
    let report = Copier::new(&source, &dest).run().unwrap();

    // exFAT は削除しても名前が欠けないので、そのままの場所に戻る。
    let deleted = scenario.file("/DOCS/NOTES.TXT").unwrap();
    assert_eq!(
        std::fs::read(mirrored(&dest, "/DOCS/NOTES.TXT")).unwrap(),
        deleted.data
    );
    assert_eq!(report.summary.files, scenario.files.len() as u64);
}

#[test]
fn logical_copy_mirrors_a_mounted_folder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("volume");
    let dest = dir.path().join("mirror");
    std::fs::create_dir_all(root.join("DCIM/100MSDCF")).unwrap();
    std::fs::create_dir_all(root.join("EMPTY")).unwrap();
    std::fs::write(root.join("DCIM/100MSDCF/a.jpg"), vec![9u8; 5000]).unwrap();
    std::fs::write(root.join("メモ.txt"), "こんにちは".as_bytes()).unwrap();

    let source = MountSource::new(&root);
    let report = Copier::new(&source, &dest)
        .with_options(CopyOptions {
            chunk_size: 1024,
            ..CopyOptions::default()
        })
        .run()
        .unwrap();

    assert_eq!(
        std::fs::read(dest.join("DCIM/100MSDCF/a.jpg")).unwrap(),
        vec![9u8; 5000]
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("メモ.txt")).unwrap(),
        "こんにちは"
    );
    assert!(dest.join("EMPTY").is_dir(), "空フォルダも作る");

    assert_eq!(report.summary.files, 2);
    assert_eq!(report.summary.copied, 2);
    assert_eq!(report.summary.dirs, 3);
    assert!(report.summary.is_complete());
    assert_eq!(report.source, root.display().to_string());
}

#[test]
fn existing_files_can_be_skipped_to_resume() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("volume");
    let dest = dir.path().join("mirror");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), b"new").unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("a.txt"), b"old").unwrap();

    let source = MountSource::new(&root);
    let report = Copier::new(&source, &dest)
        .with_options(CopyOptions {
            on_existing: ExistingFile::Skip,
            ..CopyOptions::default()
        })
        .run()
        .unwrap();
    assert_eq!(report.summary.skipped, 1);
    assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"old");

    // 既定は「番号を足して両方残す」。復旧では、消すより残すほうが安全。
    let report = Copier::new(&MountSource::new(&root), &dest).run().unwrap();
    assert_eq!(report.summary.copied, 1);
    assert_eq!(std::fs::read(dest.join("a (2).txt")).unwrap(), b"new");
}

#[test]
fn cancelling_stops_between_files_and_keeps_what_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("mirror");
    let scenario = scenarios::fat32_deleted();
    let device = device(scenario.image.clone());
    let tree = scan_fat(&device);

    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    let source = TreeSource::new(&device, &tree);
    let report = Copier::new(&source, &dest)
        .with_cancel(Arc::clone(&cancel))
        // 1 ファイル書けた時点で中断する。
        .with_file_done(move |_| flag.store(true, std::sync::atomic::Ordering::Relaxed))
        .run()
        .unwrap();

    assert!(report.summary.cancelled);
    assert_eq!(report.summary.copied, 1);
    assert!(
        report.warnings.iter().any(|w| w.contains("中断")),
        "{:?}",
        report.warnings
    );
    // 書けたファイルは残る。
    assert_eq!(report.files.len(), 1);
    assert!(report.files[0].output.is_file());
}

#[test]
fn plan_reports_what_would_be_copied_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("mirror");
    let scenario = scenarios::fat32_deleted();
    let device = device(scenario.image.clone());
    let tree = scan_fat(&device);

    let source = TreeSource::new(&device, &tree);
    let items = Copier::new(&source, &dest).plan().unwrap();

    assert!(!dest.exists(), "--dry-run 相当では何も書かない");
    let files: Vec<&str> = items
        .iter()
        .filter(|i| !i.is_dir())
        .map(|i| i.path.as_str())
        .collect();
    assert!(files.contains(&"/DCIM/100MSDCF/DSC00001.JPG"), "{files:?}");
    assert!(items.iter().all(|i| i.status == EntryStatus::Intact));
}
