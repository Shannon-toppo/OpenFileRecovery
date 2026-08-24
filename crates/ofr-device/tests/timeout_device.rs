//! 応答が返らない読み込みを見捨てられるか(PLAN.md 11.2)。
//!
//! 固まった USB コントローラは CI に置けないので、`MockDevice` の
//! 無応答注入(`hang_range`)で検証する(PLAN.md 9章)。

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use ofr_device::{Device, DeviceError, DeviceFactory, MockDevice, Route, TimeoutDevice};

const SIZE: u64 = 64 * 1024;
const HANG_AT: u64 = 8192;
const TIMEOUT: Duration = Duration::from_millis(200);

/// 何回目に開かれたかを数えつつデバイスを返す生成関数。
///
/// `hang_until` 回目までは指定範囲で固まるデバイス、それ以降は健全なデバイスを返す。
fn factory(hang_until: u32) -> (DeviceFactory, Arc<AtomicU32>) {
    let opens = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&opens);
    let f: DeviceFactory = Box::new(move || {
        let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
        let builder = MockDevice::builder(SIZE).pattern();
        let device = if n <= hang_until {
            builder.hang_range(HANG_AT, 512).build()
        } else {
            builder.build()
        };
        Ok(Box::new(device) as Box<dyn Device>)
    });
    (f, opens)
}

#[test]
fn healthy_reads_pass_through_unchanged() {
    let (f, opens) = factory(0);
    let dev = TimeoutDevice::new(f, TIMEOUT, 4).unwrap();

    let mut buf = vec![0u8; 4096];
    assert_eq!(dev.read_at(0, &mut buf).unwrap(), 4096);
    for (i, b) in buf.iter().enumerate() {
        assert_eq!(
            *b,
            MockDevice::pattern_byte(i as u64),
            "offset {i} の中身が違う"
        );
    }
    assert_eq!(dev.len(), SIZE);
    assert_eq!(dev.abandoned(), 0, "健全な読み込みで見捨ててはいけない");
    assert_eq!(opens.load(Ordering::Relaxed), 1, "無駄に開き直している");

    // 末尾を越える要求は 0 バイト。
    assert_eq!(dev.read_at(SIZE, &mut buf).unwrap(), 0);
}

#[test]
fn abandons_a_read_that_never_returns() {
    // 1 回目に開いたデバイスだけが固まる。見捨てたあとは健全なデバイスで続く。
    let (f, opens) = factory(1);
    let dev = TimeoutDevice::new(f, TIMEOUT, 4).unwrap();

    let mut buf = vec![0u8; 512];
    let started = Instant::now();
    let err = dev.read_at(HANG_AT, &mut buf).unwrap_err();

    assert!(
        matches!(err, DeviceError::Timeout { offset, .. } if offset == HANG_AT),
        "想定と違うエラー: {err}"
    );
    assert!(
        started.elapsed() < TIMEOUT * 10,
        "制限時間を過ぎても戻ってきていない"
    );
    assert_eq!(dev.abandoned(), 1);

    // 見捨てたあとも先へ進める。デバイスは開き直されている。
    assert_eq!(dev.read_at(0, &mut buf).unwrap(), 512);
    assert_eq!(buf[0], MockDevice::pattern_byte(0));
    assert_eq!(opens.load(Ordering::Relaxed), 2, "開き直していない");
}

#[test]
fn stops_when_too_many_reads_are_abandoned() {
    // 何度開いても固まるデバイス。上限に達したら打ち切る。
    let (f, _) = factory(u32::MAX);
    let dev = TimeoutDevice::new(f, TIMEOUT, 2).unwrap();

    let mut buf = vec![0u8; 512];
    let first = dev.read_at(HANG_AT, &mut buf).unwrap_err();
    assert!(matches!(first, DeviceError::Timeout { .. }), "{first}");

    let second = dev.read_at(HANG_AT, &mut buf).unwrap_err();
    assert!(
        matches!(second, DeviceError::Unresponsive { abandoned: 2 }),
        "上限に達しても打ち切っていない: {second}"
    );
    // 抜き差しを促すために、エラー文面で理由が分かること。
    assert!(second.to_string().contains("応答"));
}

#[test]
fn reopen_opens_the_device_again() {
    let (f, opens) = factory(0);
    let dev = TimeoutDevice::new(f, TIMEOUT, 4).unwrap();

    assert!(dev.reopen().unwrap());
    assert_eq!(opens.load(Ordering::Relaxed), 2);

    let mut buf = vec![0u8; 512];
    assert_eq!(dev.read_at(1024, &mut buf).unwrap(), 512);
    assert_eq!(buf[0], MockDevice::pattern_byte(1024));
}

#[test]
fn is_shareable_across_threads() {
    let (f, _) = factory(0);
    let dev = Arc::new(TimeoutDevice::new(f, TIMEOUT, 4).unwrap());

    let handles: Vec<_> = (0..4)
        .map(|i| {
            let dev = Arc::clone(&dev);
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 512];
                let offset = i * 512;
                assert_eq!(dev.read_at(offset, &mut buf).unwrap(), 512);
                assert_eq!(buf[0], MockDevice::pattern_byte(offset));
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

/// 別経路の読み込みもワーカー越しに、同じ制限時間で行うこと(PLAN.md 11.5)。
#[test]
fn alternate_reads_go_through_the_worker() {
    let f: DeviceFactory = Box::new(|| {
        Ok(Box::new(
            MockDevice::builder(SIZE)
                .pattern()
                .alternate_range(HANG_AT, 512)
                .build(),
        ) as Box<dyn Device>)
    });
    let dev = TimeoutDevice::new(f, TIMEOUT, 4).unwrap();
    assert!(dev.has_alternate(), "別経路を持つデバイスを包んでいる");

    let mut buf = vec![0u8; 512];
    assert!(dev.read_at(HANG_AT, &mut buf).unwrap_err().is_media());
    assert_eq!(
        dev.read_at_via(Route::Alternate, HANG_AT, &mut buf)
            .unwrap(),
        512
    );
    assert_eq!(buf[0], MockDevice::pattern_byte(HANG_AT));
    assert_eq!(dev.abandoned(), 0);
}
