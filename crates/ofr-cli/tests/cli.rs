//! CLI の結合テスト。Phase 2 / Phase 3 の完了条件を、実際のコマンドの形で確かめる。
//!
//! テストイメージは `ofr-testfs` が生成する本物の FAT32 / exFAT なので、
//! ここを通れば「イメージを渡して `ofr scan` → `ofr restore` で中身が戻る」
//! ところまで一続きで検証できたことになる。カービングは FS を持たない生イメージで
//! 確かめる(形式ごとの詳しい検証は `ofr-carve` 側のテスト)。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ofr_testfs::{Scenario, scenarios};

/// 終了コード: 全部できた。
const EXIT_OK: i32 = 0;
/// 終了コード: 一部だけ、または何も見つからなかった。
const EXIT_INCOMPLETE: i32 = 1;

fn ofr(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ofr"))
        .args(args)
        .output()
        .expect("ofr を起動できること")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn write_image(dir: &Path, name: &str, scenario: &Scenario) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, &scenario.image).unwrap();
    path
}

/// 復元先から、名前でファイルを探す。
fn find_restored(root: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()? {
            let path = entry.ok()?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == name) {
                return Some(path);
            }
        }
    }
    None
}

#[test]
fn scan_lists_deleted_files() {
    let dir = tempfile::tempdir().unwrap();
    let image = write_image(dir.path(), "fat32.img", &scenarios::fat32_deleted());

    let output = ofr(&["scan", image.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(EXIT_OK));

    let text = stdout(&output);
    assert!(text.contains("FAT32"), "{text}");
    assert!(text.contains("OFRTEST"), "{text}");
    assert!(text.contains("/DCIM/100MSDCF/DSC00001.JPG"), "{text}");
    // 削除済みは印と注記付きで出る。
    assert!(text.contains("削除済み"), "{text}");
    assert!(text.contains("連続配置と仮定"), "{text}");
}

#[test]
fn scan_json_is_machine_readable() {
    let dir = tempfile::tempdir().unwrap();
    let image = write_image(dir.path(), "exfat.img", &scenarios::exfat_deleted());

    let output = ofr(&["scan", image.to_str().unwrap(), "--json"]);
    assert_eq!(output.status.code(), Some(EXIT_OK));

    let text = stdout(&output);
    assert!(text.starts_with('{'), "{text}");
    assert!(text.contains("\"type\": \"exFAT\""), "{text}");
    assert!(text.contains("\"status\": \"deleted\""), "{text}");
    // exFAT は削除しても名前が欠けない。
    assert!(text.contains("長い名前の写真.jpg"), "{text}");
}

#[test]
fn scan_filters_by_pattern_and_status() {
    let dir = tempfile::tempdir().unwrap();
    let image = write_image(dir.path(), "fat32.img", &scenarios::fat32_deleted());

    let output = ofr(&[
        "scan",
        image.to_str().unwrap(),
        "--include",
        "*.jpg",
        "--status",
        "deleted",
    ]);
    let text = stdout(&output);
    assert!(text.contains(".jpg") || text.contains(".JPG"), "{text}");
    assert!(!text.contains("README.TXT"), "{text}");

    // 何も引っかからない条件なら終了コード 1。
    let output = ofr(&["scan", image.to_str().unwrap(), "--include", "*.nomatch"]);
    assert_eq!(output.status.code(), Some(EXIT_INCOMPLETE));
}

#[test]
fn restore_writes_deleted_files_with_matching_contents() {
    let dir = tempfile::tempdir().unwrap();
    let scenario = scenarios::fat32_deleted();
    let image = write_image(dir.path(), "fat32.img", &scenario);
    let dest = dir.path().join("recovered");

    let output = ofr(&["restore", image.to_str().unwrap(), dest.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(EXIT_OK), "{}", stdout(&output));

    for expected in &scenario.files {
        let name = expected.path.rsplit('/').next().unwrap();
        // 削除された 8.3 名は先頭 1 文字が失われる (LFN のある名前は欠けない)。
        let mut chars = name.chars();
        chars.next();
        let candidates = [name.to_string(), format!("_{}", chars.as_str())];
        let path = candidates
            .iter()
            .find_map(|n| find_restored(&dest, n))
            .unwrap_or_else(|| panic!("{name} が復元されていない"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            expected.data,
            "{} の中身が一致しない",
            expected.path
        );
    }

    // レポートも書かれている。
    let report = std::fs::read_to_string(dest.join("ofr-restore-report.json")).unwrap();
    assert!(report.contains("\"summary\""), "{report}");
    assert!(report.contains("\"missing\": 0"), "{report}");
}

#[test]
fn restore_rebuilds_the_tree_after_a_quick_format() {
    let dir = tempfile::tempdir().unwrap();
    let scenario = scenarios::exfat_quick_format();
    let image = write_image(dir.path(), "exfat.img", &scenario);
    let dest = dir.path().join("recovered");

    let output = ofr(&["restore", image.to_str().unwrap(), dest.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(EXIT_OK), "{}", stdout(&output));

    for expected in &scenario.files {
        let name = expected.path.rsplit('/').next().unwrap();
        let path = find_restored(&dest, name).unwrap_or_else(|| panic!("{name} がない"));
        assert_eq!(std::fs::read(&path).unwrap(), expected.data, "{name}");
        // 元のフォルダ構造(名前が分かる範囲)は保たれている。
        assert!(path.to_string_lossy().contains("Lost+Found"));
    }
}

#[test]
fn restore_dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let image = write_image(dir.path(), "fat32.img", &scenarios::fat32_deleted());
    let dest = dir.path().join("recovered");

    let output = ofr(&[
        "restore",
        image.to_str().unwrap(),
        dest.to_str().unwrap(),
        "--dry-run",
    ]);
    assert_eq!(output.status.code(), Some(EXIT_OK));
    assert!(stdout(&output).contains("--dry-run"));
    assert!(!dest.exists(), "--dry-run なのに復元先が作られている");
}

#[test]
fn restore_can_flatten_and_filter() {
    let dir = tempfile::tempdir().unwrap();
    let image = write_image(dir.path(), "fat32.img", &scenarios::fat32_deleted());
    let dest = dir.path().join("flat");

    let output = ofr(&[
        "restore",
        image.to_str().unwrap(),
        dest.to_str().unwrap(),
        "--flatten",
        "--include",
        "*.TXT",
        "--status",
        "intact",
    ]);
    assert_eq!(output.status.code(), Some(EXIT_OK), "{}", stdout(&output));

    assert!(dest.join("README.TXT").is_file());
    assert!(
        !dest.join("DCIM").exists(),
        "--flatten なのに階層が作られている"
    );
}

#[test]
fn unknown_volumes_are_reported_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("garbage.img");
    std::fs::write(&path, ofr_testfs::pattern_data(5, 4 << 20)).unwrap();

    let output = ofr(&["scan", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("見つからない"), "{stderr}");
}

/// カービング用に、ファイルシステムのない生イメージを組み立てる。
///
/// 中身はクラスタ境界に置いた PDF と GIF。どちらもチェックサムを持たないので
/// テスト内で手書きできる(本格的な検証は `ofr-carve` 側のテストでやっている)。
fn carve_image() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut pdf = Vec::from(&b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n"[..]);
    pdf.extend_from_slice(b"% ");
    pdf.extend_from_slice(&[b'x'; 64]);
    pdf.extend_from_slice(b"\ntrailer\n<< /Size 2 >>\nstartxref\n9\n%%EOF\n");

    let mut gif = Vec::from(&b"GIF89a"[..]);
    gif.extend_from_slice(&[0x40, 0x00, 0x30, 0x00]); // 64x48
    gif.extend_from_slice(&[0xF0, 0x00, 0x00]); // 大域カラーテーブルあり
    gif.extend_from_slice(&[0, 0, 0, 0xFF, 0xFF, 0xFF]);
    gif.push(0x2C); // 画像記述子
    gif.extend_from_slice(&[0, 0, 0, 0, 0x40, 0x00, 0x30, 0x00, 0x00]);
    gif.push(2); // LZW 最小符号長
    for _ in 0..2 {
        gif.push(64);
        gif.extend_from_slice(&[0x41; 64]);
    }
    gif.push(0); // サブブロック終端
    gif.push(0x3B); // トレーラ

    let mut image = vec![0u8; 4096];
    image.extend_from_slice(&pdf);
    image.resize(8192, 0);
    image.extend_from_slice(&gif);
    image.resize(12288, 0);
    (image, pdf, gif)
}

#[test]
fn carve_finds_files_without_a_filesystem() {
    let dir = tempfile::tempdir().unwrap();
    let (image, pdf, gif) = carve_image();
    let path = dir.path().join("nofs.img");
    std::fs::write(&path, &image).unwrap();
    let dest = dir.path().join("carved");

    let output = ofr(&[
        "carve",
        path.to_str().unwrap(),
        dest.to_str().unwrap(),
        "--align",
        "4096",
    ]);
    assert_eq!(output.status.code(), Some(EXIT_OK), "{}", stdout(&output));

    let text = stdout(&output);
    assert!(text.contains("発見:   2 件"), "{text}");

    // 形式ごとのサブフォルダに、元と 1 バイトも違わない中身で出る。
    assert_eq!(
        std::fs::read(find_restored(&dest, "carved_000001.pdf").expect("PDF が出ていない"))
            .unwrap(),
        pdf
    );
    assert_eq!(
        std::fs::read(find_restored(&dest, "carved_000002.gif").expect("GIF が出ていない"))
            .unwrap(),
        gif
    );

    // レポートは機械可読。
    let report = std::fs::read_to_string(dest.join("carve-report.json")).unwrap();
    assert!(report.contains("\"format\": \"pdf\""), "{report}");
    assert!(report.contains("\"confidence\": \"exact\""), "{report}");
}

#[test]
fn carve_dry_run_lists_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let (image, _, _) = carve_image();
    let path = dir.path().join("nofs.img");
    std::fs::write(&path, &image).unwrap();
    let dest = dir.path().join("carved");

    let output = ofr(&[
        "carve",
        path.to_str().unwrap(),
        dest.to_str().unwrap(),
        "--dry-run",
        "--formats",
        "gif",
    ]);
    assert_eq!(output.status.code(), Some(EXIT_OK), "{}", stdout(&output));

    let text = stdout(&output);
    assert!(text.contains("発見:   1 件"), "{text}");
    assert!(text.contains("64x48"), "{text}");
    assert!(!dest.exists(), "--dry-run なのに出力先が作られている");
}

#[test]
fn copy_expands_an_image_into_a_mirror_tree() {
    let dir = tempfile::tempdir().unwrap();
    let scenario = scenarios::fat32_deleted();
    let image = write_image(dir.path(), "fat32.img", &scenario);
    let dest = dir.path().join("mirror");

    let output = ofr(&["copy", image.to_str().unwrap(), dest.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(EXIT_OK), "{}", stdout(&output));

    let text = stdout(&output);
    assert!(text.contains("ファイルシステムを直読みして展開"), "{text}");
    assert!(text.contains("コピー: 3 件"), "{text}");

    // 生きているファイルがフォルダ構造ごと、中身も一致して並ぶ。
    for expected in scenario.files.iter().filter(|f| !f.deleted) {
        let mut path = dest.clone();
        for component in expected.path.split('/').filter(|s| !s.is_empty()) {
            path.push(component);
        }
        assert_eq!(
            std::fs::read(&path).unwrap_or_else(|e| panic!("{} が無い: {e}", path.display())),
            expected.data,
            "{} の中身が一致しない",
            expected.path
        );
    }
    // 削除済みは対象外 (それは `ofr restore` の仕事)。
    assert!(!dest.join("DOCS/NOTES.TXT").exists());

    // レポートは JSON と人間向けテキストの 2 つ。
    let json = std::fs::read_to_string(dest.join("ofr-copy-report.json")).unwrap();
    assert!(json.contains("\"status\": \"copied\""), "{json}");
    assert!(json.contains("\"complete\": true"), "{json}");
    let summary = std::fs::read_to_string(dest.join("ofr-copy-report.txt")).unwrap();
    assert!(summary.contains("コピー済み: 3 件"), "{summary}");
}

#[test]
fn copy_can_include_deleted_entries() {
    let dir = tempfile::tempdir().unwrap();
    let scenario = scenarios::exfat_deleted();
    let image = write_image(dir.path(), "exfat.img", &scenario);
    let dest = dir.path().join("mirror");

    let output = ofr(&[
        "copy",
        image.to_str().unwrap(),
        dest.to_str().unwrap(),
        "--include-deleted",
    ]);
    assert_eq!(output.status.code(), Some(EXIT_OK), "{}", stdout(&output));

    // exFAT は削除しても名前が欠けないので、元の場所にそのまま戻る。
    let notes = scenario.file("/DOCS/NOTES.TXT").unwrap();
    assert_eq!(
        std::fs::read(dest.join("DOCS/NOTES.TXT")).unwrap(),
        notes.data
    );
}

#[test]
fn copy_dry_run_lists_a_mounted_folder_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("volume");
    std::fs::create_dir_all(root.join("DCIM")).unwrap();
    std::fs::write(root.join("DCIM/a.jpg"), vec![1u8; 2048]).unwrap();
    let dest = dir.path().join("mirror");

    let output = ofr(&[
        "copy",
        root.to_str().unwrap(),
        dest.to_str().unwrap(),
        "--dry-run",
    ]);
    assert_eq!(output.status.code(), Some(EXIT_OK), "{}", stdout(&output));

    let text = stdout(&output);
    assert!(
        text.contains("OS のファイル API で読む論理コピー"),
        "{text}"
    );
    assert!(text.contains("/DCIM/a.jpg"), "{text}");
    assert!(!dest.exists(), "--dry-run なのに宛先が作られている");
}

#[test]
fn copy_refuses_a_destination_on_the_source_device() {
    // PLAN.md 6章 2項。マウント済みフォルダのコピーでは、宛先が同じディスク上に
    // あれば止める (テンポラリの中は復旧元も宛先も同じディスクなので必ず引っかかる)。
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("volume");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), b"x").unwrap();

    let output = ofr(&[
        "copy",
        root.to_str().unwrap(),
        dir.path().join("mirror").to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("同じデバイス上"), "{stderr}");
}
