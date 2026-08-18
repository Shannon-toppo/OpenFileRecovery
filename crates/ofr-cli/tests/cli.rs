//! CLI の結合テスト。Phase 2 の完了条件を、実際のコマンドの形で確かめる。
//!
//! テストイメージは `ofr-testfs` が生成する本物の FAT32 / exFAT なので、
//! ここを通れば「イメージを渡して `ofr scan` → `ofr restore` で中身が戻る」
//! ところまで一続きで検証できたことになる。

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
