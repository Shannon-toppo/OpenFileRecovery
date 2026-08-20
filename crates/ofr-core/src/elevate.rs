//! 権限の確認と昇格(PLAN.md 5.1 / 10章)。
//!
//! 生デバイスの読み込みには管理者 / root 権限が要る。列挙だけは権限なしでも
//! 動くので、GUI は「一覧は出るが開けない」状態になりやすい。ここで
//! いまの権限を調べ、足りなければ GUI が案内を出せるようにする。
//!
//! - **Windows**: インストールしたアプリは manifest の `requireAdministrator` で
//!   起動時に UAC が出るので、実行できている時点で昇格済みになる。開発ビルドを
//!   直接叩いた場合だけ非昇格になりうるので、そのときは案内を出す。
//! - **macOS**: GUI から root を取る素直な方法がない。`osascript` の
//!   administrator privileges で自分自身を root として起動し直す経路を用意し、
//!   それが嫌な利用者向けに `sudo` で CLI を叩く案内も出す(PLAN.md 10章)。
//!
//! イメージファイルの解析には権限が要らない。壊れかけメディアは
//! 「まず sudo で吸い出し → 以後はイメージを解析」で権限問題を回避できる
//! (PLAN.md 6章 4項)ので、GUI はこの逃げ道も併せて案内する。

use serde::Serialize;

use crate::error::{CoreError, Result};

/// いまの権限。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivilegeDto {
    /// 管理者 / root で動いているか。
    pub elevated: bool,
    /// 動いている OS(`macos` / `windows` / `other`)。
    pub platform: &'static str,
    /// この場で昇格して起動し直せるか。
    pub can_relaunch: bool,
    /// 生デバイスを読むのに昇格が要るか。
    pub needed_for_raw_device: bool,
}

/// いまの権限を調べる。
pub fn state() -> PrivilegeDto {
    PrivilegeDto {
        elevated: is_elevated(),
        platform: platform(),
        can_relaunch: cfg!(target_os = "macos"),
        needed_for_raw_device: true,
    }
}

/// 動いている OS。
pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "other"
    }
}

/// 管理者 / root で動いているか。
pub fn is_elevated() -> bool {
    sys::is_elevated()
}

/// 自分自身を管理者権限で起動し直す。
///
/// 呼び出し側は、これが成功したら**いまのプロセスを終了する**こと。
/// 同じアプリが 2 つ立ち上がったままになる。
pub fn relaunch_elevated() -> Result<()> {
    sys::relaunch_elevated()
}

#[cfg(target_os = "macos")]
mod sys {
    // geteuid の呼び出しにだけ unsafe を使う。
    #![allow(unsafe_code)]

    use std::process::Command;

    use crate::error::{CoreError, Result};

    pub(super) fn is_elevated() -> bool {
        // SAFETY: geteuid はスレッド安全で、引数も戻り値も単純な整数。
        unsafe { libc::geteuid() == 0 }
    }

    pub(super) fn relaunch_elevated() -> Result<()> {
        let exe = std::env::current_exe().map_err(|e| CoreError::Io {
            path: std::path::PathBuf::from("(current_exe)"),
            source: e,
        })?;

        // AppleScript の文字列に埋めるので、\ と " をエスケープする。
        // さらに shell script として解釈されるので、パスは ' で囲む。
        let path = exe.display().to_string();
        if path.contains('\'') {
            return Err(CoreError::BadRequest(
                "アプリのパスに ' が含まれているので昇格して起動し直せない".to_string(),
            ));
        }
        let script = format!(
            "do shell script \"'{}' >/dev/null 2>&1 &\" with administrator privileges",
            path.replace('\\', "\\\\").replace('"', "\\\"")
        );

        let status = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(&script)
            .status()
            .map_err(|e| CoreError::Io {
                path: std::path::PathBuf::from("/usr/bin/osascript"),
                source: e,
            })?;
        if !status.success() {
            // 利用者がパスワード入力をやめた場合もここに来る。
            return Err(CoreError::BadRequest(
                "管理者権限で起動できません(またはパスワードが違います)".to_string(),
            ));
        }
        Ok(())
    }
}

#[cfg(windows)]
mod sys {
    // トークンの照会にだけ unsafe を使う。
    #![allow(unsafe_code)]

    use std::mem::size_of;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    use crate::error::{CoreError, Result};

    pub(super) fn is_elevated() -> bool {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: 自プロセスのトークンを照会するだけ。取得できたら必ず閉じる。
        unsafe {
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut size = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                (&raw mut elevation).cast(),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut size,
            );
            CloseHandle(token);
            ok != 0 && elevation.TokenIsElevated != 0
        }
    }

    pub(super) fn relaunch_elevated() -> Result<()> {
        // インストール版は manifest の requireAdministrator で起動時に昇格する。
        // 開発ビルドを直接叩いた場合は、管理者として実行し直してもらう。
        Err(CoreError::BadRequest(
            "Windowsでは管理者としてアプリを起動し直してください (インストール版は起動時にUACが出ます)"
                .to_string(),
        ))
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod sys {
    use crate::error::{CoreError, Result};

    pub(super) fn is_elevated() -> bool {
        false
    }

    pub(super) fn relaunch_elevated() -> Result<()> {
        Err(CoreError::BadRequest("このOSでは未対応です".to_string()))
    }
}

/// 権限不足のエラーに添える案内(日本語)。
pub fn permission_hint(source: &str) -> String {
    match platform() {
        "macos" => format!(
            "{source} を読み込むにはroot権限が必要です。「管理者で実行し直す」を押すか、\
                ターミナルで`sudo ofr image {source} <出力先>`を実行してイメージを取り、\
                そのイメージを開いてください。"
        ),
        "windows" => format!(
            "{source} を読むには管理者権限が必要です。アプリを右クリックして「管理者として実行」で開き直してください。"
        ),
        _ => format!("{source} を読む権限がありません。"),
    }
}

impl CoreError {
    /// 権限不足なら、GUI に出す案内を返す。
    pub fn hint(&self, source: &str) -> Option<String> {
        match self.code() {
            crate::ErrorCode::PermissionDenied => Some(permission_hint(source)),
            crate::ErrorCode::Busy => Some(format!(
                "{source} は使用中です。macOSなら「開始前にアンマウントする」を有効にして実行し直してください。"
            )),
            _ => None,
        }
    }
}
