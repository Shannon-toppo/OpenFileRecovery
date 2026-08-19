//! 破損サンプル集に対する修復の回帰テスト(PLAN.md 8章 Phase 5 の完了条件)。
//!
//! 完了条件は「JPEG/PNG/AVI は既定ケースで修復成功。MP4 は参照ファイルありで
//! 再生可能なファイルを出力(自動テストはコンテナ整合性チェックまで)」。
//! ここでは壊し方ごとに 1 本ずつ用意し、
//!
//! - 静止画は「デコードが通り、元の絵と一致するか」
//! - 動画は「索引が実データの中を指しているか」
//!
//! を確かめる。サンプルの作り方は [`support`] にある。

mod support;

use std::path::{Path, PathBuf};

use ofr_repair::{RepairOptions, RepairStatus, Repairer, Verification};

/// 一時ディレクトリにファイルを置く。
fn place(dir: &tempfile::TempDir, name: &str, data: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, data).unwrap();
    path
}

/// 出力を読み込む。
fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

// ---------------------------------------------------------------- JPEG

#[test]
fn jpeg_header_damage_is_repaired_with_a_reference() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::jpeg(160, 120);
    // SOS の手前まで潰す。量子化表もハフマン表もフレームヘッダも失われる。
    let sos = support::find(&healthy, &[0xFF, 0xDA]).unwrap();
    let broken = support::destroy_head(&healthy, sos);

    let input = place(&dir, "broken.jpg", &broken);
    let reference = place(&dir, "reference.jpg", &healthy);
    let output = dir.path().join("fixed.jpg");

    let report = Repairer::new(&input, &output)
        .with_reference(&reference)
        .run()
        .unwrap();

    assert_eq!(report.status, RepairStatus::Repaired, "{report:?}");
    assert!(
        matches!(
            report.verification,
            Verification::Decoded {
                width: 160,
                height: 120
            }
        ),
        "{:?}",
        report.verification
    );

    // 参照から借りたテーブルは元と同じものなので、絵は完全に一致する。
    let fixed = support::decode(&read(&output)).expect("修復結果をデコードできない");
    let original = support::decode(&healthy).unwrap();
    assert_eq!(support::mean_difference(&fixed, &original), 0.0);
}

#[test]
fn jpeg_header_damage_without_a_reference_uses_standard_tables() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::jpeg(160, 120);
    let sos = support::find(&healthy, &[0xFF, 0xDA]).unwrap();
    let input = place(&dir, "broken.jpg", &support::destroy_head(&healthy, sos));
    let output = dir.path().join("fixed.jpg");

    let report = Repairer::new(&input, &output)
        .with_options(RepairOptions {
            width: Some(160),
            height: Some(120),
            ..RepairOptions::default()
        })
        .run()
        .unwrap();

    assert!(report.status.produced_output(), "{report:?}");
    assert!(report.verification.passed(), "{:?}", report.verification);
    // 標準テーブルは元の機器のものと違う。直ったことにしない。
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.contains("標準テーブル") || i.contains("色や階調")),
        "{:?}",
        report.issues
    );
    assert!(support::decode(&read(&output)).is_some());
}

#[test]
fn jpeg_reference_must_be_a_healthy_jpeg() {
    let dir = tempfile::tempdir().unwrap();
    let input = place(&dir, "broken.jpg", &support::jpeg(32, 32));
    let reference = place(&dir, "reference.jpg", b"not a jpeg at all");

    let err = Repairer::new(&input, dir.path().join("out.jpg"))
        .with_reference(&reference)
        .run()
        .unwrap_err();
    assert!(
        matches!(err, ofr_repair::RepairError::Reference { .. }),
        "{err}"
    );
}

// ---------------------------------------------------------------- PNG

#[test]
fn png_bad_crc_is_recomputed() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::png(80, 60);
    let input = place(&dir, "crc.png", &support::break_png_crc(&healthy, b"IDAT"));
    let output = dir.path().join("fixed.png");

    let report = Repairer::new(&input, &output).run().unwrap();
    assert_eq!(report.status, RepairStatus::Repaired, "{report:?}");
    assert!(
        report.fixes.iter().any(|f| f.contains("CRC")),
        "{:?}",
        report.fixes
    );

    // PNG は可逆なので、画素は 1 つも変わらない。
    let fixed = support::decode(&read(&output)).expect("修復結果をデコードできない");
    assert_eq!(
        support::mean_difference(&fixed, &support::decode(&healthy).unwrap()),
        0.0
    );
}

#[test]
fn png_missing_iend_is_appended() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::png(80, 60);
    // IEND チャンク (12 バイト) を落とす。
    let input = place(&dir, "noend.png", &healthy[..healthy.len() - 12]);
    let output = dir.path().join("fixed.png");

    let report = Repairer::new(&input, &output).run().unwrap();
    assert!(report.status.produced_output(), "{report:?}");
    assert!(
        report.fixes.iter().any(|f| f.contains("IEND")),
        "{:?}",
        report.fixes
    );
    let fixed = support::decode(&read(&output)).expect("修復結果をデコードできない");
    assert_eq!(fixed.dimensions(), (80, 60));
}

#[test]
fn png_truncation_keeps_the_rows_that_survived() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::png(80, 60);
    let input = place(&dir, "cut.png", &support::truncate(&healthy, 0.6));
    let output = dir.path().join("fixed.png");

    let report = Repairer::new(&input, &output).run().unwrap();
    assert_eq!(report.status, RepairStatus::Partial, "{report:?}");
    assert!(report.verification.passed(), "{:?}", report.verification);
    assert!(
        report.issues.iter().any(|i| i.contains("失われている")),
        "{:?}",
        report.issues
    );

    let fixed = support::decode(&read(&output)).expect("修復結果をデコードできない");
    let original = support::decode(&healthy).unwrap();
    assert_eq!(fixed.dimensions(), original.dimensions());
    // 上の方の行は元のまま残っている。
    for y in 0..5 {
        for x in 0..original.width() {
            assert_eq!(
                fixed.get_pixel(x, y),
                original.get_pixel(x, y),
                "({x}, {y}) が変わっている"
            );
        }
    }
}

#[test]
fn png_missing_ihdr_is_taken_from_the_reference() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::png(80, 60);
    // IHDR の名前を潰す。中身は残っているが読めなくなる。
    let input = place(
        &dir,
        "noihdr.png",
        &support::rename_tag(&healthy, b"IHDR", b"junk"),
    );
    let reference = place(&dir, "reference.png", &healthy);
    let output = dir.path().join("fixed.png");

    let report = Repairer::new(&input, &output)
        .with_reference(&reference)
        .run()
        .unwrap();

    assert!(report.status.produced_output(), "{report:?}");
    assert!(
        report.fixes.iter().any(|f| f.contains("IHDR")),
        "{:?}",
        report.fixes
    );
    let fixed = support::decode(&read(&output)).expect("修復結果をデコードできない");
    assert_eq!(
        support::mean_difference(&fixed, &support::decode(&healthy).unwrap()),
        0.0
    );
}

#[test]
fn png_intact_file_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::png(40, 30);
    let input = place(&dir, "ok.png", &healthy);
    let output = dir.path().join("copy.png");

    let report = Repairer::new(&input, &output).run().unwrap();
    assert_eq!(report.status, RepairStatus::Intact, "{report:?}");
    assert!(report.fixes.is_empty(), "{:?}", report.fixes);
}

// ---------------------------------------------------------------- AVI

#[test]
fn avi_missing_index_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::avi(30);
    let input = place(
        &dir,
        "noidx.avi",
        &support::remove_riff_chunk(&healthy, b"idx1"),
    );
    let output = dir.path().join("fixed.avi");

    let report = Repairer::new(&input, &output).run().unwrap();
    assert_eq!(report.status, RepairStatus::Repaired, "{report:?}");
    assert_eq!(report.verification, Verification::Container);
    assert!(
        report.fixes.iter().any(|f| f.contains("idx1")),
        "{:?}",
        report.fixes
    );

    let fixed = read(&output);
    let idx1 = support::riff_top(&fixed, b"idx1").expect("idx1 が無い");
    assert_eq!(idx1.len(), 30 * 16, "フレーム数ぶんの索引がある");
    // 索引の先頭は "movi" の 4 文字を 0 とした位置なので 4 から始まる。
    assert_eq!(&idx1[..4], b"00dc");
    assert_eq!(support::le32(&idx1, 8), 4);
}

#[test]
fn avi_broken_header_values_are_measured_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut healthy = support::avi(30);
    // avih の総フレーム数を壊す。hdrl > avih の中身は先頭から 12 + 16 バイト目。
    let avih = support::find(&healthy, b"avih").unwrap();
    healthy[avih + 8 + 16..avih + 8 + 20].copy_from_slice(&9999u32.to_le_bytes());
    let input = place(&dir, "badhdr.avi", &healthy);
    let output = dir.path().join("fixed.avi");

    let report = Repairer::new(&input, &output).run().unwrap();
    assert_eq!(report.status, RepairStatus::Repaired, "{report:?}");
    assert!(
        report.fixes.iter().any(|f| f.contains("総フレーム数")),
        "{:?}",
        report.fixes
    );

    let fixed = read(&output);
    let hdrl = support::riff_top(&fixed, b"hdrl").unwrap();
    assert_eq!(support::le32(&hdrl, 8 + 16), 30);
}

#[test]
fn avi_truncated_file_is_closed_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::avi(30);
    let input = place(&dir, "cut.avi", &support::truncate(&healthy, 0.6));
    let output = dir.path().join("fixed.avi");

    let report = Repairer::new(&input, &output).run().unwrap();
    assert_eq!(report.status, RepairStatus::Partial, "{report:?}");
    assert_eq!(report.verification, Verification::Container);

    // 残ったフレームだけの索引ができている。
    let fixed = read(&output);
    let idx1 = support::riff_top(&fixed, b"idx1").unwrap();
    assert!(!idx1.is_empty() && idx1.len() < 30 * 16, "{}", idx1.len());
    // RIFF のサイズ欄が実際の長さと合っている。
    assert_eq!(support::le32(&fixed, 4) as usize + 8, fixed.len());
}

#[test]
fn avi_intact_file_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::avi(10);
    let input = place(&dir, "ok.avi", &healthy);
    let output = dir.path().join("copy.avi");

    let report = Repairer::new(&input, &output).run().unwrap();
    assert_eq!(report.status, RepairStatus::Intact, "{report:?}");
    assert!(report.fixes.is_empty(), "{:?}", report.fixes);
    assert_eq!(read(&output).len(), healthy.len());
}

// ---------------------------------------------------------------- MP4

#[test]
fn mp4_missing_moov_is_rebuilt_from_a_reference() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::mp4(40);
    let input = place(&dir, "nomoov.mp4", &support::remove_box(&healthy, b"moov"));
    let reference = place(&dir, "reference.mp4", &healthy);
    let output = dir.path().join("fixed.mp4");

    let report = Repairer::new(&input, &output)
        .with_reference(&reference)
        .run()
        .unwrap();

    assert_eq!(report.status, RepairStatus::Repaired, "{report:?}");
    assert_eq!(report.verification, Verification::Container);
    assert!(
        report.fixes.iter().any(|f| f.contains("索引を作り直した")),
        "{:?}",
        report.fixes
    );

    // 索引にフレーム数ぶんのサンプルが載っている。
    let fixed = read(&output);
    let stbl = support::mp4_path(&fixed, &[b"moov", b"trak", b"mdia", b"minf", b"stbl"])
        .expect("stbl が無い");
    let stsz = support::mp4_path(&stbl, &[b"stsz"]).unwrap();
    assert_eq!(support::be32(&stsz, 8), 40);
    // コーデック設定は参照ファイルのものを引き継いでいる。
    assert!(support::find(&stbl, b"avcC").is_some());
    // キーフレームは 10 フレームごと。
    let stss = support::mp4_path(&stbl, &[b"stss"]).expect("stss が無い");
    assert_eq!(support::be32(&stss, 4), 4);
}

#[test]
fn mp4_missing_moov_without_a_reference_falls_back_to_annex_b() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::mp4(40);
    let input = place(&dir, "nomoov.mp4", &support::remove_box(&healthy, b"moov"));
    let output = dir.path().join("fixed.mp4");

    let report = Repairer::new(&input, &output).run().unwrap();

    assert_eq!(report.status, RepairStatus::Partial, "{report:?}");
    let out = report.output.as_ref().expect("出力が無い");
    assert_eq!(out.extension().unwrap(), "h264", "拡張子を付け替えている");
    assert!(out.exists());
    // 開始コードから始まる Annex-B になっている。
    assert_eq!(&read(out)[..4], &[0, 0, 0, 1]);
    assert!(
        report.issues.iter().any(|i| i.contains("参照")),
        "{:?}",
        report.issues
    );
}

#[test]
fn mp4_truncated_file_drops_samples_outside_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::mp4(40);
    let input = place(&dir, "cut.mp4", &support::truncate(&healthy, 0.6));
    let output = dir.path().join("fixed.mp4");

    let report = Repairer::new(&input, &output).run().unwrap();
    assert_eq!(report.status, RepairStatus::Partial, "{report:?}");
    assert_eq!(report.verification, Verification::Container);
    assert!(
        report.fixes.iter().any(|f| f.contains("索引から外した")),
        "{:?}",
        report.fixes
    );

    let fixed = read(&output);
    let stsz = support::mp4_path(
        &fixed,
        &[b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsz"],
    )
    .unwrap();
    let count = support::be32(&stsz, 8);
    assert!((1..40).contains(&count), "{count}");
}

#[test]
fn mp4_intact_file_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = support::mp4(10);
    let input = place(&dir, "ok.mp4", &healthy);
    let output = dir.path().join("copy.mp4");

    let report = Repairer::new(&input, &output).run().unwrap();
    assert_eq!(report.status, RepairStatus::Intact, "{report:?}");
    assert_eq!(read(&output), healthy);
}

// ---------------------------------------------------------------- 共通

#[test]
fn format_is_detected_even_without_a_usable_header() {
    let dir = tempfile::tempdir().unwrap();
    // 拡張子も中身の先頭も当てにならない状態から、本体側のタグで見分ける。
    let png = support::destroy_head(&support::png(40, 30), 8);
    let input = place(&dir, "mystery.bin", &png);
    let report = Repairer::new(&input, dir.path().join("out.bin"))
        .run()
        .unwrap();
    assert_eq!(report.format, ofr_repair::RepairFormat::Png);
    assert!(report.status.produced_output(), "{report:?}");
}

#[test]
fn the_original_file_is_never_touched() {
    let dir = tempfile::tempdir().unwrap();
    let broken = support::truncate(&support::png(40, 30), 0.5);
    let input = place(&dir, "broken.png", &broken);

    let _ = Repairer::new(&input, dir.path().join("fixed.png"))
        .run()
        .unwrap();
    assert_eq!(read(&input), broken, "修復元が書き換えられている");
}

/// 破損サンプル集を `testdata/out/repair/` に書き出す(PLAN.md 9章)。
///
/// 手元のプレイヤーや画像ビューアで実際に開いて確かめるためのもの。
/// 自動テストではないので `--ignored` を付けたときだけ走る。
///
/// ```text
/// cargo test -p ofr-repair --test repair -- --ignored write_samples --nocapture
/// ```
#[test]
#[ignore = "サンプルを書き出すだけ。手動確認用"]
fn write_samples() {
    // テストの作業ディレクトリはクレートの中なので、リポジトリのルートから辿る。
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/out/repair");
    std::fs::create_dir_all(&dir).unwrap();

    let jpeg = support::jpeg(640, 480);
    let sos = support::find(&jpeg, &[0xFF, 0xDA]).unwrap();
    let png = support::png(640, 480);
    let avi = support::avi(120);
    let mp4 = support::mp4(120);

    let samples: Vec<(&str, Vec<u8>)> = vec![
        ("healthy.jpg", jpeg.clone()),
        ("head-damage.jpg", support::destroy_head(&jpeg, sos)),
        ("truncated.jpg", support::truncate(&jpeg, 0.6)),
        ("trailing-junk.jpg", support::append_junk(&jpeg, 4096)),
        ("healthy.png", png.clone()),
        ("bad-crc.png", support::break_png_crc(&png, b"IDAT")),
        ("truncated.png", support::truncate(&png, 0.6)),
        ("no-ihdr.png", support::rename_tag(&png, b"IHDR", b"junk")),
        ("healthy.avi", avi.clone()),
        ("no-idx1.avi", support::remove_riff_chunk(&avi, b"idx1")),
        ("truncated.avi", support::truncate(&avi, 0.6)),
        ("healthy.mp4", mp4.clone()),
        ("no-moov.mp4", support::remove_box(&mp4, b"moov")),
        ("truncated.mp4", support::truncate(&mp4, 0.6)),
    ];

    for (name, data) in &samples {
        let path = dir.join(name);
        std::fs::write(&path, data).unwrap();
        println!("{} ({} バイト)", path.display(), data.len());
    }
}

#[test]
fn skip_intact_writes_nothing_for_healthy_files() {
    let dir = tempfile::tempdir().unwrap();
    let options = RepairOptions {
        write_intact: false,
        ..RepairOptions::default()
    };

    for (name, data) in [
        ("ok.png", support::png(32, 32)),
        ("ok.avi", support::avi(6)),
        ("ok.mp4", support::mp4(6)),
    ] {
        let input = place(&dir, name, &data);
        let output = dir.path().join(format!("{name}.out"));
        let report = Repairer::new(&input, &output)
            .with_options(options.clone())
            .run()
            .unwrap();

        assert_eq!(report.status, RepairStatus::Intact, "{name}: {report:?}");
        assert!(report.output.is_none(), "{name}");
        assert!(!output.exists(), "{name}: 書く必要が無いのに書いている");
    }
}
