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
        skip_size_min: 4096,
        skip_size_max: 16384,
        probe_size_max: 4096,
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

/// 大きな死に領域を、コピーパスが読み飛ばして先へ進むこと(PLAN.md 11.2c)。
///
/// 死に領域を舐め切るまでの失敗の総数は変わらない(いつかは全部に触るため)。
/// 変わるのは順番で、**死に領域の向こう側にある読める領域を先に確保する**。
/// 壊れかけメディアでは 1 回の失敗に数秒かかるので、この違いがそのまま
/// 「途中で力尽きたときにどれだけ手元に残るか」になる。
#[test]
fn skips_ahead_over_a_large_dead_area() {
    const SIZE: u64 = 1 << 20;
    const DEAD_AT: u64 = 128 * 1024;
    const DEAD_LEN: u64 = 512 * 1024;
    const PAST_DEAD: u64 = DEAD_AT + DEAD_LEN;

    /// コピーパスだけを走らせて、(死に領域の先に到達するまでの読み込み回数, 取得量)。
    fn copy_only(skip_size_max: u64) -> (usize, u64) {
        let dir = tempfile::tempdir().unwrap();
        let (img, map) = image_paths(dir.path());
        let device = MockDevice::builder(SIZE)
            .pattern()
            .bad_range(DEAD_AT, DEAD_LEN)
            .record_reads(true)
            .build();

        let summary = Imager::new(&device)
            .with_options(ImageOptions {
                skip_size_max,
                trim: false,
                scrape: false,
                retry: false,
                ..options()
            })
            .run(&img, Some(&map))
            .unwrap();

        let reached = device
            .reads()
            .iter()
            .position(|(offset, _)| *offset >= PAST_DEAD)
            .expect("死に領域の先を読んでいない");
        (reached, summary.rescued)
    }

    let (skipping, skipping_rescued) = copy_only(64 * 1024);
    let (plain, plain_rescued) = copy_only(0);

    assert_eq!(
        skipping_rescued,
        SIZE - DEAD_LEN,
        "読める領域を取り逃している"
    );
    assert_eq!(
        skipping_rescued, plain_rescued,
        "読み飛ばしで取得量が変わってはいけない"
    );
    assert!(
        skipping * 2 < plain,
        "読み飛ばしが効いていない: {skipping} 回目で到達 vs {plain} 回目"
    );
}

/// 死に領域が広いときだけ、スクレイプの探索単位を広げること(PLAN.md 11.2b)。
#[test]
fn widens_the_probe_inside_a_large_dead_area() {
    const DEAD_AT: u64 = 16 * 1024;
    const DEAD_LEN: u64 = 16 * 1024;

    // スクレイプまでを走らせて、失敗した読み込みの回数と不良バイト数を測る。
    fn scrape_only(probe_size_max: u64) -> (u64, u64) {
        let dir = tempfile::tempdir().unwrap();
        let (img, map) = image_paths(dir.path());
        let device = MockDevice::builder(DEVICE_SIZE)
            .pattern()
            .bad_range(DEAD_AT, DEAD_LEN)
            .build();

        let summary = Imager::new(&device)
            .with_options(ImageOptions {
                probe_size_max,
                retry: false,
                ..options()
            })
            .run(&img, Some(&map))
            .unwrap();

        (device.stats().failed_reads, summary.bad)
    }

    let (wide_reads, wide_bad) = scrape_only(4096);
    let (fine_reads, fine_bad) = scrape_only(SECTOR);

    assert_eq!(wide_bad, DEAD_LEN, "死に領域を不良として記録できていない");
    assert_eq!(
        wide_bad, fine_bad,
        "探索単位で不良域の広さが変わってはいけない"
    );
    assert!(
        wide_reads * 2 < fine_reads,
        "探索単位が広がっていない: {wide_reads} 回 vs {fine_reads} 回"
    );
}

/// 不良セクタが数個だけなら、探索単位を広げずセクタ単位まで絞ること。
///
/// [`widens_the_probe_inside_a_large_dead_area`] の裏返し。普通の壊れ方で
/// 精度を落とさないことを確かめる。
#[test]
fn keeps_small_bad_areas_exact() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let bad_at = 20 * 1024;
    let device = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .bad_range(bad_at, SECTOR)
        .build();

    let summary = Imager::new(&device)
        // 探索単位の上限を広く取っても、狭い不良域は総当たりされる。
        .with_options(ImageOptions {
            probe_size_max: 32 * 1024,
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap();

    assert_eq!(summary.bad, SECTOR, "不良域が 1 セクタまで絞られていない");
    assert_image_matches(&img, &device, &[(bad_at, SECTOR)]);
}

/// 読み込みが返らなくなっても、見捨てて先へ進めること(PLAN.md 11.2a)。
#[test]
fn keeps_going_when_reads_never_return() {
    use ofr_device::{DeviceFactory, TimeoutDevice};

    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let hang_at = 8192;

    // この範囲を読んだスレッドは戻らない。開き直しても同じ。
    let factory: DeviceFactory = Box::new(move || {
        let device = MockDevice::builder(DEVICE_SIZE)
            .pattern()
            .hang_range(hang_at, SECTOR)
            .build();
        Ok(Box::new(device) as Box<dyn Device>)
    });
    let device = TimeoutDevice::new(factory, Duration::from_millis(100), 64).unwrap();

    let summary = Imager::new(&device)
        .with_options(ImageOptions {
            retries: 0,
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.timeouts > 0, "タイムアウトが記録されていない");
    assert!(!summary.is_complete());
    assert_eq!(summary.bad, SECTOR, "固まった範囲だけが不良のはず");
    assert_eq!(
        summary.rescued,
        DEVICE_SIZE - SECTOR,
        "他の領域が取れていない"
    );

    // 固まった範囲以外はイメージに入っている。
    let reference = MockDevice::patterned(DEVICE_SIZE);
    assert_image_matches(&img, &reference, &[(hang_at, SECTOR)]);
}

/// 応答しない読み込みが続いたら、mapfile を残して打ち切ること(PLAN.md 11.2a)。
#[test]
fn stops_when_the_device_stops_responding_entirely() {
    use ofr_device::{DeviceFactory, TimeoutDevice};

    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());

    // 4KiB 以降が全部無応答。見捨てる上限は 2 本。
    let factory: DeviceFactory = Box::new(move || {
        let device = MockDevice::builder(DEVICE_SIZE)
            .pattern()
            .hang_range(4096, DEVICE_SIZE - 4096)
            .build();
        Ok(Box::new(device) as Box<dyn Device>)
    });
    let device = TimeoutDevice::new(factory, Duration::from_millis(100), 2).unwrap();

    let err = Imager::new(&device)
        .with_options(options())
        .run(&img, Some(&map))
        .unwrap_err();
    assert!(
        matches!(&err, ofr_image::ImageError::Device(e) if matches!(e, ofr_device::DeviceError::Unresponsive { .. })),
        "想定と違うエラー: {err}"
    );

    // 打ち切っても、そこまでの成果と mapfile は残る。
    let saved = MapFile::load(&map).unwrap();
    assert_eq!(
        saved.blocks.rescued(),
        4096,
        "取得済み領域が記録されていない"
    );
    assert_eq!(fs::metadata(&img).unwrap().len(), DEVICE_SIZE);
}

/// 抜き差しで戻ってきたら、同じ所から読み直して続けられること(PLAN.md 11.3)。
#[test]
fn resumes_after_the_device_comes_back() {
    use ofr_device::{DeviceFactory, TimeoutDevice};
    use ofr_image::{Stall, StallAction};

    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let hang_at = 8192;

    // 「抜き差しされたか」を表すフラグ。真の間は読み込みが返らない。
    let wedged = Arc::new(AtomicBool::new(true));

    let stuck = Arc::clone(&wedged);
    let factory: DeviceFactory = Box::new(move || {
        let builder = MockDevice::builder(DEVICE_SIZE).pattern();
        let device = if stuck.load(Ordering::Relaxed) {
            builder.hang_range(hang_at, SECTOR).build()
        } else {
            builder.build()
        };
        Ok(Box::new(device) as Box<dyn Device>)
    });
    // 2 回見捨てたら「応答しない」と判断させる。
    let device = TimeoutDevice::new(factory, Duration::from_millis(100), 2).unwrap();

    let replugged = Arc::clone(&wedged);
    let stalls = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&stalls);

    let summary = Imager::new(&device)
        .with_options(options())
        // 抜き差しの代わり。ここでフラグを倒すと、開き直した先は健全なデバイスになる。
        .with_stall_handler(move |stall: &Stall| {
            counter.fetch_add(1, Ordering::Relaxed);
            assert!(stall.attempt >= 1);
            replugged.store(false, Ordering::Relaxed);
            StallAction::Resume
        })
        .run(&img, Some(&map))
        .unwrap();

    assert!(
        stalls.load(Ordering::Relaxed) > 0,
        "足踏みが通知されていない"
    );
    assert_eq!(summary.stalls, stalls.load(Ordering::Relaxed) as u32);
    assert!(summary.timeouts > 0, "見捨てた読み込みが記録されていない");
    assert!(
        summary.is_complete(),
        "戻ってきたのに全域を取得できていない (取得 {} / {})",
        summary.rescued,
        summary.total
    );

    let reference = MockDevice::patterned(DEVICE_SIZE);
    assert_image_matches(&img, &reference, &[]);
}

/// 抜き差しを待たない(待っても戻らなかった)場合は、その場で打ち切ること。
#[test]
fn aborts_when_the_stall_handler_gives_up() {
    use ofr_device::{DeviceFactory, TimeoutDevice};
    use ofr_image::{Stall, StallAction};

    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());

    let factory: DeviceFactory = Box::new(move || {
        let device = MockDevice::builder(DEVICE_SIZE)
            .pattern()
            .hang_range(4096, DEVICE_SIZE - 4096)
            .build();
        Ok(Box::new(device) as Box<dyn Device>)
    });
    let device = TimeoutDevice::new(factory, Duration::from_millis(100), 2).unwrap();

    let err = Imager::new(&device)
        .with_options(options())
        .with_stall_handler(|_: &Stall| StallAction::Abort)
        .run(&img, Some(&map))
        .unwrap_err();

    assert!(
        matches!(&err, ofr_image::ImageError::Device(e) if matches!(e, ofr_device::DeviceError::Unresponsive { .. })),
        "想定と違うエラー: {err}"
    );
    // 打ち切っても、そこまでの成果と mapfile は残る。
    assert_eq!(MapFile::load(&map).unwrap().blocks.rescued(), 4096);
}

/// `retry_only`: 読めなかった領域だけを狙い、未試行の領域には触らないこと(PLAN.md 11.3)。
#[test]
fn retries_only_the_bad_areas_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let bad_at = 20 * 1024;

    // 1 回目。1 セクタが読めないまま終わる。
    let broken = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .bad_range(bad_at, SECTOR)
        .build();
    let first = Imager::new(&broken)
        .with_options(options())
        .run(&img, Some(&map))
        .unwrap();
    assert_eq!(first.bad, SECTOR);

    // 2 回目。抜き差しして読めるようになった想定で、不良域だけを狙う。
    let healed = MockDevice::patterned(DEVICE_SIZE);
    let second = Imager::new(&healed)
        .with_options(ImageOptions {
            retry_only: true,
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap();

    assert!(second.is_complete(), "不良域を回収できていない");
    assert_eq!(
        healed.stats().bytes_read,
        SECTOR,
        "取得済みの領域まで読み直している"
    );
    assert_image_matches(&img, &healed, &[]);
}

/// `retry_only` は前回の記録がないと使えないこと。
#[test]
fn retry_only_needs_a_mapfile() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let device = MockDevice::patterned(DEVICE_SIZE);

    let err = Imager::new(&device)
        .with_options(ImageOptions {
            retry_only: true,
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap_err();

    assert!(
        matches!(err, ofr_image::ImageError::InvalidOptions(_)),
        "想定と違うエラー: {err}"
    );
}

/// `reset_bad`: 不良の記録を捨ててやり直すこと(PLAN.md 11.3)。
#[test]
fn resets_bad_marks_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let bad_at = 20 * 1024;

    let broken = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .bad_range(bad_at, SECTOR)
        .build();
    Imager::new(&broken)
        .with_options(options())
        .run(&img, Some(&map))
        .unwrap();
    assert_eq!(
        MapFile::load(&map).unwrap().blocks.status_at(bad_at),
        Some(BlockStatus::BadSector)
    );

    let healed = MockDevice::patterned(DEVICE_SIZE);
    let summary = Imager::new(&healed)
        .with_options(ImageOptions {
            reset_bad: true,
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.is_complete());
    assert_eq!(
        healed.stats().bytes_read,
        SECTOR,
        "取得済みの領域まで読み直している"
    );
    let saved = MapFile::load(&map).unwrap();
    assert_eq!(saved.blocks.bytes_with(BlockStatus::BadSector), 0);
}
