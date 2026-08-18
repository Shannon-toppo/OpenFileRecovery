//! FileDevice のテスト。イメージファイルを Device として読めることを確認する。

use std::io::Write;

use ofr_device::{Device, DeviceError, DeviceKind, FileDevice};
use tempfile::NamedTempFile;

fn temp_image(size: usize) -> (NamedTempFile, Vec<u8>) {
    let content: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let mut f = NamedTempFile::new().expect("一時ファイル作成");
    f.write_all(&content).expect("書き込み");
    f.flush().expect("flush");
    (f, content)
}

#[test]
fn reads_image_file() {
    let (f, content) = temp_image(8192);
    let dev = FileDevice::open(f.path()).unwrap();

    assert_eq!(dev.len(), 8192);
    assert_eq!(dev.block_size(), 512);
    assert_eq!(dev.info().kind, DeviceKind::ImageFile);
    assert!(!dev.is_empty());

    let mut buf = vec![0u8; 1024];
    assert_eq!(dev.read_at(4096, &mut buf).unwrap(), 1024);
    assert_eq!(buf, content[4096..5120]);

    assert_eq!(dev.read_vec_at(0, 16).unwrap(), content[..16]);
}

#[test]
fn clamps_at_end_of_file() {
    let (f, _) = temp_image(1000);
    let dev = FileDevice::open(f.path()).unwrap();

    let mut buf = vec![0u8; 512];
    assert_eq!(dev.read_at(800, &mut buf).unwrap(), 200);
    assert_eq!(dev.read_at(1000, &mut buf).unwrap(), 0);
    assert!(matches!(
        dev.read_exact_at(900, &mut buf).unwrap_err(),
        DeviceError::UnexpectedEof { .. }
    ));
}

#[test]
fn custom_block_size_is_reported() {
    let (f, _) = temp_image(4096);
    let dev = FileDevice::open_with_block_size(f.path(), 4096).unwrap();
    assert_eq!(dev.block_size(), 4096);
    assert_eq!(dev.info().block_size, 4096);
}

#[test]
fn missing_file_reports_not_found() {
    let err = FileDevice::open("does-not-exist-ofr.img").unwrap_err();
    assert!(matches!(err, DeviceError::NotFound(_)), "{err}");
    assert!(!err.is_retryable());
}

#[test]
fn empty_file_is_empty_device() {
    let f = NamedTempFile::new().unwrap();
    let dev = FileDevice::open(f.path()).unwrap();
    assert!(dev.is_empty());
    assert!(!dev.info().is_selectable_as_source());
}

#[test]
fn concurrent_reads_do_not_interfere() {
    let (f, content) = temp_image(1 << 16);
    let dev = std::sync::Arc::new(FileDevice::open(f.path()).unwrap());

    let mut handles = Vec::new();
    for t in 0..4usize {
        let dev = std::sync::Arc::clone(&dev);
        let expect = content[t * 4096..(t + 1) * 4096].to_vec();
        handles.push(std::thread::spawn(move || {
            for _ in 0..20 {
                let got = dev.read_vec_at((t * 4096) as u64, 4096).unwrap();
                assert_eq!(got, expect);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn path_is_preserved() {
    let (f, _) = temp_image(16);
    let dev = FileDevice::open(f.path()).unwrap();
    assert_eq!(dev.path(), f.path());
    assert_eq!(dev.info().id, f.path().display().to_string());
}
