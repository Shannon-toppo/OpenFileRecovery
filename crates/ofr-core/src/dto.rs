//! GUI に渡す値の形。
//!
//! # 表示文字列を持たせない
//!
//! GUI は日本語と英語の 2 言語に対応する(PLAN.md 7章)ので、ここから
//! 「削除済み」「コピー」のような**表示用の文字列を渡さない**。状態や段階は
//! `deleted` `copy` のような機械可読なコードで渡し、言語ごとの文言は
//! GUI 側のリソースファイルが持つ。
//!
//! 例外は下位クレートが組み立てる自由文(解析中の警告、OS のエラー文言)で、
//! これは日本語のまま `message` として流す。訳しようがないものを無理に
//! コード化するより、原文をそのまま見せるほうが復旧作業の役に立つ。

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

/// 接続されているデバイス 1 台。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDto {
    /// OS が使う識別子(`/dev/disk4`, `\\.\PhysicalDrive2`)。
    pub id: String,
    /// 画面表示用の名前。
    pub name: String,
    /// 種別コード(`physical-disk` / `volume` / `image-file` / `mock`)。
    pub kind: String,
    /// 容量(バイト)。不明なら 0。
    pub size_bytes: u64,
    /// 論理ブロックサイズ。
    pub block_size: u32,
    /// リムーバブルメディアか。
    pub removable: bool,
    /// OS 起動ディスクか。真なら GUI はグレーアウトする(PLAN.md 6章 3項)。
    pub is_system_disk: bool,
    /// 復旧元として選べるか。
    pub selectable: bool,
    /// シリアル番号。
    pub serial: Option<String>,
}

impl From<&ofr_device::DeviceInfo> for DeviceDto {
    fn from(d: &ofr_device::DeviceInfo) -> Self {
        Self {
            id: d.id.clone(),
            name: d.display_name.clone(),
            kind: d.kind.to_string(),
            size_bytes: d.size_bytes,
            block_size: d.block_size,
            removable: d.removable,
            is_system_disk: d.is_system_disk,
            selectable: d.is_selectable_as_source(),
            serial: d.serial.clone(),
        }
    }
}

/// 解析したボリュームの情報。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeDto {
    /// ファイルシステム名(`FAT32` / `exFAT`)。訳す必要のない固有名詞。
    pub fs: String,
    /// ボリュームラベル。
    pub label: Option<String>,
    /// クラスタサイズ。
    pub cluster_size: u32,
    /// ボリューム全体のバイト数。
    pub total_bytes: u64,
    /// デバイス先頭からの開始位置。
    pub offset: u64,
    /// パーティションの種別名。
    pub partition: String,
    /// ブートセクタの入手経路コード(`primary` / `backup` / `estimated`)。
    ///
    /// `estimated` なら解析結果の信頼度が落ちるので、GUI は注意を出す。
    pub boot_source: String,
    /// 解析時に気付いたこと(日本語の自由文)。
    pub notes: Vec<String>,
}

impl VolumeDto {
    /// 解析結果から組み立てる。
    pub fn new(volume: &ofr_fs::VolumeInfo, offset: u64, partition: &str) -> Self {
        Self {
            fs: volume.kind.label().to_string(),
            label: volume.label.clone(),
            cluster_size: volume.bytes_per_cluster,
            total_bytes: volume.total_bytes,
            offset,
            partition: partition.to_string(),
            boot_source: match volume.boot_source {
                ofr_fs::BootSource::Primary => "primary",
                ofr_fs::BootSource::Backup => "backup",
                ofr_fs::BootSource::Estimated => "estimated",
            }
            .to_string(),
            notes: volume.notes.clone(),
        }
    }
}

/// 復元候補につく懸念。GUI が注記として出す(PLAN.md 5.3)。
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConcernsDto {
    /// FAT チェーンが失われていたので連続配置と仮定して拾った。
    pub contiguous_assumed: bool,
    /// 名前を完全には復元できていない。
    pub name_partial: bool,
    /// 他のファイルに使われているクラスタの数(上書きの疑い)。
    pub conflicting_clusters: u32,
    /// 記録サイズ分の領域を集めきれなかった。
    pub truncated: bool,
}

impl From<&ofr_fs::EntryQuality> for ConcernsDto {
    fn from(q: &ofr_fs::EntryQuality) -> Self {
        Self {
            contiguous_assumed: q.contiguous_assumed,
            name_partial: q.name_partial,
            conflicting_clusters: q.conflicting_clusters,
            truncated: q.truncated,
        }
    }
}

/// 解析で見つかった 1 項目。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryDto {
    /// ツリー内の ID。復元するときはこれを送り返す。
    pub id: usize,
    /// 親の ID。
    pub parent: Option<usize>,
    /// 名前。
    pub name: String,
    /// ルートからのパス。
    pub path: String,
    /// `file` か `dir`。
    pub kind: &'static str,
    /// 記録されているサイズ。
    pub size: u64,
    /// 実際に書き出せるバイト数。
    pub recoverable: u64,
    /// 状態コード(`intact` / `deleted` / `orphaned` / `damaged`)。
    pub status: &'static str,
    /// 更新日時(`YYYY-MM-DD HH:MM:SS`)。
    pub modified: Option<String>,
    /// 小文字の拡張子。GUI がアイコンとプレビュー可否を決めるのに使う。
    pub ext: String,
    /// 懸念。
    pub concerns: ConcernsDto,
}

impl From<&ofr_fs::RecoveredEntry> for EntryDto {
    fn from(e: &ofr_fs::RecoveredEntry) -> Self {
        Self {
            id: e.id,
            parent: e.parent,
            name: e.name.clone(),
            path: e.path.clone(),
            kind: if e.is_dir() { "dir" } else { "file" },
            size: e.size,
            recoverable: e.recoverable_bytes(),
            status: e.status.as_str(),
            modified: e.times.best().map(|t| t.to_string()),
            ext: extension_of(&e.name),
            concerns: ConcernsDto::from(&e.quality),
        }
    }
}

/// 解析結果の統計。
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStatsDto {
    /// ディレクトリ数。
    pub dirs: usize,
    /// ファイル数。
    pub files: usize,
    /// 無傷の項目数。
    pub intact: usize,
    /// 削除済みの項目数。
    pub deleted: usize,
    /// 孤立していた項目数。
    pub orphaned: usize,
    /// 破損扱いの項目数。
    pub damaged: usize,
    /// 走査したクラスタ数。
    pub clusters_scanned: u64,
    /// 上限に達して打ち切ったか。
    pub truncated: bool,
    /// 中断されたか。
    pub cancelled: bool,
    /// 所要秒数。
    pub elapsed_secs: f64,
}

impl ScanStatsDto {
    /// ツリーから組み立てる。
    pub fn new(tree: &ofr_fs::FileTree) -> Self {
        let s = &tree.stats;
        Self {
            dirs: s.dirs,
            files: s.files,
            intact: tree
                .entries()
                .iter()
                .filter(|e| e.status == ofr_fs::EntryStatus::Intact)
                .count(),
            deleted: s.deleted,
            orphaned: s.orphaned,
            damaged: s.damaged,
            clusters_scanned: s.clusters_scanned,
            truncated: s.truncated,
            cancelled: s.cancelled,
            elapsed_secs: s.elapsed.as_secs_f64(),
        }
    }
}

/// 領域マップの 1 区間。イメージング画面の帯グラフになる(PLAN.md 5.2)。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MapSegmentDto {
    /// 開始オフセット。
    pub pos: u64,
    /// 長さ。
    pub len: u64,
    /// 状態コード(`rescued` / `bad` / `nonTried` / `nonTrimmed` / `nonScraped`)。
    pub status: &'static str,
}

impl From<&ofr_image::Block> for MapSegmentDto {
    fn from(b: &ofr_image::Block) -> Self {
        use ofr_image::BlockStatus as S;
        Self {
            pos: b.pos,
            len: b.size,
            status: match b.status {
                S::Finished => "rescued",
                S::BadSector => "bad",
                S::NonTried => "nonTried",
                S::NonTrimmed => "nonTrimmed",
                S::NonScraped => "nonScraped",
            },
        }
    }
}

/// イメージング完了時のサマリ。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSummaryDto {
    /// デバイス全長。
    pub total: u64,
    /// 取得できたバイト数。
    pub rescued: u64,
    /// 不良バイト数。
    pub bad: u64,
    /// 未取得のまま残ったバイト数。
    pub remaining: u64,
    /// 読み込みエラーの回数。
    pub errors: u64,
    /// デバイスを開き直した回数。
    pub reopens: u32,
    /// 所要秒数。
    pub elapsed_secs: f64,
    /// 中断されたか。
    pub cancelled: bool,
    /// 全域を取得できたか。
    pub complete: bool,
    /// 出力したイメージのパス。
    pub image_path: String,
    /// mapfile のパス。
    pub map_path: Option<String>,
}

/// カービングで切り出した 1 ファイル。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CarvedFileDto {
    /// 通し番号。プレビューのときに送り返す。
    pub index: u64,
    /// 付けた名前。
    pub name: String,
    /// 形式コード(`jpeg` / `png` / …)。
    pub format: &'static str,
    /// 拡張子。
    pub ext: &'static str,
    /// デバイス上の位置。
    pub offset: u64,
    /// サイズ。
    pub size: u64,
    /// 境界を確定できたか(`exact` / `truncated`)。
    pub confidence: &'static str,
    /// 読めなかったバイト数。
    pub bad_bytes: u64,
    /// 書き出した先(`--dry-run` 相当なら `None`)。
    pub output: Option<String>,
    /// 撮影日時などのメタデータ。
    pub metadata: CarvedMetadataDto,
}

/// 切り出したファイルから拾えたメタデータ(PLAN.md 5.4 の Exif 抽出)。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CarvedMetadataDto {
    /// 撮影日時。
    pub timestamp: Option<String>,
    /// 幅。
    pub width: Option<u32>,
    /// 高さ。
    pub height: Option<u32>,
    /// カメラのメーカー。
    pub camera_make: Option<String>,
    /// カメラの機種名。
    pub camera_model: Option<String>,
    /// 動画の長さ(ミリ秒)。
    pub duration_ms: Option<u64>,
}

impl CarvedFileDto {
    /// 切り出し結果から組み立てる。`dest` を渡すと出力先パスも埋める。
    pub fn new(f: &ofr_carve::CarvedFile, dest: Option<&Path>) -> Self {
        let m = &f.metadata;
        Self {
            index: f.index,
            name: f.file_name.clone(),
            format: f.format.name(),
            ext: f.extension,
            offset: f.offset,
            size: f.size,
            confidence: if f.confidence.is_exact() {
                "exact"
            } else {
                "truncated"
            },
            bad_bytes: f.bad_bytes,
            // 出力は形式ごとのサブフォルダに分かれる(ofr-carve の出力規則)。
            output: dest.map(|d| d.join(f.extension).join(&f.file_name).display().to_string()),
            metadata: CarvedMetadataDto {
                timestamp: m.timestamp.map(|t| t.to_string()),
                width: m.width,
                height: m.height,
                camera_make: m.camera_make.clone(),
                camera_model: m.camera_model.clone(),
                duration_ms: m.duration_ms,
            },
        }
    }
}

/// カービング全体のサマリ。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CarveSummaryDto {
    /// 走査したバイト数。
    pub scanned: u64,
    /// 見つけた数。
    pub found: u64,
    /// うち境界を確定できた数。
    pub exact: u64,
    /// 切り出したバイト数。
    pub bytes_recovered: u64,
    /// 読み込みエラーの回数。
    pub read_errors: u64,
    /// 所要秒数。
    pub elapsed_secs: f64,
    /// 中断されたか。
    pub cancelled: bool,
    /// 形式ごとの件数。
    pub by_format: Vec<FormatCountDto>,
    /// 出力先。
    pub output: Option<String>,
    /// JSON レポートのパス。
    pub report_path: Option<String>,
}

/// 形式ごとの件数。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatCountDto {
    /// 形式コード。
    pub format: &'static str,
    /// 件数。
    pub count: u64,
    /// 合計バイト数。
    pub bytes: u64,
}

/// コピー / 復元した 1 ファイルの記録。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileResultDto {
    /// 復旧元でのパス。
    pub source: String,
    /// 書き出した先。
    pub output: String,
    /// 元のサイズ。
    pub size: u64,
    /// 書き出したバイト数。
    pub written: u64,
    /// 読めずにゼロで埋めたバイト数。
    pub missing: u64,
    /// 状態コード(`copied` / `partial` / `failed` / `skipped`)。
    pub status: &'static str,
    /// 失敗した理由(日本語の自由文)。
    pub error: Option<String>,
}

impl From<&ofr_copy::FileResult> for FileResultDto {
    fn from(f: &ofr_copy::FileResult) -> Self {
        Self {
            source: f.source.clone(),
            output: f.output.display().to_string(),
            size: f.size,
            written: f.written,
            missing: f.missing,
            status: f.status.as_str(),
            error: f.error.clone(),
        }
    }
}

/// コピー / 復元全体のサマリ。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopySummaryDto {
    /// 対象になったファイル数。
    pub files: u64,
    /// 欠けなくコピーできた数。
    pub copied: u64,
    /// 一部が欠けた数。
    pub partial: u64,
    /// 失敗した数。
    pub failed: u64,
    /// 飛ばした数。
    pub skipped: u64,
    /// 作ったディレクトリ数。
    pub dirs: u64,
    /// 書き出したバイト数。
    pub bytes_written: u64,
    /// 読めずに埋めたバイト数。
    pub bytes_missing: u64,
    /// 所要秒数。
    pub elapsed_secs: f64,
    /// 中断されたか。
    pub cancelled: bool,
    /// 欠けも失敗も中断もなかったか。
    pub complete: bool,
    /// 宛先。
    pub destination: String,
    /// JSON レポートのパス。
    pub report_json: Option<String>,
    /// 人間向けサマリのパス。
    pub report_text: Option<String>,
}

impl CopySummaryDto {
    /// コピーのサマリから組み立てる。
    pub fn new(s: &ofr_copy::CopySummary, destination: &Path) -> Self {
        Self {
            files: s.files,
            copied: s.copied,
            partial: s.partial,
            failed: s.failed,
            skipped: s.skipped,
            dirs: s.dirs,
            bytes_written: s.bytes_written,
            bytes_missing: s.bytes_missing,
            elapsed_secs: s.elapsed.as_secs_f64(),
            cancelled: s.cancelled,
            complete: s.is_complete(),
            destination: destination.display().to_string(),
            report_json: None,
            report_text: None,
        }
    }
}

/// 修復の結果(PLAN.md 5.6)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairReportDto {
    /// 直したファイル。
    pub input: String,
    /// 出力先。
    pub output: Option<String>,
    /// 参照ファイル。
    pub reference: Option<String>,
    /// 形式コード(`jpeg` / `png` / `avi` / `mp4`)。
    pub format: String,
    /// 状態コード(`intact` / `repaired` / `partial` / `failed`)。
    pub status: &'static str,
    /// 入力サイズ。
    pub input_size: u64,
    /// 出力サイズ。
    pub output_size: u64,
    /// 直したこと(日本語の自由文)。
    pub fixes: Vec<String>,
    /// 直しきれなかったこと(日本語の自由文)。
    pub issues: Vec<String>,
    /// 検証の種別コード(`decoded` / `container` / `failed` / `skipped`)。
    pub verification: &'static str,
    /// 検証の詳細(日本語の自由文)。
    pub verification_detail: String,
    /// 検証が通ったか。
    pub verified: bool,
    /// 所要秒数。
    pub elapsed_secs: f64,
}

impl From<&ofr_repair::RepairReport> for RepairReportDto {
    fn from(r: &ofr_repair::RepairReport) -> Self {
        Self {
            input: r.input.display().to_string(),
            output: r.output.as_ref().map(|p| p.display().to_string()),
            reference: r.reference.as_ref().map(|p| p.display().to_string()),
            format: r.format.as_str().to_string(),
            status: r.status.as_str(),
            input_size: r.input_size,
            output_size: r.output_size,
            fixes: r.fixes.clone(),
            issues: r.issues.clone(),
            verification: r.verification.as_str(),
            verification_detail: r.verification.label(),
            verified: r.verification.passed(),
            elapsed_secs: r.elapsed.as_secs_f64(),
        }
    }
}

/// 進捗イベント。ジョブの種類によらず同じ形にしてある。
///
/// GUI 側の進捗表示を 1 つで済ませるため、使う項目だけ埋めて残りは 0 にする。
/// 発火は 100ms 間隔に間引かれている(PLAN.md 5.7)。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressDto {
    /// 段階コード。`copy` / `trim` / `scrape` / `retry`(イメージング)、
    /// `directories` / `orphans`(解析)、`carving`、`copying`、`restoring`。
    pub phase: &'static str,
    /// 何回目のパスか(イメージングのリトライ)。
    pub pass: u32,
    /// いま読んでいる位置。
    pub position: u64,
    /// 全長。
    pub total: u64,
    /// 進み具合(0.0〜1.0)。
    pub ratio: f64,
    /// 終わった項目数。
    pub items_done: u64,
    /// 対象の項目数。
    pub items_total: u64,
    /// 書き出したバイト数。
    pub bytes_done: u64,
    /// 対象の合計バイト数。
    pub bytes_total: u64,
    /// 取得できたバイト数(イメージング)。
    pub rescued: u64,
    /// 不良バイト数(イメージング)。
    pub bad: u64,
    /// 未取得バイト数(イメージング)。
    pub pending: u64,
    /// 見つかった数(解析 / カービング)。
    pub found: u64,
    /// エラー回数。
    pub errors: u64,
    /// 速度(バイト/秒)。
    pub rate: u64,
    /// 推定残り秒数。
    pub eta_secs: Option<u64>,
    /// 経過秒数。
    pub elapsed_secs: f64,
    /// いま処理している対象の名前。
    pub current: String,
    /// 領域マップ(イメージングのみ)。
    pub map: Vec<MapSegmentDto>,
}

/// 秒に丸めた残り時間。
pub fn eta_secs(eta: Option<Duration>) -> Option<u64> {
    eta.map(|d| d.as_secs())
}

/// 小文字の拡張子(ドットなし)。拡張子がなければ空文字。
pub fn extension_of(name: &str) -> String {
    match name.rsplit_once('.') {
        // 先頭がドットのファイル(`.gitignore`)は拡張子とみなさない。
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 8 => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_extensions() {
        assert_eq!(extension_of("IMG_0042.JPG"), "jpg");
        assert_eq!(extension_of("報告書.docx"), "docx");
        assert_eq!(extension_of("README"), "");
        assert_eq!(extension_of(".gitignore"), "");
    }
}
