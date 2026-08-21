//! ジョブ API の通し確認。
//!
//! GUI がやることを、画面なしでそのままなぞる:
//! 「イメージを解析 → 結果を引く → 中身をプレビュー → 選んで復元」。
//! 画面が無いだけで通る道は同じなので、GUI 側の不具合をここで先に潰せる。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ofr_core::{Core, EntryQuery, JobEvent, JobId, JobRequest, JobResult, Outcome, ScanRequest};

/// 流れてきたイベントを溜める受け口。
#[derive(Default)]
struct Events {
    all: Mutex<Vec<JobEvent>>,
    done: AtomicBool,
}

impl Events {
    fn sink(self: &Arc<Self>) -> ofr_core::EventSink {
        let me = Arc::clone(self);
        Arc::new(move |event: JobEvent| {
            if event.is_terminal() {
                me.done.store(true, Ordering::SeqCst);
            }
            me.all.lock().unwrap().push(event);
        })
    }

    /// 終わるまで待って、最後のイベントを返す。
    fn wait(&self) -> JobEvent {
        let deadline = Instant::now() + Duration::from_secs(120);
        while !self.done.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "ジョブが終わらない");
            std::thread::sleep(Duration::from_millis(5));
        }
        self.all.lock().unwrap().last().cloned().unwrap()
    }

    fn progress_count(&self) -> usize {
        self.all
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, JobEvent::Progress { .. }))
            .count()
    }
}

/// シナリオのイメージをファイルに書き出す。
fn write_image(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn run(core: &Arc<Core>, request: JobRequest) -> (JobId, Arc<Events>, JobEvent) {
    let events = Arc::new(Events::default());
    let job = core.start(request, events.sink()).unwrap();
    let last = events.wait();
    (job, events, last)
}

fn scan(core: &Arc<Core>, source: &std::path::Path) -> (JobId, JobEvent) {
    let (job, _, last) = run(
        core,
        JobRequest::Scan(ScanRequest {
            source: source.display().to_string(),
            fs: ofr_core::FsChoice::Auto,
            offset: None,
            deleted: true,
            orphans: true,
        }),
    );
    (job, last)
}

/// 削除したファイルを GUI と同じ手順で復元し、中身が元と一致すること。
#[test]
fn scans_previews_and_restores_deleted_files() {
    let dir = tempfile::tempdir().unwrap();
    let scenario = ofr_testfs::scenarios::fat32_deleted();
    let image = write_image(dir.path(), "usb.img", &scenario.image);

    let core = Core::new();
    let (session, last) = scan(&core, &image);

    let JobEvent::Finished {
        outcome, result, ..
    } = last
    else {
        panic!("解析が失敗した: {last:?}");
    };
    assert_eq!(outcome, Outcome::Complete);
    let JobResult::Scan(scan) = *result else {
        panic!("結果の型が違う");
    };
    assert_eq!(scan.session, session);
    assert_eq!(scan.volume.fs, "FAT32");
    assert!(scan.stats.deleted > 0, "削除済みが見つかっていない");

    // 結果ツリー画面が最初に引くもの: 削除済みのファイルだけ。
    let page = core
        .entries(
            session,
            &EntryQuery {
                statuses: vec!["deleted".to_string()],
                files_only: true,
                ..EntryQuery::default()
            },
        )
        .unwrap();
    assert_eq!(page.total, page.entries.len());
    assert!(page.files > 0);
    assert!(page.bytes > 0);

    // 削除されたはずのファイルが出ていること。
    //
    // FAT32 の削除エントリは 8.3 名の先頭 1 文字が失われる (仕様上どうやっても
    // 戻らない) ので、名前ではなくサイズで突き合わせる。名前が不完全なものには
    // その旨の注記が付いていること。
    let expected: Vec<&ofr_testfs::ExpectedFile> =
        scenario.files.iter().filter(|f| f.deleted).collect();
    assert!(!expected.is_empty());
    for file in &expected {
        let hit = page
            .entries
            .iter()
            .find(|e| e.size == file.data.len() as u64)
            .unwrap_or_else(|| {
                panic!(
                    "{} (サイズ {}) が結果に出ていない",
                    file.path,
                    file.data.len()
                )
            });
        assert_eq!(hit.status, "deleted");
        // FAT チェーンは削除時に消えるので、連続配置を仮定して拾っている。
        assert!(hit.concerns.contiguous_assumed);
    }

    // サムネイル: 中身が本当に残っているかを目で確かめる経路。
    let target = page
        .entries
        .iter()
        .find(|e| e.size == expected[0].data.len() as u64)
        .unwrap();
    let preview = core.preview(session, target.id, 0).unwrap();
    assert!(preview.bytes > 0);
    assert!(!preview.data.is_empty());

    // 復元。GUI は選んだ項目の ID を送る。
    let dest = dir.path().join("recovered");
    let (_, _, last) = run(
        &core,
        JobRequest::Restore(
            serde_json::from_value(serde_json::json!({
                "kind": "restore",
                "session": session,
                "entries": page.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
                "dest": dest,
            }))
            .unwrap(),
        ),
    );
    let JobEvent::Finished { result, .. } = last else {
        panic!("復元が失敗した: {last:?}");
    };
    let JobResult::Restore(restore) = *result else {
        panic!("結果の型が違う");
    };
    assert_eq!(restore.summary.files as usize, page.entries.len());
    assert_eq!(restore.summary.failed, 0);

    // 中身が元と 1 バイトも違わないこと。
    let restored = read_all_files(&dest);
    for file in &expected {
        assert!(
            restored.contains(&file.data),
            "{} の中身が復元されていない",
            file.path
        );
    }

    // レポートが宛先に残ること。
    let report = dest.join(ofr_core::RESTORE_REPORT_NAME);
    assert!(report.is_file(), "レポートが無い");
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(json["summary"]["failed"], 0);
}

/// フォルダを選んだら中身も全部復元されること(GUI のチェックボックスの挙動)。
#[test]
fn restoring_a_folder_includes_everything_inside() {
    let dir = tempfile::tempdir().unwrap();
    let scenario = ofr_testfs::scenarios::exfat_deleted();
    let image = write_image(dir.path(), "sd.img", &scenario.image);

    let core = Core::new();
    let (session, last) = scan(&core, &image);
    assert!(matches!(last, JobEvent::Finished { .. }), "{last:?}");

    let all = core.entries(session, &EntryQuery::default()).unwrap();
    let folder = all
        .entries
        .iter()
        .find(|e| e.kind == "dir" && e.name.eq_ignore_ascii_case("DCIM"))
        .expect("DCIM が無い");

    let dest = dir.path().join("out");
    let (_, _, last) = run(
        &core,
        JobRequest::Restore(
            serde_json::from_value(serde_json::json!({
                "kind": "restore",
                "session": session,
                "entries": [folder.id],
                "dest": dest,
            }))
            .unwrap(),
        ),
    );
    let JobEvent::Finished { result, .. } = last else {
        panic!("復元が失敗した: {last:?}");
    };
    let JobResult::Restore(restore) = *result else {
        panic!("結果の型が違う");
    };
    // DCIM 以下のファイルが全部入っている。
    let inside = scenario
        .files
        .iter()
        .filter(|f| f.path.starts_with("/DCIM/"))
        .count();
    assert_eq!(restore.summary.files as usize, inside);
    assert_eq!(restore.summary.copied as usize, inside);
}

/// カービングは走査しながら 1 件ずつ流れ、結果からプレビューできること。
#[test]
fn carving_streams_found_files_and_previews_them() {
    let dir = tempfile::tempdir().unwrap();
    // 本物の PNG を 1 枚埋めたイメージを組み立てる。
    let png = make_png();
    let mut image = vec![0u8; 1 << 20];
    let at = 4096;
    image[at..at + png.len()].copy_from_slice(&png);
    let path = write_image(dir.path(), "carve.img", &image);

    let core = Core::new();
    let out = dir.path().join("carved");
    let (job, events, last) = run(
        &core,
        JobRequest::Carve(
            serde_json::from_value(serde_json::json!({
                "kind": "carve",
                "source": path,
                "output": out,
                "align": 4096,
            }))
            .unwrap(),
        ),
    );

    let JobEvent::Finished { result, .. } = last else {
        panic!("カービングが失敗した: {last:?}");
    };
    let JobResult::Carve(carve) = *result else {
        panic!("結果の型が違う");
    };
    assert_eq!(carve.summary.found, 1);
    assert_eq!(carve.session, job);

    // 見つけた瞬間に流れていること(GUI が走査中にツリーを育てるため)。
    let items = events
        .all
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, JobEvent::Item { .. }))
        .count();
    assert_eq!(items, 1);

    let files = core.carved(job).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].format, "png");
    assert!(std::path::Path::new(files[0].output.as_ref().unwrap()).is_file());

    let preview = core.preview(job, files[0].index as usize, 0).unwrap();
    assert_eq!(preview.mime, "image/png");
    assert!(preview.bytes > 0);
}

/// 中断すると、そこまでの結果を持って終わること。
#[test]
fn cancelling_stops_the_job() {
    let dir = tempfile::tempdir().unwrap();
    // 大きめのイメージを走査させて、始まった直後に止める。
    let path = write_image(dir.path(), "big.img", &vec![0u8; 64 << 20]);

    let core = Core::new();
    let events = Arc::new(Events::default());
    let job = core
        .start(
            JobRequest::Carve(
                serde_json::from_value(serde_json::json!({
                    "kind": "carve",
                    "source": path,
                    "output": dir.path().join("carved"),
                }))
                .unwrap(),
            ),
            events.sink(),
        )
        .unwrap();
    assert!(core.cancel(job));

    let last = events.wait();
    let JobEvent::Finished { outcome, .. } = last else {
        panic!("中断で失敗になった: {last:?}");
    };
    assert_eq!(outcome, Outcome::Cancelled);
    assert!(!core.is_running(job));
}

/// 進捗は流れるが、間引かれていること(PLAN.md 5.7)。
#[test]
fn progress_is_throttled() {
    let dir = tempfile::tempdir().unwrap();
    let scenario = ofr_testfs::scenarios::fat32_quick_format();
    let image = write_image(dir.path(), "usb.img", &scenario.image);

    let core = Core::new();
    let (_, events, last) = run(
        &core,
        JobRequest::Scan(ScanRequest {
            source: image.display().to_string(),
            fs: ofr_core::FsChoice::Auto,
            offset: None,
            deleted: true,
            orphans: true,
        }),
    );
    assert!(matches!(last, JobEvent::Finished { .. }), "{last:?}");
    // 48MiB の走査で数百回も飛んで来ないこと。
    assert!(events.progress_count() < 200, "進捗が多すぎる");
}

/// 起動ディスクは復旧元にできない(PLAN.md 6章 3項)。
#[test]
fn refuses_a_missing_source_with_a_code() {
    let core = Core::new();
    let (_, _, last) = run(
        &core,
        JobRequest::Scan(ScanRequest {
            source: "/dev/このデバイスは無い".to_string(),
            fs: ofr_core::FsChoice::Auto,
            offset: None,
            deleted: true,
            orphans: true,
        }),
    );
    let JobEvent::Failed { code, message, .. } = last else {
        panic!("失敗するはずが成功した");
    };
    assert!(!message.is_empty());
    // GUI が分岐に使えるコードが付いていること。
    assert!(matches!(
        code,
        ofr_core::ErrorCode::NotFound | ofr_core::ErrorCode::PermissionDenied
    ));
}

/// 宛先の全ファイルの中身。レポート以外を集める。
fn read_all_files(dir: &std::path::Path) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_none_or(|e| e != "json") {
                out.push(std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

/// 小さな本物の PNG(8x8 の灰色)。
fn make_png() -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let img = image::RgbImage::from_pixel(8, 8, image::Rgb([128, 128, 128]));
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut buf, image::ImageFormat::Png)
        .unwrap();
    buf.into_inner()
}

/// 吸い出しが最後まで行くと、完了イベントと結果が返ること。
///
/// GUI はこの完了イベントで「次に進む」ボタンを出すので、ここが出ないと
/// 画面が実行中のまま止まる。
#[test]
fn imaging_finishes_and_reports_the_result() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_image(dir.path(), "src.img", &vec![7u8; 3 << 20]);
    let output = dir.path().join("out.img");

    let core = Core::new();
    let (_, events, last) = run(
        &core,
        JobRequest::Image(
            serde_json::from_value(serde_json::json!({
                "kind": "image",
                "source": source,
                "output": output,
            }))
            .unwrap(),
        ),
    );

    let JobEvent::Finished {
        outcome, result, ..
    } = last
    else {
        panic!("完了イベントが来ない: {last:?}");
    };
    assert_eq!(outcome, Outcome::Complete);
    let JobResult::Image(summary) = *result else {
        panic!("結果の型が違う");
    };
    assert!(summary.complete);
    assert_eq!(summary.rescued, 3 << 20);
    assert_eq!(summary.remaining, 0);
    assert!(!summary.image_path.is_empty());
    assert_eq!(std::fs::metadata(&output).unwrap().len(), 3 << 20);

    // 進捗が 1 回も出ずに終わっていないこと (GUI の帯グラフが空になる)。
    assert!(events.progress_count() > 0);

    // JSON にしたときに GUI が読む形になっていること。
    let json = serde_json::to_value(
        events
            .all
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|e| e.is_terminal())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(json["event"], "finished");
    assert_eq!(json["result"]["kind"], "image");
    assert_eq!(json["result"]["complete"], true);
}

/// 出力先の状態を、始める前に正しく言えること。
///
/// 「上書きになるのか続きからになるのか」を取り違えると、中断した吸い出しを
/// 丸ごと読み直させてしまう。壊れかけメディアではそれ自体が損害になる。
#[test]
fn output_state_tells_resume_from_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("usb.img");

    // まだ何もない。
    let state = ofr_core::output_state(&output);
    assert!(!state.exists);
    assert!(!state.resumable);

    // 途中まで吸い出したデバイスを作る。後半は読めない。
    let device = ofr_device::MockDevice::builder(4 << 20)
        .pattern()
        .bad_range(2 << 20, 2 << 20)
        .build();
    let summary = ofr_image::Imager::new(&device)
        .run(&output, Some(&ofr_core::mapfile_path(&output)))
        .unwrap();
    assert!(!summary.is_complete(), "全部読めてしまっては試験にならない");

    // イメージも mapfile もあるので「続きから」。取得済みバイト数も言える。
    let state = ofr_core::output_state(&output);
    assert!(state.exists);
    assert!(state.resumable);
    assert_eq!(state.rescued, summary.rescued);
    assert_eq!(state.total, 4 << 20);

    // mapfile だけ消すと、続きからにはできない = 取り直しになる。
    std::fs::remove_file(ofr_core::mapfile_path(&output)).unwrap();
    let state = ofr_core::output_state(&output);
    assert!(state.exists);
    assert!(!state.resumable);
}
