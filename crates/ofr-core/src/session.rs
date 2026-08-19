//! 解析 / カービング結果の保持。
//!
//! GUI は「スキャン → 結果ツリーを見る → 選んで復元」と進む(PLAN.md 7章)。
//! 途中でデバイスを開き直すと壊れかけメディアを余計に触ることになるので、
//! 解析に使ったデバイスと結果をここに置いて使い回す。
//!
//! プレビュー(サムネイル)もここから読む。復旧ソフトの信頼感は
//! 「本当に中身が残っているか」を目で確かめられるかで決まる(PLAN.md 7章 4)。

use std::path::PathBuf;
use std::sync::Arc;

use ofr_device::{Device, SliceDevice};
use ofr_fs::{EntryStatus, FileTree, FsKind};
use serde::Serialize;

use crate::dto::{CarvedFileDto, EntryDto};
use crate::error::{CoreError, Result};
use crate::filter::Filter;

/// プレビューで読み出す既定の上限。
///
/// 画像 1 枚をブラウザに渡すぶんには十分で、これ以上は webview に送る
/// コストのほうが問題になる。
pub const DEFAULT_PREVIEW_LIMIT: u64 = 8 << 20;

/// 解析結果 1 件。
pub struct ScanSession {
    /// 復旧元の指定文字列。
    pub source: String,
    /// 解析に使ったデバイス。
    pub device: Arc<dyn Device>,
    /// ボリュームの開始位置。
    pub offset: u64,
    /// ボリュームの長さ。
    pub len: u64,
    /// ファイルシステムの種別。
    pub kind: FsKind,
    /// 見つかった項目。
    pub tree: FileTree,
}

impl ScanSession {
    /// ボリュームだけを見せるビューを作る。
    pub fn region(&self) -> Result<SliceDevice<Arc<dyn Device>>> {
        Ok(SliceDevice::new(
            Arc::clone(&self.device),
            self.offset,
            self.len,
        )?)
    }

    /// 選ばれた項目を、ディレクトリの中身まで展開してファイルだけにする。
    ///
    /// GUI ではフォルダのチェックボックスを 1 つ押すのが自然なので、
    /// 「フォルダを選んだら中身も全部」を核側で解決する。空なら全ファイル。
    pub fn expand(&self, selected: &[usize]) -> Vec<usize> {
        if selected.is_empty() {
            return self
                .tree
                .entries()
                .iter()
                .filter(|e| !e.is_dir())
                .map(|e| e.id)
                .collect();
        }

        let mut out = Vec::new();
        let mut seen = vec![false; self.tree.len()];
        let mut stack: Vec<usize> = selected.to_vec();
        while let Some(id) = stack.pop() {
            if id >= self.tree.len() || seen[id] {
                continue;
            }
            seen[id] = true;
            let Some(entry) = self.tree.get(id) else {
                continue;
            };
            if entry.is_dir() {
                stack.extend_from_slice(self.tree.children(id));
            } else {
                out.push(id);
            }
        }
        // 選んだ順ではなくツリーの順に戻す。復元ログが読みやすくなる。
        out.sort_unstable();
        out
    }
}

/// カービング結果 1 件。
pub struct CarveSession {
    /// 復旧元の指定文字列。
    pub source: String,
    /// 走査に使ったデバイス。
    pub device: Arc<dyn Device>,
    /// 切り出したファイル。
    pub files: Vec<ofr_carve::CarvedFile>,
    /// 書き出し先。
    pub output: Option<PathBuf>,
}

/// ジョブが残した結果。
pub enum Session {
    /// ファイルシステム解析。
    Scan(Box<ScanSession>),
    /// カービング。
    Carve(Box<CarveSession>),
}

/// 項目の取り出し条件。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryQuery {
    /// 名前かパスのパターン(`*.jpg` など)。
    #[serde(default)]
    pub include: Vec<String>,
    /// 状態コード(`deleted` など)。
    #[serde(default)]
    pub statuses: Vec<String>,
    /// ディレクトリを含めないか。
    #[serde(default)]
    pub files_only: bool,
    /// 何件目から返すか。
    #[serde(default)]
    pub offset: usize,
    /// 何件返すか。0 なら既定値。
    #[serde(default)]
    pub limit: usize,
}

/// 1 ページぶんの項目。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryPage {
    /// 条件に合う総数。
    pub total: usize,
    /// このページの開始位置。
    pub offset: usize,
    /// 項目。
    pub entries: Vec<EntryDto>,
    /// 条件に合うもののうち、ファイルの数と合計バイト数。
    pub files: usize,
    /// 復元したときに書き出されるバイト数の合計。
    pub bytes: u64,
}

/// 1 ページの既定の件数。
pub const DEFAULT_PAGE: usize = 2000;

impl ScanSession {
    /// 条件に合う項目を 1 ページ取り出す。
    pub fn page(&self, query: &EntryQuery) -> EntryPage {
        let filter = Filter {
            include: query.include.clone(),
            statuses: Vec::new(),
        }
        .with_status_codes(&query.statuses);

        let matched: Vec<&ofr_fs::RecoveredEntry> = self
            .tree
            .entries()
            .iter()
            .filter(|e| !(query.files_only && e.is_dir()))
            .filter(|e| filter.matches(e))
            .collect();

        let limit = if query.limit == 0 {
            DEFAULT_PAGE
        } else {
            query.limit
        };
        let entries = matched
            .iter()
            .skip(query.offset)
            .take(limit)
            .map(|e| EntryDto::from(*e))
            .collect();

        EntryPage {
            total: matched.len(),
            offset: query.offset,
            entries,
            files: matched.iter().filter(|e| !e.is_dir()).count(),
            bytes: matched
                .iter()
                .filter(|e| !e.is_dir())
                .map(|e| e.recoverable_bytes())
                .sum(),
        }
    }
}

/// プレビュー用に読み出した中身。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDto {
    /// 名前。
    pub name: String,
    /// 推定した MIME タイプ。
    pub mime: String,
    /// 中身(base64)。webview には data URL として渡す。
    pub data: String,
    /// 読み出したバイト数。
    pub bytes: u64,
    /// 上限で打ち切ったか。
    pub truncated: bool,
}

impl Session {
    /// プレビューを読み出す。`index` は解析なら項目 ID、カービングなら通し番号。
    pub fn preview(&self, index: usize, limit: u64) -> Result<PreviewDto> {
        let limit = if limit == 0 {
            DEFAULT_PREVIEW_LIMIT
        } else {
            limit
        };
        match self {
            Session::Scan(s) => {
                let entry = s
                    .tree
                    .get(index)
                    .ok_or_else(|| CoreError::BadRequest(format!("項目 {index} がない")))?;
                if entry.is_dir() {
                    return Err(CoreError::BadRequest(
                        "ディレクトリはプレビューできない".to_string(),
                    ));
                }
                let region = s.region()?;
                let want = entry.recoverable_bytes().min(limit);
                let mut data = Vec::with_capacity(want as usize);
                for extent in &entry.extents {
                    if data.len() as u64 >= want {
                        break;
                    }
                    let take = (want - data.len() as u64).min(extent.len) as usize;
                    let mut buf = vec![0u8; take];
                    // 読めない部分はゼロのまま残し、長さは詰めない。位置がずれないので、
                    // 途中から壊れたファイルでも「どこまで残っているか」を目で確かめられる。
                    if region.read_at(extent.offset, &mut buf).is_err() {
                        // 失敗したときの buf の中身は不定 (Device の契約)。
                        buf.fill(0);
                    }
                    data.extend_from_slice(&buf);
                }
                Ok(PreviewDto {
                    name: entry.name.clone(),
                    mime: mime_for(&entry.name),
                    bytes: data.len() as u64,
                    truncated: (data.len() as u64) < entry.recoverable_bytes(),
                    data: base64(&data),
                })
            }
            Session::Carve(c) => {
                let file = c
                    .files
                    .iter()
                    .find(|f| f.index as usize == index)
                    .ok_or_else(|| CoreError::BadRequest(format!("切り出し {index} がない")))?;
                let want = file.size.min(limit);
                let mut data = vec![0u8; want as usize];
                let n = c.device.read_at(file.offset, &mut data).unwrap_or(0);
                data.truncate(n);
                Ok(PreviewDto {
                    name: file.file_name.clone(),
                    mime: mime_for(&file.file_name),
                    bytes: data.len() as u64,
                    truncated: (data.len() as u64) < file.size,
                    data: base64(&data),
                })
            }
        }
    }
}

/// 拡張子から MIME タイプを当てる。プレビューできない形式は
/// `application/octet-stream` にして、GUI 側でアイコン表示に落とす。
pub fn mime_for(name: &str) -> String {
    let ext = crate::dto::extension_of(name);
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "heic" | "heif" => "image/heic",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "pdf" => "application/pdf",
        "txt" | "log" | "md" => "text/plain",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// base64 に変換する。
///
/// 依存を増やさないための最小実装(webview へ画像を渡すのにしか使わない)。
pub fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        out.push(TABLE[(n >> 18 & 63) as usize] as char);
        out.push(TABLE[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// カービング結果を GUI 用の形にする。
pub fn carved_dtos(session: &CarveSession) -> Vec<CarvedFileDto> {
    session
        .files
        .iter()
        .map(|f| CarvedFileDto::new(f, session.output.as_deref()))
        .collect()
}

/// 状態コードの一覧(GUI の絞り込み UI 用)。
pub fn all_statuses() -> [EntryStatus; 4] {
    [
        EntryStatus::Intact,
        EntryStatus::Deleted,
        EntryStatus::Orphaned,
        EntryStatus::Damaged,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xd8, 0xff]), "/9j/");
    }

    #[test]
    fn guesses_mime_types() {
        assert_eq!(mime_for("IMG_0042.JPG"), "image/jpeg");
        assert_eq!(mime_for("a.png"), "image/png");
        assert_eq!(mime_for("a.bin"), "application/octet-stream");
    }
}
