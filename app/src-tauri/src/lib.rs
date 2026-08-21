//! Tauri コマンド層。
//!
//! GUI とコアの間は [`ofr_core`] のジョブ API だけでやり取りする
//! (PLAN.md 4章)。ここは薄い橋渡しに徹していて、復旧の判断は何も持たない。
//! 進捗・発見・完了は [`JOB_EVENT`] という 1 本のイベントに流し、
//! フロント側で job ID ごとに振り分ける。

#![deny(unsafe_code)]

use std::sync::Arc;

use ofr_core::{
    CarvedFileDto, Core, DeviceDto, EntryPage, EntryQuery, JobEvent, JobId, JobRequest, PreviewDto,
    PrivilegeDto,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

/// ジョブのイベントを流すチャンネル名。
pub const JOB_EVENT: &str = "ofr://job";

/// フロントに返すエラー。
///
/// `code` は機械可読な種別で、これを見て「管理者で実行し直す」などの
/// 案内を出し分ける。`message` は原因まで辿った日本語の説明。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    /// 種別。
    pub code: ofr_core::ErrorCode,
    /// 説明。
    pub message: String,
}

impl From<ofr_core::CoreError> for ApiError {
    fn from(e: ofr_core::CoreError) -> Self {
        Self {
            code: e.code(),
            message: e.full_message(),
        }
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

/// 接続されているデバイスの一覧。起動ディスクは `selectable: false` で返る。
#[tauri::command]
fn list_devices(core: State<'_, Arc<Core>>) -> ApiResult<Vec<DeviceDto>> {
    Ok(core.devices()?)
}

/// いまの権限(管理者 / root で動いているか)。
#[tauri::command]
fn privileges(core: State<'_, Arc<Core>>) -> PrivilegeDto {
    core.privileges()
}

/// フルディスクアクセスの設定画面を開く(macOS)。
///
/// root でも TCC に止められている場合、権限を上げ直しても直らない。
/// 利用者がその場で許可できるように、設定画面まで連れて行く。
#[tauri::command]
fn open_privacy_settings(app: AppHandle) -> ApiResult<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(ofr_core::FULL_DISK_ACCESS_SETTINGS, None::<String>)
        .map_err(|e| ApiError {
            code: ofr_core::ErrorCode::Other,
            message: format!("設定画面を開けなかった: {e}"),
        })
}

/// 管理者権限で起動し直す(macOS)。成功したらこのプロセスは終了する。
#[tauri::command]
fn relaunch_elevated(app: AppHandle) -> ApiResult<()> {
    ofr_core::elevate::relaunch_elevated()?;
    app.exit(0);
    Ok(())
}

/// 吸い出しの出力先を調べる (既存か、続きから進めるか)。
///
/// 中断した吸い出しを「最初からやり直す」と誤解させないため、開始前に伝える。
#[tauri::command]
fn output_state(output: String) -> ofr_core::OutputState {
    ofr_core::output_state(std::path::Path::new(&output))
}

/// ジョブを始める。戻り値はジョブ ID。
#[tauri::command]
fn start_job(app: AppHandle, core: State<'_, Arc<Core>>, request: JobRequest) -> ApiResult<JobId> {
    let sink: ofr_core::EventSink = Arc::new(move |event: JobEvent| {
        if let Err(e) = app.emit(JOB_EVENT, &event) {
            tracing::warn!(error = %e, "イベントを送れなかった");
        }
    });
    Ok(core.inner().start(request, sink)?)
}

/// ジョブを中断する。書き出し済みのものは残る。
#[tauri::command]
fn cancel_job(core: State<'_, Arc<Core>>, job: JobId) -> bool {
    core.cancel(job)
}

/// 解析結果を 1 ページ取り出す。
#[tauri::command]
fn entries(core: State<'_, Arc<Core>>, session: JobId, query: EntryQuery) -> ApiResult<EntryPage> {
    Ok(core.entries(session, &query)?)
}

/// カービング結果の一覧。
#[tauri::command]
fn carved(core: State<'_, Arc<Core>>, session: JobId) -> ApiResult<Vec<CarvedFileDto>> {
    Ok(core.carved(session)?)
}

/// 中身をプレビュー用に読み出す(サムネイル表示)。
#[tauri::command]
fn preview(
    core: State<'_, Arc<Core>>,
    session: JobId,
    index: usize,
    limit: u64,
) -> ApiResult<PreviewDto> {
    Ok(core.preview(session, index, limit)?)
}

/// 解析結果を捨てる。開いていたデバイスもここで閉じる。
#[tauri::command]
fn close_session(core: State<'_, Arc<Core>>, session: JobId) {
    core.close_session(session);
}

/// アプリを組み立てて実行する。
///
/// # Panics
///
/// Tauri の起動に失敗したときに panic する(ウィンドウを出せない状態では
/// 続行しても何もできない)。
pub fn run() {
    init_tracing();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            app.manage(Core::new());
            Ok(())
        })
        .on_window_event(|window, event| {
            // 閉じるときに走っているジョブを止める。壊れかけメディアへの
            // アクセスを、ウィンドウが無くなった後まで続けない。
            if let tauri::WindowEvent::Destroyed = event
                && let Some(core) = window.app_handle().try_state::<Arc<Core>>()
            {
                core.cancel_all();
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_devices,
            privileges,
            relaunch_elevated,
            open_privacy_settings,
            output_state,
            start_job,
            cancel_job,
            entries,
            carved,
            preview,
            close_session,
        ])
        .run(tauri::generate_context!())
        .expect("Tauri アプリを起動できなかった");
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("OFR_LOG").unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "warn,ofr_gui_lib=info,ofr_core=info,ofr_device=info,ofr_image=info,\
             ofr_fs=info,ofr_fat=info,ofr_exfat=info,ofr_carve=info,ofr_copy=info,ofr_repair=info",
        )
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}
