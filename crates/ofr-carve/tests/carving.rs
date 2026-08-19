//! カービングの統合テスト。
//!
//! Phase 3 の完了条件「テストイメージに埋めた既知ファイル群の 90% 以上を
//! 正しい境界で切り出せる」をここで測る(PLAN.md 8章)。テストイメージは
//! `support` が機械生成するので、実機のメディアなしで CI から検証できる。

mod support;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ofr_carve::{CarveOptions, CarveReport, Carver, Confidence, FileFormat};
use ofr_device::MockDevice;

/// テストイメージのクラスタサイズ。ファイルはこの境界に置く。
const CLUSTER: usize = 4096;

/// テストイメージに埋めた 1 ファイル。
struct Placed {
    name: &'static str,
    offset: u64,
    data: Vec<u8>,
}

/// ファイル群をクラスタ境界に並べ、隙間を雑音で埋めたイメージを作る。
fn build_image(files: Vec<(&'static str, Vec<u8>)>) -> (Vec<u8>, Vec<Placed>) {
    let mut rng = support::Rng::new(0xC0FFEE);
    let mut image = rng.bytes(CLUSTER * 2); // 先頭にはFS跡地に見立てた雑音を置く
    let mut placed = Vec::new();

    for (name, data) in files {
        pad_to_cluster(&mut image, &mut rng);
        placed.push(Placed {
            name,
            offset: image.len() as u64,
            data: data.clone(),
        });
        image.extend_from_slice(&data);
        // ファイル間には未使用領域(雑音)を挟む。
        image.extend_from_slice(&rng.bytes(CLUSTER / 2 + 137));
    }

    pad_to_cluster(&mut image, &mut rng);
    image.extend_from_slice(&rng.bytes(CLUSTER));
    (image, placed)
}

fn pad_to_cluster(image: &mut Vec<u8>, rng: &mut support::Rng) {
    let rem = image.len() % CLUSTER;
    if rem != 0 {
        image.extend_from_slice(&rng.bytes(CLUSTER - rem));
    }
}

/// 全形式を 1 つずつ含むテストイメージ。
fn sample_set() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("jpeg-exif", support::jpeg(1920, 1080, true)),
        ("jpeg-plain", support::jpeg(640, 480, false)),
        ("png", support::png(800, 600)),
        ("gif", support::gif(320, 240)),
        ("mp4", support::mp4(4096)),
        ("mov", support::mov(2048)),
        ("heic", support::heic(3000)),
        ("avi", support::avi(1280, 720, 8)),
        ("wav", support::wav(2000)),
        ("mp3", support::mp3(12)),
        ("docx", support::docx()),
        (
            "zip",
            support::zip(&[("readme.txt", b"hello ofr".to_vec())]),
        ),
        ("pdf", support::pdf()),
    ]
}

fn carve(image: &[u8], dest: Option<&Path>, options: CarveOptions) -> CarveReport {
    let device = MockDevice::builder(image.len() as u64)
        .data(image.to_vec())
        .build();
    Carver::new(&device)
        .with_options(options)
        .run(dest)
        .expect("カービングが失敗した")
}

fn options() -> CarveOptions {
    CarveOptions {
        align: CLUSTER as u64,
        ..CarveOptions::default()
    }
}

#[test]
fn carves_every_format_at_the_right_boundaries() {
    let (image, placed) = build_image(sample_set());
    let dest = tempfile::tempdir().unwrap();
    let report = carve(&image, Some(dest.path()), options());

    let mut failures: Vec<String> = Vec::new();
    let mut correct = 0usize;

    for want in &placed {
        let Some(got) = report.files.iter().find(|f| f.offset == want.offset) else {
            failures.push(format!("{}: 見つからなかった", want.name));
            continue;
        };
        if got.size != want.data.len() as u64 {
            failures.push(format!(
                "{}: 境界がずれた (期待 {} バイト, 実際 {} バイト)",
                want.name,
                want.data.len(),
                got.size
            ));
            continue;
        }
        // 書き出したファイルの中身が元と 1 バイトも違わないこと。
        let path = dest.path().join(got.extension).join(&got.file_name);
        let written = std::fs::read(&path).expect("切り出したファイルが読めない");
        if written != want.data {
            failures.push(format!("{}: 書き出した中身が一致しない", want.name));
            continue;
        }
        assert_eq!(got.confidence, Confidence::Exact, "{}", want.name);
        assert!(got.is_intact(), "{}", want.name);
        correct += 1;
    }

    let ratio = correct as f64 / placed.len() as f64;
    assert!(
        ratio >= 0.9,
        "正しい境界で切り出せたのは {correct}/{} ({:.0}%)。Phase 3 の完了条件は 90% 以上。\n{}",
        placed.len(),
        ratio * 100.0,
        failures.join("\n")
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));

    // 埋めたファイル以外を大量に拾っていないこと(雑音からの誤検出)。
    assert_eq!(
        report.files.len(),
        placed.len(),
        "余計なファイルを拾っている: {:?}",
        report
            .files
            .iter()
            .map(|f| (f.offset, f.format, f.size))
            .collect::<Vec<_>>()
    );
    assert_eq!(report.summary.found, placed.len() as u64);
    assert_eq!(report.summary.exact, placed.len() as u64);
    assert_eq!(report.summary.read_errors, 0);
}

#[test]
fn identifies_formats_and_extensions() {
    let (image, placed) = build_image(sample_set());
    let report = carve(&image, None, options());

    let by_name = |name: &str| -> &ofr_carve::CarvedFile {
        let want = placed.iter().find(|p| p.name == name).unwrap();
        report
            .files
            .iter()
            .find(|f| f.offset == want.offset)
            .unwrap_or_else(|| panic!("{name} が見つからない"))
    };

    assert_eq!(by_name("jpeg-exif").format, FileFormat::Jpeg);
    assert_eq!(by_name("png").format, FileFormat::Png);
    assert_eq!(by_name("gif").format, FileFormat::Gif);
    assert_eq!(by_name("mp4").format, FileFormat::Mp4);
    assert_eq!(by_name("mov").format, FileFormat::Mov);
    assert_eq!(by_name("heic").format, FileFormat::Heic);
    assert_eq!(by_name("avi").format, FileFormat::Avi);
    assert_eq!(by_name("wav").format, FileFormat::Wav);
    assert_eq!(by_name("mp3").format, FileFormat::Mp3);
    assert_eq!(by_name("pdf").format, FileFormat::Pdf);

    // ZIP は中身で拡張子が変わる。
    assert_eq!(by_name("docx").format, FileFormat::Zip);
    assert_eq!(by_name("docx").extension, "docx");
    assert_eq!(by_name("zip").extension, "zip");
}

#[test]
fn extracts_metadata_and_names_files_by_capture_time() {
    let (image, placed) = build_image(sample_set());
    let report = carve(&image, None, options());
    let by_name = |name: &str| -> &ofr_carve::CarvedFile {
        let want = placed.iter().find(|p| p.name == name).unwrap();
        report
            .files
            .iter()
            .find(|f| f.offset == want.offset)
            .unwrap()
    };

    // JPEG の Exif。撮影日時はファイル名にも反映される。
    let jpeg = by_name("jpeg-exif");
    let meta = &jpeg.metadata;
    assert_eq!(meta.camera_make.as_deref(), Some(support::EXIF_MAKE));
    assert_eq!(meta.camera_model.as_deref(), Some(support::EXIF_MODEL));
    assert_eq!(meta.orientation, Some(1));
    assert_eq!(meta.width, Some(1920));
    assert_eq!(meta.height, Some(1080));
    assert_eq!(
        meta.timestamp.map(|t| t.to_string()).as_deref(),
        Some("2023-04-15 14:25:30")
    );
    assert!(
        jpeg.file_name.starts_with("20230415-142530_"),
        "{}",
        jpeg.file_name
    );
    assert!(jpeg.file_name.ends_with(".jpg"));

    // Exif のない JPEG は連番だけの名前になる。
    assert!(by_name("jpeg-plain").file_name.starts_with("carved_"));
    assert_eq!(by_name("jpeg-plain").metadata.width, Some(640));

    // MP4 は mvhd から作成日時と長さを拾う。
    let mp4 = by_name("mp4");
    assert_eq!(
        mp4.metadata.timestamp.map(|t| t.to_string()).as_deref(),
        Some("2023-04-15 14:25:30")
    );
    assert_eq!(mp4.metadata.duration_ms, Some(5000));

    // 画像は寸法、音声・動画は長さ。
    assert_eq!(by_name("png").metadata.width, Some(800));
    assert_eq!(by_name("gif").metadata.height, Some(240));
    assert_eq!(by_name("avi").metadata.width, Some(1280));
    assert_eq!(by_name("wav").metadata.duration_ms, Some(45)); // 8000 バイト / 176400
    assert_eq!(by_name("mp3").metadata.duration_ms, Some(313)); // 12 * 1152 / 44100
    assert_eq!(
        by_name("pdf")
            .metadata
            .timestamp
            .map(|t| t.to_string())
            .as_deref(),
        Some("2023-04-15 14:25:30")
    );
}

#[test]
fn does_not_carve_files_nested_inside_other_files() {
    // ZIP の中に JPEG と PNG を無圧縮で入れる。中身を別ファイルとして
    // 二重に拾ってはいけない。
    let archive = support::zip(&[
        ("photo.jpg", support::jpeg(100, 100, true)),
        ("shot.png", support::png(64, 64)),
    ]);
    let (image, placed) = build_image(vec![("zip-with-images", archive)]);
    let report = carve(&image, None, options());

    assert_eq!(report.files.len(), 1, "入れ子のファイルまで拾っている");
    assert_eq!(report.files[0].offset, placed[0].offset);
    assert_eq!(report.files[0].size, placed[0].data.len() as u64);
    assert_eq!(report.files[0].format, FileFormat::Zip);
}

#[test]
fn clamps_a_file_without_an_end_marker_to_the_next_signature() {
    // EOI を削った JPEG。終端が求まらないので、次のシグネチャの手前で切る。
    let mut broken = support::jpeg(320, 240, false);
    broken.truncate(broken.len() - 2);
    let (image, placed) = build_image(vec![("jpeg-no-eoi", broken), ("png", support::png(32, 32))]);
    let report = carve(&image, None, options());

    let jpeg = &report.files[0];
    assert_eq!(jpeg.offset, placed[0].offset);
    assert_eq!(jpeg.confidence, Confidence::Truncated);
    assert_eq!(
        jpeg.end(),
        placed[1].offset,
        "次のシグネチャの手前まで切り出すはず"
    );

    // 後続の PNG はきちんと境界が出る。
    assert_eq!(report.files[1].offset, placed[1].offset);
    assert_eq!(report.files[1].confidence, Confidence::Exact);
}

#[test]
fn recovers_what_it_can_from_a_device_with_bad_sectors() {
    let (image, placed) = build_image(sample_set());
    let wav = placed.iter().find(|p| p.name == "wav").unwrap();
    // WAV の途中にセクタ境界の不良域を作る。
    let bad_at = (wav.offset + 2048) / 512 * 512;

    let device = MockDevice::builder(image.len() as u64)
        .data(image.clone())
        .bad_range(bad_at, 1024)
        .build();
    let dest = tempfile::tempdir().unwrap();
    let report = Carver::new(&device)
        .with_options(options())
        .run(Some(dest.path()))
        .unwrap();

    // 不良域があっても走査は止まらない。他のファイルは無傷で揃う。
    assert_eq!(report.files.len(), placed.len());
    assert!(report.summary.read_errors > 0);

    let carved_wav = report
        .files
        .iter()
        .find(|f| f.offset == wav.offset)
        .unwrap();
    assert_eq!(carved_wav.size, wav.data.len() as u64);
    assert!(carved_wav.bad_bytes > 0, "不良バイトが記録されていない");
    assert!(!carved_wav.is_intact());

    // 読めた部分はそのまま、読めなかった部分はゼロで埋まっている。
    let path = dest
        .path()
        .join(carved_wav.extension)
        .join(&carved_wav.file_name);
    let written = std::fs::read(&path).unwrap();
    assert_eq!(written.len(), wav.data.len());
    let head = (bad_at - wav.offset) as usize;
    assert_eq!(&written[..head], &wav.data[..head]);
    assert!(written[head..head + 512].iter().all(|b| *b == 0));

    // 不良域以外のファイルは 1 バイトも違わない。
    for p in placed.iter().filter(|p| p.name != "wav") {
        let f = report.files.iter().find(|f| f.offset == p.offset).unwrap();
        let path = dest.path().join(f.extension).join(&f.file_name);
        assert_eq!(std::fs::read(&path).unwrap(), p.data, "{}", p.name);
    }
}

#[test]
fn only_carves_the_requested_formats() {
    let (image, placed) = build_image(sample_set());
    let report = carve(
        &image,
        None,
        CarveOptions {
            formats: Some(vec![FileFormat::Jpeg, FileFormat::Pdf]),
            ..options()
        },
    );

    assert!(
        report
            .files
            .iter()
            .all(|f| matches!(f.format, FileFormat::Jpeg | FileFormat::Pdf))
    );
    // JPEG 2 つと PDF 1 つ。
    assert_eq!(report.files.len(), 3);
    let pdf = placed.iter().find(|p| p.name == "pdf").unwrap();
    assert!(report.files.iter().any(|f| f.offset == pdf.offset));
}

#[test]
fn alignment_controls_which_offsets_are_considered() {
    // クラスタ境界から 512 バイトずらした位置に JPEG を置く。
    let mut rng = support::Rng::new(7);
    let mut image = rng.bytes(CLUSTER + 512);
    let offset = image.len() as u64;
    let jpeg = support::jpeg(64, 64, false);
    image.extend_from_slice(&jpeg);
    image.extend_from_slice(&rng.bytes(CLUSTER));

    // クラスタ境界だけを見る設定では拾えない。
    let report = carve(
        &image,
        None,
        CarveOptions {
            align: 4096,
            ..CarveOptions::default()
        },
    );
    assert!(report.files.is_empty());

    // セクタ境界まで見る既定の設定なら拾える。
    let report = carve(&image, None, CarveOptions::default());
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].offset, offset);
    assert_eq!(report.files[0].size, jpeg.len() as u64);
}

#[test]
fn carves_from_an_image_file_through_file_device() {
    let (image, placed) = build_image(sample_set());
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("usb.img");
    std::fs::write(&img_path, &image).unwrap();

    let device = ofr_device::FileDevice::open(&img_path).unwrap();
    let dest = dir.path().join("recovered");
    let report = Carver::new(&device)
        .with_options(options())
        .run(Some(&dest))
        .unwrap();

    assert_eq!(report.files.len(), placed.len());
    for p in &placed {
        let f = report.files.iter().find(|f| f.offset == p.offset).unwrap();
        assert_eq!(
            std::fs::read(dest.join(f.extension).join(&f.file_name)).unwrap(),
            p.data
        );
    }
}

#[test]
fn reports_progress_and_found_files() {
    let (image, placed) = build_image(sample_set());
    let device = MockDevice::builder(image.len() as u64).data(image).build();

    let found = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&found);
    let progress = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counter = Arc::clone(&progress);

    let report = Carver::new(&device)
        .with_options(CarveOptions {
            // 進捗の間引きを外して必ず届くようにする。
            progress_interval: std::time::Duration::ZERO,
            ..options()
        })
        .with_found(move |f| sink.lock().unwrap().push(f.offset))
        .with_progress(move |_| {
            counter.fetch_add(1, Ordering::Relaxed);
        })
        .run(None)
        .unwrap();

    assert_eq!(found.lock().unwrap().len(), placed.len());
    assert_eq!(
        *found.lock().unwrap(),
        report.files.iter().map(|f| f.offset).collect::<Vec<_>>()
    );
    assert!(progress.load(Ordering::Relaxed) > 0);
}

#[test]
fn cancelling_stops_the_scan() {
    let (image, _) = build_image(sample_set());
    let device = MockDevice::builder(image.len() as u64).data(image).build();

    let cancel = Arc::new(AtomicBool::new(true));
    let report = Carver::new(&device)
        .with_options(options())
        .with_cancel(cancel)
        .run(None)
        .unwrap();

    assert!(report.summary.cancelled);
    assert_eq!(report.summary.found, 0);
}

#[test]
fn scanning_random_data_never_panics() {
    // PLAN.md 6章 5項: 不正なバイト列で panic しないこと。
    // 乱数の中にはシグネチャに似たバイト列が必ず現れるので、
    // 全バリデータが最も雑な入力に晒される。
    for seed in [1u64, 2, 3, 4, 5] {
        let mut rng = support::Rng::new(seed);
        let image = rng.bytes(2 << 20);
        let report = carve(
            &image,
            None,
            CarveOptions {
                // 全バイトを候補にして、バリデータを最大限叩く。
                align: 1,
                ..CarveOptions::default()
            },
        );
        for f in &report.files {
            assert!(f.size >= 64, "最小サイズを下回る切り出し");
            assert!(f.end() <= image.len() as u64, "デバイス末尾を越えた");
        }
    }
}

#[test]
fn scanning_truncated_and_zeroed_data_never_panics() {
    // 各形式のヘッダだけを残して途中で切ったもの、ゼロで埋めたものを並べる。
    let mut image: Vec<u8> = Vec::new();
    for (_, data) in sample_set() {
        for cut in [4usize, 16, 64, 200] {
            let mut head = data.clone();
            head.truncate(cut.min(head.len()));
            image.extend_from_slice(&head);
            image.resize(image.len().next_multiple_of(512), 0);
        }
        // 中身をゼロで潰したもの(ヘッダだけ本物)。
        let mut zeroed = data.clone();
        let keep = 32.min(zeroed.len());
        zeroed[keep..].fill(0);
        image.extend_from_slice(&zeroed);
        image.resize(image.len().next_multiple_of(512), 0);
    }

    let report = carve(
        &image,
        None,
        CarveOptions {
            align: 1,
            ..CarveOptions::default()
        },
    );
    for f in &report.files {
        assert!(f.end() <= image.len() as u64);
    }
}

/// テストイメージを `testdata/out/` に書き出す(手動実行用)。
///
/// CLI を実機なしで試すためのもの。CI では走らせない。
///
/// ```text
/// cargo test -p ofr-carve --test carving -- --ignored write_test_image --nocapture
/// ofr carve testdata/out/carve-test.img /tmp/recovered
/// ```
#[test]
#[ignore = "手動実行用。testdata/out/ にテストイメージを書き出す"]
fn write_test_image() {
    let (image, placed) = build_image(sample_set());
    let out = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/out")
        .canonicalize()
        .unwrap_or_else(|_| {
            let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/out");
            std::fs::create_dir_all(&p).unwrap();
            p
        });
    std::fs::create_dir_all(&out).unwrap();

    let img = out.join("carve-test.img");
    std::fs::write(&img, &image).unwrap();

    // 何をどこに埋めたかの一覧。切り出し結果の照合に使う。
    let mut manifest = String::from("# carve-test.img に埋めたファイル\n\noffset\tsize\tname\n");
    for p in &placed {
        manifest.push_str(&format!("{}\t{}\t{}\n", p.offset, p.data.len(), p.name));
        std::fs::write(out.join(format!("{}.bin", p.name)), &p.data).unwrap();
    }
    std::fs::write(out.join("carve-test.manifest.tsv"), manifest).unwrap();

    println!(
        "{} ({} バイト, {} ファイル) を書き出した",
        img.display(),
        image.len(),
        placed.len()
    );
}
