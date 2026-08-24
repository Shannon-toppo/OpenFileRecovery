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
use ofr_image::{BlockStatus, Domain, ImageOptions, Imager, MapFile};

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

/// 範囲を限定したら、その外は読まないこと(PLAN.md 11.4)。
#[test]
fn reads_only_the_requested_domain() {
    const AT: u64 = 8 * 1024;
    const LEN: u64 = 4 * 1024;

    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    // 範囲の外に不良を置く。触っていなければ、これは結果に出てこない。
    let device = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .bad_range(0, SECTOR)
        .record_reads(true)
        .build();

    let summary = Imager::new(&device)
        .with_options(ImageOptions {
            domain: Some(Domain::new([(AT, LEN)])),
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.is_complete(), "限定した範囲を取り切れていない");
    assert_eq!(summary.total, LEN, "分母が限定した範囲になっていない");
    assert_eq!(summary.rescued, LEN);
    assert_eq!(summary.bad, 0, "範囲外の不良を拾ってしまっている");

    for (offset, len) in device.reads() {
        assert!(
            offset >= AT && offset + len as u64 <= AT + LEN,
            "範囲外を読んでいる: {offset}+{len}"
        );
    }

    // イメージは全長ぶん確保するが、中身が入るのは限定した範囲だけ。
    let image = fs::read(&img).unwrap();
    assert_eq!(image.len() as u64, DEVICE_SIZE);
    assert_eq!(
        &image[AT as usize..(AT + LEN) as usize],
        &device.data()[AT as usize..(AT + LEN) as usize],
    );
    assert!(image[..AT as usize].iter().all(|&b| b == 0));

    // mapfile 上も、範囲の外は未試行のまま。
    let saved = MapFile::load(&map).unwrap();
    assert_eq!(saved.blocks.status_at(0), Some(BlockStatus::NonTried));
    assert_eq!(saved.blocks.status_at(AT), Some(BlockStatus::Finished));
    assert_eq!(
        saved.blocks.status_at(AT + LEN),
        Some(BlockStatus::NonTried)
    );
}

/// 限定した範囲がセクタ境界に乗っていなければ、外側へ広げて読むこと。
#[test]
fn widens_the_domain_to_whole_sectors() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let device = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .record_reads(true)
        .build();

    // 1 セクタ目の途中から 2 セクタ目の途中まで。2 セクタ丸ごと読めばよい。
    let summary = Imager::new(&device)
        .with_options(ImageOptions {
            domain: Some(Domain::new([(SECTOR + 100, 500)])),
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap();

    assert_eq!(summary.total, 2 * SECTOR);
    assert!(summary.is_complete());
    let image = fs::read(&img).unwrap();
    let range = SECTOR as usize..(3 * SECTOR) as usize;
    assert_eq!(&image[range.clone()], &device.data()[range]);
}

/// デバイスの外だけを指した範囲は設定エラーにすること。
#[test]
fn rejects_a_domain_outside_the_device() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let device = MockDevice::patterned(DEVICE_SIZE);

    let error = Imager::new(&device)
        .with_options(ImageOptions {
            domain: Some(Domain::new([(DEVICE_SIZE, 4096)])),
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap_err();
    assert!(matches!(error, ofr_image::ImageError::InvalidOptions(_)));
}

/// メタデータに指定した範囲を、全体を舐めるより先に確保すること(PLAN.md 11.4)。
#[test]
fn takes_the_priority_regions_first() {
    const AT: u64 = DEVICE_SIZE - 4 * 1024;
    const LEN: u64 = 4 * 1024;

    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let device = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .record_reads(true)
        .build();

    let mut passes = Vec::new();
    let summary = Imager::new(&device)
        .with_options(ImageOptions {
            // 末尾に置く。順に読んだのでは最後になる位置。
            priority: Some(Domain::new([(AT, LEN)])),
            ..options()
        })
        .with_progress(move |p| {
            if passes.last() != Some(&p.pass) {
                passes.push(p.pass);
            }
            if p.pass == ofr_image::Pass::Priority {
                assert_eq!(p.total, DEVICE_SIZE, "分母は全域のまま出す");
            }
        })
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.is_complete(), "優先範囲だけで終わってしまっている");
    let reads = device.reads();
    let (first, len) = reads[0];
    assert_eq!(
        (first, len as u64),
        (AT, LEN),
        "優先した範囲を最初に読んでいない"
    );
    assert!(
        reads[1..].iter().all(|(offset, _)| *offset < AT),
        "優先した範囲を二度読んでいる"
    );
}

/// 優先範囲は限定範囲の中に閉じ込めること。
#[test]
fn keeps_priority_inside_the_domain() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let device = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .record_reads(true)
        .build();

    let summary = Imager::new(&device)
        .with_options(ImageOptions {
            domain: Some(Domain::new([(8192, 4096)])),
            // 半分は限定範囲の外。そこは読んではいけない。
            priority: Some(Domain::new([(6144, 4096)])),
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap();

    assert_eq!(summary.total, 4096);
    assert!(summary.is_complete());
    for (offset, len) in device.reads() {
        assert!(
            offset >= 8192 && offset + len as u64 <= 8192 + 4096,
            "範囲外を読んでいる: {offset}+{len}"
        );
    }
}

/// 通常経路で読めないセクタを、別経路で拾えること(PLAN.md 11.5)。
#[test]
fn recovers_sectors_only_the_alternate_route_can_read() {
    const AT: u64 = 8192;
    const LEN: u64 = 1024;

    fn run(alt_route: bool) -> (ofr_image::ImageSummary, u64) {
        let dir = tempfile::tempdir().unwrap();
        let (img, map) = image_paths(dir.path());
        let device = MockDevice::builder(DEVICE_SIZE)
            .pattern()
            .alternate_range(AT, LEN)
            .build();

        let summary = Imager::new(&device)
            .with_options(ImageOptions {
                alt_route,
                ..options()
            })
            .run(&img, Some(&map))
            .unwrap();
        (summary, device.stats().alternate_calls)
    }

    let (with_alt, alt_calls) = run(true);
    assert!(with_alt.is_complete(), "別経路で拾えていない");
    assert_eq!(with_alt.alt_rescues, LEN, "別経路で拾った量が合わない");
    assert!(alt_calls > 0, "別経路を使っていない");

    // 切れば、そこは不良のまま残る。
    let (without_alt, alt_calls) = run(false);
    assert_eq!(without_alt.bad, LEN);
    assert_eq!(without_alt.alt_rescues, 0);
    assert_eq!(alt_calls, 0, "切ったのに別経路を使っている");
}

/// 別経路でも、拾った中身は元のとおりであること。
#[test]
fn alternate_route_returns_the_real_contents() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let device = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .alternate_range(8192, 1024)
        .build();

    Imager::new(&device)
        .with_options(options())
        .run(&img, Some(&map))
        .unwrap();
    assert_image_matches(&img, &device, &[]);
}

/// 小さい転送を受け付けないメディアから、転送単位を変えて拾えること(PLAN.md 11.5)。
#[test]
fn changes_the_transfer_size_as_a_last_resort() {
    const SIZE: u64 = 8192;
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    // 4KiB 未満の読み込みを受け付けないブリッジ。読み込み単位を 1KiB にして、
    // 通常のパスが全部失敗する状況を作る。
    let device = MockDevice::builder(SIZE)
        .pattern()
        .min_read_len(4096)
        .build();

    let summary = Imager::new(&device)
        .with_options(ImageOptions {
            chunk_size: 1024,
            skip_size_min: 1024,
            skip_size_max: 1024,
            // 探索単位を広げさせない。広げると 4KiB に届いた時点で普通に読めてしまい、
            // 転送単位を変える経路を通らなくなる。
            probe_size_max: SECTOR,
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.is_complete(), "転送単位を変えても拾えていない");
    assert_eq!(summary.resized_rescues, SIZE);
    assert_image_matches(&img, &device, &[]);
}

/// 逆向き指定で、末尾から手前へ読むこと(PLAN.md 11.5)。
#[test]
fn reads_backwards_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let device = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .record_reads(true)
        .build();

    let summary = Imager::new(&device)
        .with_options(ImageOptions {
            reverse: true,
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.is_complete());
    assert_image_matches(&img, &device, &[]);

    let reads = device.reads();
    let (first, len) = reads[0];
    assert_eq!(
        first + len as u64,
        DEVICE_SIZE,
        "末尾から読み始めていない: {first}+{len}"
    );
    assert!(
        reads.windows(2).all(|w| w[0].0 > w[1].0),
        "手前へ向かって読んでいない"
    );
}

/// 読むたびに内容が変わる領域を「不確か」として記録すること(PLAN.md 11.5)。
#[test]
fn records_wobbling_contents_as_uncertain() {
    const AT: u64 = 8192;
    const LEN: u64 = 512;

    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    // 開き直すまで読めない = リトライパスまで残る。読めるようになってからは、
    // 読むたびに中身が変わる。
    let device = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .stuck_until_reopen(AT, LEN)
        .unstable_range(AT, LEN, 0)
        .build();

    let summary = Imager::new(&device)
        .with_options(ImageOptions {
            verify: 3,
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.is_complete(), "リトライで拾えていない");
    assert_eq!(summary.uncertain, LEN, "不確かとして記録していない");

    // mapfile に残り、読み直しても拾える(ddrescue から見ればただのコメント)。
    let saved = MapFile::load(&map).unwrap();
    assert_eq!(saved.uncertain.total(), LEN);
    assert!(saved.uncertain.contains(AT));
}

/// 内容が安定していれば、多数決を掛けても「不確か」にはしないこと。
#[test]
fn stable_contents_are_not_marked_uncertain() {
    let dir = tempfile::tempdir().unwrap();
    let (img, map) = image_paths(dir.path());
    let device = MockDevice::builder(DEVICE_SIZE)
        .pattern()
        .stuck_until_reopen(8192, 512)
        .build();

    let summary = Imager::new(&device)
        .with_options(ImageOptions {
            verify: 3,
            ..options()
        })
        .run(&img, Some(&map))
        .unwrap();

    assert!(summary.is_complete());
    assert_eq!(summary.uncertain, 0);
    assert_image_matches(&img, &device, &[]);
}
