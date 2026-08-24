//! MockDevice のエラー注入と Device trait の契約のテスト。
#![cfg(feature = "mock")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use ofr_device::{Device, DeviceError, DeviceKind, MockDevice, Route};

#[test]
fn reads_patterned_data() {
    let dev = MockDevice::patterned(8192);
    let mut buf = vec![0u8; 1024];

    assert_eq!(dev.read_at(2048, &mut buf).unwrap(), 1024);
    for (i, b) in buf.iter().enumerate() {
        assert_eq!(*b, MockDevice::pattern_byte(2048 + i as u64), "offset {i}");
    }
}

#[test]
fn read_past_end_is_clamped_not_an_error() {
    let dev = MockDevice::patterned(1000);
    let mut buf = vec![0u8; 512];

    // 末尾をまたぐ読み込みは切り詰められる。
    assert_eq!(dev.read_at(800, &mut buf).unwrap(), 200);
    // 末尾以降は 0 バイト。
    assert_eq!(dev.read_at(1000, &mut buf).unwrap(), 0);
    assert_eq!(dev.read_at(u64::from(u32::MAX), &mut buf).unwrap(), 0);
}

#[test]
fn read_exact_at_reports_eof() {
    let dev = MockDevice::patterned(1000);
    let mut buf = vec![0u8; 512];

    let err = dev.read_exact_at(800, &mut buf).unwrap_err();
    match err {
        DeviceError::UnexpectedEof {
            offset,
            needed,
            got,
        } => {
            assert_eq!((offset, needed, got), (800, 512, 200));
        }
        other => panic!("想定外のエラー: {other}"),
    }
}

#[test]
fn hard_bad_range_always_fails() {
    let dev = MockDevice::builder(8192)
        .pattern()
        .bad_range(4096, 1024)
        .build();
    let mut buf = vec![0u8; 512];

    for _ in 0..5 {
        let err = dev.read_at(4096, &mut buf).unwrap_err();
        assert!(err.is_media(), "メディアエラーであること: {err}");
        assert!(err.is_retryable());
        assert_eq!(err.offset(), Some(4096));
    }

    // 不良域に少しでも重なれば失敗する。
    assert!(dev.read_at(3840, &mut buf).is_err());
    assert!(dev.read_at(4864, &mut buf).is_err());
    // 隣接するだけなら成功する。
    assert_eq!(dev.read_at(3584, &mut buf).unwrap(), 512);
    assert_eq!(dev.read_at(5120, &mut buf).unwrap(), 512);

    let stats = dev.stats();
    assert_eq!(stats.failed_reads, 7);
    assert_eq!(stats.bytes_read, 1024);
}

#[test]
fn transient_range_succeeds_after_configured_failures() {
    let dev = MockDevice::builder(4096)
        .pattern()
        .transient_range(1024, 512, 2)
        .build();
    let mut buf = vec![0u8; 512];

    assert!(dev.read_at(1024, &mut buf).unwrap_err().is_media());
    assert!(dev.read_at(1024, &mut buf).unwrap_err().is_media());
    assert_eq!(dev.read_at(1024, &mut buf).unwrap(), 512);
    assert_eq!(dev.read_at(1024, &mut buf).unwrap(), 512);

    for (i, b) in buf.iter().enumerate() {
        assert_eq!(*b, MockDevice::pattern_byte(1024 + i as u64));
    }
    assert_eq!(dev.stats().failed_reads, 2);
}

#[test]
fn slow_range_delays_but_succeeds() {
    let dev = MockDevice::builder(4096)
        .pattern()
        .slow_range(2048, 512, Duration::from_millis(60))
        .build();
    let mut buf = vec![0u8; 512];

    let t0 = Instant::now();
    assert_eq!(dev.read_at(0, &mut buf).unwrap(), 512);
    let fast = t0.elapsed();

    let t1 = Instant::now();
    assert_eq!(dev.read_at(2048, &mut buf).unwrap(), 512);
    let slow = t1.elapsed();

    assert!(
        slow >= Duration::from_millis(50),
        "遅延が効いていない: {slow:?}"
    );
    assert!(fast < slow);
}

#[test]
fn alignment_can_be_required() {
    let dev = MockDevice::builder(8192)
        .block_size(512)
        .require_alignment(true)
        .build();
    let mut buf = vec![0u8; 512];

    assert_eq!(dev.read_at(1024, &mut buf).unwrap(), 512);

    let err = dev.read_at(1025, &mut buf).unwrap_err();
    assert!(matches!(err, DeviceError::Unaligned { .. }), "{err}");
    assert!(!err.is_retryable(), "呼び出し側のバグはリトライ対象外");

    let mut odd = vec![0u8; 500];
    assert!(matches!(
        dev.read_at(0, &mut odd).unwrap_err(),
        DeviceError::Unaligned { .. }
    ));
}

#[test]
fn max_read_len_rejects_oversized_reads() {
    let dev = MockDevice::builder(1 << 20).max_read_len(64 * 1024).build();

    let mut ok = vec![0u8; 64 * 1024];
    assert_eq!(dev.read_at(0, &mut ok).unwrap(), 64 * 1024);

    let mut too_big = vec![0u8; 64 * 1024 + 1];
    let err = dev.read_at(0, &mut too_big).unwrap_err();
    assert!(!err.is_retryable(), "{err}");
}

#[test]
fn records_read_pattern() {
    let dev = MockDevice::builder(4096).record_reads(true).build();
    let mut buf = vec![0u8; 1024];

    for off in (0..4096).step_by(1024) {
        dev.read_at(off, &mut buf).unwrap();
    }

    assert_eq!(
        dev.reads(),
        vec![(0, 1024), (1024, 1024), (2048, 1024), (3072, 1024)]
    );

    dev.clear_reads();
    assert!(dev.reads().is_empty());
}

#[test]
fn stats_can_be_reset() {
    let dev = MockDevice::builder(4096).bad_range(0, 512).build();
    let mut buf = vec![0u8; 512];

    let _ = dev.read_at(0, &mut buf);
    let _ = dev.read_at(1024, &mut buf);
    assert_eq!(dev.stats().read_calls, 2);

    dev.reset_stats();
    assert_eq!(dev.stats(), Default::default());
}

#[test]
fn device_info_marks_system_disk_unselectable() {
    let removable = MockDevice::builder(4096).name("usb").build();
    assert_eq!(removable.info().kind, DeviceKind::Mock);
    assert!(removable.info().removable);
    assert!(removable.info().is_selectable_as_source());

    let system = MockDevice::builder(4096)
        .name("boot")
        .removable(false)
        .system_disk(true)
        .build();
    assert!(
        !system.info().is_selectable_as_source(),
        "起動ディスクは復旧元に選ばせない (PLAN.md 6章 3項)"
    );
}

#[test]
fn works_through_shared_references_and_boxes() {
    let dev: Box<dyn Device> = Box::new(MockDevice::patterned(4096));
    let mut buf = vec![0u8; 16];
    assert_eq!(dev.read_at(0, &mut buf).unwrap(), 16);

    let shared: Arc<dyn Device> = Arc::new(MockDevice::patterned(4096));
    assert_eq!(shared.read_vec_at(32, 16).unwrap().len(), 16);
}

#[test]
fn is_shareable_across_threads() {
    let dev = Arc::new(MockDevice::patterned(1 << 16));
    let mut handles = Vec::new();

    for t in 0..4u64 {
        let dev = Arc::clone(&dev);
        handles.push(std::thread::spawn(move || {
            let offset = t * 4096;
            let buf = dev.read_vec_at(offset, 4096).unwrap();
            for (i, b) in buf.iter().enumerate() {
                assert_eq!(*b, MockDevice::pattern_byte(offset + i as u64));
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(dev.stats().bytes_read, 4 * 4096);
}

#[test]
fn empty_device_reads_nothing() {
    let dev = MockDevice::zeroed(0);
    assert!(dev.is_empty());
    assert_eq!(dev.read_at(0, &mut [0u8; 16]).unwrap(), 0);
}

#[test]
fn explicit_data_defines_size() {
    let dev = MockDevice::builder(0).data(b"OFR".to_vec()).build();
    assert_eq!(dev.len(), 3);
    assert_eq!(dev.read_vec_at(0, 3).unwrap(), b"OFR");
    assert_eq!(dev.data(), b"OFR");
}

/// 別経路でだけ読める領域(PLAN.md 11.5)。
#[test]
fn alternate_route_reads_what_the_primary_cannot() {
    let dev = MockDevice::builder(4096)
        .pattern()
        .alternate_range(1024, 512)
        .build();
    assert!(dev.has_alternate());

    let mut buf = [0u8; 512];
    assert!(dev.read_at(1024, &mut buf).unwrap_err().is_media());
    assert_eq!(
        dev.read_at_via(Route::Alternate, 1024, &mut buf).unwrap(),
        512
    );
    assert_eq!(buf[0], MockDevice::pattern_byte(1024));
    assert_eq!(dev.stats().alternate_calls, 1);

    // 不良のない範囲は、どちらの経路でも読める。
    assert_eq!(dev.read_at(0, &mut buf).unwrap(), 512);
    assert_eq!(dev.read_at_via(Route::Primary, 0, &mut buf).unwrap(), 512);
}

/// 中身が揺れる領域は、読むたびに違う値を返す(PLAN.md 11.5 の多数決の検証用)。
#[test]
fn unstable_range_changes_between_reads() {
    let dev = MockDevice::builder(4096)
        .pattern()
        .unstable_range(0, 512, 2)
        .build();

    let first = dev.read_vec_at(0, 512).unwrap();
    let second = dev.read_vec_at(0, 512).unwrap();
    assert_ne!(first, second, "揺れていない");

    // 3 回目からは落ち着き、本来の中身に戻る。
    let third = dev.read_vec_at(0, 512).unwrap();
    assert_eq!(third, dev.read_vec_at(0, 512).unwrap());
    assert_eq!(third[0], MockDevice::pattern_byte(0));
}

/// 別経路を持たないデバイスでは、経路を指定しても普段どおり読む。
#[test]
fn devices_without_an_alternate_ignore_the_route() {
    let dev = MockDevice::patterned(4096);
    assert!(!dev.has_alternate());
    let mut buf = [0u8; 16];
    assert_eq!(dev.read_at_via(Route::Alternate, 0, &mut buf).unwrap(), 16);
    assert_eq!(buf[0], MockDevice::pattern_byte(0));
}
