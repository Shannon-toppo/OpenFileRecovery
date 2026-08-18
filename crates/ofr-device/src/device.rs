//! 読み取り専用デバイス抽象。
//!
//! 実デバイス・イメージファイル・テスト用モックを同じ型で扱うための trait。
//! 上位クレート(ofr-image / ofr-fat / ofr-exfat / ofr-carve / ofr-copy)は
//! この trait だけを見るので、OS 非依存かつユニットテスト可能になる。
//!
//! # 安全原則
//!
//! この trait には**書き込みAPIを追加しないこと**。復旧元デバイスへの書き込み経路を
//! コンパイル時点で存在させないのが目的(PLAN.md 6章 1項)。

use std::fmt;

use crate::error::{DeviceError, Result};

/// デバイスの種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DeviceKind {
    /// 物理ディスク全体(`\\.\PhysicalDrive2`, `/dev/rdisk4` など)。
    PhysicalDisk,
    /// パーティション/ボリューム単位。
    Volume,
    /// ディスクイメージファイル(.img など)。
    ImageFile,
    /// テスト用の合成デバイス。
    Mock,
}

impl fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            DeviceKind::PhysicalDisk => "physical-disk",
            DeviceKind::Volume => "volume",
            DeviceKind::ImageFile => "image-file",
            DeviceKind::Mock => "mock",
        };
        f.write_str(s)
    }
}

/// GUI/CLI に見せるデバイスの識別情報。
///
/// デバイス列挙(Phase 1)が埋める。イメージやモックでは最小限の値が入る。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeviceInfo {
    /// 一意な識別子。OS が使うパス表現をそのまま入れる。
    pub id: String,
    /// 画面表示用の名前(製品名やファイル名)。
    pub display_name: String,
    /// 種別。
    pub kind: DeviceKind,
    /// 全長(バイト)。不明なら 0。
    pub size_bytes: u64,
    /// 論理ブロック(セクタ)サイズ。
    pub block_size: u32,
    /// リムーバブルメディアか。
    pub removable: bool,
    /// OS 起動ディスクか。真なら復旧元として選択させない(PLAN.md 6章 3項)。
    pub is_system_disk: bool,
    /// シリアル番号(取得できた場合)。
    pub serial: Option<String>,
}

impl DeviceInfo {
    /// 最低限の項目だけを埋めた [`DeviceInfo`] を作る。
    pub fn new(
        id: impl Into<String>,
        display_name: impl Into<String>,
        kind: DeviceKind,
        size_bytes: u64,
        block_size: u32,
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            kind,
            size_bytes,
            block_size,
            removable: false,
            is_system_disk: false,
            serial: None,
        }
    }

    /// 復旧元として選択してよいか(PLAN.md 6章 3項)。
    pub fn is_selectable_as_source(&self) -> bool {
        !self.is_system_disk && self.size_bytes > 0
    }
}

/// 読み取り専用のブロックデバイス。
///
/// # 実装の契約
///
/// - `read_at` は任意のオフセット・任意の長さを受け付ける。整列が必要な
///   バックエンド(Windows の `FILE_FLAG_NO_BUFFERING` 等)は内部で吸収する。
/// - `offset >= len()` のときは `Ok(0)` を返す。末尾をまたぐ要求は
///   デバイス末尾までに切り詰めて、読めたバイト数を返す。
/// - 読み込み範囲内でエラーが起きた場合は `Err` を返す。このとき `buf` の内容は不定。
/// - `&self` で読めること。複数スレッドから共有される(ただし壊れかけデバイスへの
///   実アクセスは PLAN.md 5.7 に従い IO スレッド1本に固定する)。
pub trait Device: Send + Sync {
    /// 指定オフセットから `buf` へ読み込み、読めたバイト数を返す。
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize>;

    /// デバイスの全長(バイト)。
    fn len(&self) -> u64;

    /// 論理ブロック(セクタ)サイズ。
    fn block_size(&self) -> u32;

    /// デバイスの識別情報。
    fn info(&self) -> &DeviceInfo;

    /// 長さが 0 か。
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `buf` を必ず満たすまで読む。足りなければ [`DeviceError::UnexpectedEof`]。
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let needed = buf.len();
        let mut done = 0usize;
        while done < needed {
            let n = self.read_at(offset + done as u64, &mut buf[done..])?;
            if n == 0 {
                return Err(DeviceError::UnexpectedEof {
                    offset,
                    needed,
                    got: done,
                });
            }
            done += n;
        }
        Ok(())
    }

    /// `len` バイトを新しい `Vec` に読み込む。
    fn read_vec_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.read_exact_at(offset, &mut buf)?;
        Ok(buf)
    }
}

impl<D: Device + ?Sized> Device for &D {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        (**self).read_at(offset, buf)
    }
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn block_size(&self) -> u32 {
        (**self).block_size()
    }
    fn info(&self) -> &DeviceInfo {
        (**self).info()
    }
}

impl<D: Device + ?Sized> Device for Box<D> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        (**self).read_at(offset, buf)
    }
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn block_size(&self) -> u32 {
        (**self).block_size()
    }
    fn info(&self) -> &DeviceInfo {
        (**self).info()
    }
}

impl<D: Device + ?Sized> Device for std::sync::Arc<D> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        (**self).read_at(offset, buf)
    }
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn block_size(&self) -> u32 {
        (**self).block_size()
    }
    fn info(&self) -> &DeviceInfo {
        (**self).info()
    }
}

/// 読み込み要求をデバイス末尾までに切り詰める。
///
/// `offset` が末尾以降なら `None`(= `Ok(0)` を返すべきケース)。
pub(crate) fn clamp_read(offset: u64, buf_len: usize, device_len: u64) -> Option<usize> {
    if offset >= device_len {
        return None;
    }
    let remaining = device_len - offset;
    Some((buf_len as u64).min(remaining) as usize)
}
