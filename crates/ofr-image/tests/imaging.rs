//! イメージングエンジンの結合テスト。
//!
//! 壊れた USB メモリは CI に置けないので、不良スキップ・リトライ・再開は
//! すべて `MockDevice` のエラー注入で検証する(PLAN.md 9章)。

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use ofr_device::{Device, MockDevice};
use ofr_image::{BlockStatus, ImageOptions, Imager, MapFile};

const SECTOR: u64 = 512;
const DEVICE_SIZE: u64 = 64 * 1024;

/// テストが待たされないよう、待ち時間は 0 にしておく。
fn options() -> ImageOptions {
    ImageOptions {
        chunk_size: 4096,
        min_chunk_size: SECTOR,
        sector_size: Some(SECTOR as u32),
        retry_delay: Duration::ZERO,
        max_retry_delay: Duration::ZERO,
        progress_interval: Duration::ZERO,
        map_save_interval: Duration::ZERO,
        ..ImageOptions::default()
    }
}

fn image_paths(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    (dir.join("dev.img"), dir.join("dev.img.map"))
}

/// イメージの中身がデバイスと一致するか、指定範囲だけ確かめる。
fn assert_image_matches(image: &Path, device: &MockDevice, skip: &[(u64, u64)]) {
    let got = fs::read(image).unwrap();
    assert_eq!(got.len() as u64, device.len(), "イメージ長がデバイスと違う");
    for (i, (a, b)) in got.iter().zip(device.data()).enumerate() {
        let pos = i as u64;
        if skip.iter().any(|(s, l)| pos >= *s && pos < s + l) {
            continue;
        }
        assert_eq!(a, b, "offset {pos} の中身が違う");
    }
}

#[test]
fn images_a_healthy_device_completely() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let device = MockDevice::patterned(DEVICE_SIZE);

    let summary = Imager::new(&device)
        .with_options(options())
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.is_complete());
    assert_eq!(summary.rescued, DEVICE_SIZE);
    assert_eq!(summary.bad, 0);
    assert_eq!(summary.errors, 0);
    assert!(!summary.cancelled);
    assert_image_matches(&img, &device, &[]);

    let saved = MapFile::load(&map).unwrap();
    assert_eq!(saved.blocks.rescued(), DEVICE_SIZE);
    assert_eq!(saved.blocks.blocks().len(), 1);
}

#[test]
fn narrows_bad_regions_down_to_the_failing_sectors() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    // 1 セクタだけ壊れているデバイス。コピーパスは 4KiB 単位なので、
    // 最初は 4KiB 丸ごと不良扱いになり、トリムとスクレイプで 512B まで絞られるはず。
    let bad_at = 20 * 1024;
    let device = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .bad_range(bad_at, SECTOR)
        .build();

    let summary = Imager::new(&device)
        .with_options(options())
        .run(&img, Some(&map))
        .unwrap();

    assert!(!summary.is_complete());
    assert_eq!(summary.bad, SECTOR, "不良域が 1 セクタまで絞られていない");
    assert_eq!(summary.rescued, DEVICE_SIZE - SECTOR);
    assert!(summary.errors > 0);

    // 不良セクタ以外は全部イメージに入っている。
    assert_image_matches(&img, &device, &[(bad_at, SECTOR)]);

    let saved = MapFile::load(&map).unwrap();
    assert_eq!(saved.blocks.status_at(bad_at), Some(BlockStatus::BadSector));
    assert_eq!(
        saved.blocks.status_at(bad_at - 1),
        Some(BlockStatus::Finished)
    );
    assert_eq!(
        saved.blocks.status_at(bad_at + SECTOR),
        Some(BlockStatus::Finished)
    );
    // 途中状態(未トリム・未スクレイプ)は残らない。
    assert_eq!(saved.blocks.bytes_with(BlockStatus::NonTrimmed), 0);
    assert_eq!(saved.blocks.bytes_with(BlockStatus::NonScraped), 0);
    assert_eq!(saved.blocks.bytes_with(BlockStatus::NonTried), 0);
}

#[test]
fn recovers_sectors_that_fail_only_a_few_times() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    // 3 回失敗してから読めるようになる領域。リトライパスで拾えるはず。
    let device = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .transient_range(8192, SECTOR, 3)
        .build();

    let summary = Imager::new(&device)
        .with_options(options())
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.is_complete(), "リトライで回収しきれていない");
    assert!(summary.errors > 0, "一度も失敗していないならテストが無意味");
    assert_image_matches(&img, &device, &[]);
}

#[test]
fn reopens_the_handle_when_reads_keep_failing() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    // ハンドルを開き直すまで読めない領域(USB コントローラが固まったケース)。
    let device = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .stuck_until_reopen(4096, 8192)
        .build();

    let summary = Imager::new(&device)
        .with_options(options())
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.reopens > 0, "開き直しが試みられていない");
    assert_eq!(device.stats().reopen_calls, u64::from(summary.reopens));
    assert!(summary.is_complete(), "開き直したのに回収できていない");
    assert_image_matches(&img, &device, &[]);
}

#[test]
fn resumes_from_a_mapfile_without_rereading() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let device = MockDevice::patterned(DEVICE_SIZE);

    // 半分ほど進んだところでキャンセルする。
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    let summary = Imager::new(&device)
        .with_options(options())
        .with_cancel(Arc::clone(&cancel))
        .with_progress(move |p| {
            if p.rescued >= DEVICE_SIZE / 2 {
                flag.store(true, Ordering::Relaxed);
            }
        })
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.cancelled);
    assert!(!summary.is_complete());
    let partial = summary.rescued;
    assert!((DEVICE_SIZE / 2..DEVICE_SIZE).contains(&partial));

    // 中断時点の mapfile が残っている。
    let saved = MapFile::load(&map).unwrap();
    assert_eq!(saved.blocks.rescued(), partial);

    // 再開すると、取得済み領域は読み直さない。
    device.reset_stats();
    let summary = Imager::new(&device)
        .with_options(options())
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.is_complete());
    assert_eq!(
        device.stats().bytes_read,
        DEVICE_SIZE - partial,
        "取得済み領域を読み直している"
    );
    assert_image_matches(&img, &device, &[]);
}

#[test]
fn keeps_the_image_length_even_when_nothing_can_be_read() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    // 全域が不良のデバイス。1 セクタに固執せず最後まで走り切ること。
    let device = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .bad_range(0, DEVICE_SIZE)
        .build();

    let summary = Imager::new(&device)
        .with_options(ImageOptions {
            retries: 1,
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap();

    assert_eq!(summary.rescued, 0);
    assert_eq!(summary.bad, DEVICE_SIZE);
    // 未取得領域は穴のまま。ファイル長だけはデバイスと同じにする。
    assert_eq!(fs::metadata(&img).unwrap().len(), DEVICE_SIZE);

    let saved = MapFile::load(&map).unwrap();
    assert_eq!(saved.blocks.bytes_with(BlockStatus::BadSector), DEVICE_SIZE);
}

#[test]
fn reports_progress_at_most_once_per_interval() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let device = MockDevice::patterned(DEVICE_SIZE);

    let calls = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&calls);
    let summary = Imager::new(&device)
        .with_options(ImageOptions {
            // イベント洪水で GUI が固まらないよう間引く(PLAN.md 5.7)。
            progress_interval: Duration::from_secs(3600),
            ..options()
        })
        .with_progress(move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
        })
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.is_complete());
    // 間引かれるので、パス開始と完了時の強制発火だけが残る。
    let n = calls.load(Ordering::Relaxed);
    assert!(n > 0 && n <= 8, "進捗イベントが間引かれていない: {n} 回");
}

#[test]
fn refuses_a_mapfile_from_a_different_device() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());

    let small = MockDevice::patterned(4096);
    Imager::new(&small)
        .with_options(options())
        .run(&img, Some(&map))
        .unwrap();

    // サイズの違うデバイスに同じ mapfile を使うと弾かれる。
    let big = MockDevice::patterned(DEVICE_SIZE);
    let err = Imager::new(&big)
        .with_options(options())
        .run(&img, Some(&map))
        .unwrap_err();
    assert!(
        matches!(err, ofr_image::ImageError::MapMismatch { .. }),
        "想定と違うエラー: {err}"
    );
}
