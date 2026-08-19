//! ファイルシステムのタイムスタンプ。
//!
//! FAT32 も exFAT も、日時を「ローカル時刻の年月日時分秒」として持つ
//! (exFAT だけ UTC オフセットを別バイトで持てる)。暦の変換に外部クレートを
//! 足すほどの処理ではないので、必要な分だけここに置く。

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 日時。値はファイルシステムに書かれていたそのまま。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp {
    /// 西暦年。
    pub year: u16,
    /// 月(1〜12)。
    pub month: u8,
    /// 日(1〜31)。
    pub day: u8,
    /// 時(0〜23)。
    pub hour: u8,
    /// 分(0〜59)。
    pub minute: u8,
    /// 秒(0〜59)。
    pub second: u8,
    /// ミリ秒(FAT は 10ms 単位までしか持たない)。
    pub millis: u16,
    /// UTC からのオフセット(分)。不明なら `None`。
    pub utc_offset_minutes: Option<i16>,
}

impl Timestamp {
    /// FAT の日付・時刻ワードから作る。値が暦としてありえなければ `None`。
    ///
    /// - `date`: bit15..9 = 1980 年からの年、bit8..5 = 月、bit4..0 = 日
    /// - `time`: bit15..11 = 時、bit10..5 = 分、bit4..0 = 2 秒単位の秒
    /// - `fine`: 10ms 単位の補正(0〜199)。FAT の作成時刻だけが持つ
    pub fn from_fat(date: u16, time: u16, fine: u8) -> Option<Self> {
        if date == 0 {
            return None;
        }
        let year = 1980 + (date >> 9);
        let month = ((date >> 5) & 0x0F) as u8;
        let day = (date & 0x1F) as u8;
        let hour = (time >> 11) as u8;
        let minute = ((time >> 5) & 0x3F) as u8;
        let second = ((time & 0x1F) as u8).saturating_mul(2);

        let extra_seconds = fine / 100;
        let millis = (fine % 100) as u16 * 10;

        let ts = Timestamp {
            year,
            month,
            day,
            hour,
            minute,
            second: second.saturating_add(extra_seconds),
            millis,
            utc_offset_minutes: None,
        };
        ts.is_valid().then_some(ts)
    }

    /// exFAT の UTC オフセットバイトを反映する。
    ///
    /// bit7 が立っていれば有効で、下位 7bit が 15 分単位の符号付き値
    /// (2 の補数)。
    pub fn with_exfat_offset(mut self, raw: u8) -> Self {
        if raw & 0x80 != 0 {
            let mut quarters = (raw & 0x7F) as i16;
            if quarters >= 64 {
                quarters -= 128;
            }
            self.utc_offset_minutes = Some(quarters * 15);
        }
        self
    }

    /// 暦としてありえる値か。
    pub fn is_valid(&self) -> bool {
        (1980..=2107).contains(&self.year)
            && (1..=12).contains(&self.month)
            && self.day >= 1
            && self.day <= days_in_month(self.year, self.month)
            && self.hour < 24
            && self.minute < 60
            && self.second < 60
    }

    /// UNIX 時刻(秒)。UTC オフセットが分かっていればそれで補正する。
    pub fn unix_seconds(&self) -> i64 {
        let days = days_from_civil(self.year as i64, self.month, self.day);
        let secs =
            days * 86_400 + self.hour as i64 * 3600 + self.minute as i64 * 60 + self.second as i64;
        match self.utc_offset_minutes {
            Some(offset) => secs - offset as i64 * 60,
            // オフセット不明ならローカル時刻をそのまま UTC とみなす。
            // FAT のタイムスタンプにはタイムゾーン情報がないので、これ以上は分からない。
            None => secs,
        }
    }

    /// [`SystemTime`] に変換する。復元したファイルの更新日時を合わせるのに使う。
    pub fn to_system_time(&self) -> Option<SystemTime> {
        let secs = self.unix_seconds();
        let millis = self.millis as u32 % 1000;
        if secs >= 0 {
            UNIX_EPOCH.checked_add(Duration::new(secs as u64, millis * 1_000_000))
        } else {
            UNIX_EPOCH.checked_sub(Duration::from_secs(secs.unsigned_abs()))
        }
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// 1 つの項目が持つ日時の組。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timestamps {
    /// 作成日時。
    pub created: Option<Timestamp>,
    /// 更新日時。
    pub modified: Option<Timestamp>,
    /// アクセス日時。
    pub accessed: Option<Timestamp>,
}

impl Timestamps {
    /// 表示に使う代表値(更新 → 作成 → アクセスの順)。
    pub fn best(&self) -> Option<Timestamp> {
        self.modified.or(self.created).or(self.accessed)
    }
}

fn is_leap(year: u16) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// 1970-01-01 からの日数。Howard Hinnant の days_from_civil と同じ式。
fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_fat_date_and_time() {
        // 2026-08-19 12:34:56
        let date = ((2026 - 1980) << 9) | (8 << 5) | 19;
        let time = (12 << 11) | (34 << 5) | (56 / 2);
        let ts = Timestamp::from_fat(date, time, 0).unwrap();
        assert_eq!(ts.to_string(), "2026-08-19 12:34:56");
    }

    #[test]
    fn rejects_impossible_dates() {
        assert!(Timestamp::from_fat(0, 0, 0).is_none());
        // 2 月 30 日。
        let date = ((2026 - 1980) << 9) | (2 << 5) | 30;
        assert!(Timestamp::from_fat(date, 0, 0).is_none());
        // 13 月。
        let date = ((2026 - 1980) << 9) | (13 << 5) | 1;
        assert!(Timestamp::from_fat(date, 0, 0).is_none());
    }

    #[test]
    fn carries_the_ten_millisecond_field() {
        let date = ((2026 - 1980) << 9) | (1 << 5) | 1;
        let ts = Timestamp::from_fat(date, 1, 150).unwrap();
        assert_eq!(ts.second, 3); // 2 秒 + 100 * 10ms
        assert_eq!(ts.millis, 500);
    }

    #[test]
    fn converts_to_unix_seconds() {
        let epoch = Timestamp {
            year: 1970,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            millis: 0,
            utc_offset_minutes: None,
        };
        assert_eq!(epoch.unix_seconds(), 0);

        let y2k = Timestamp {
            year: 2000,
            month: 1,
            day: 1,
            ..epoch
        };
        assert_eq!(y2k.unix_seconds(), 946_684_800);

        // UTC+9 のローカル時刻は UTC では 9 時間前。
        let jst = Timestamp {
            utc_offset_minutes: Some(540),
            ..y2k
        };
        assert_eq!(jst.unix_seconds(), 946_684_800 - 9 * 3600);
    }

    #[test]
    fn decodes_exfat_utc_offsets() {
        let base = Timestamp::from_fat(((2026 - 1980) << 9) | (1 << 5) | 1, 0, 0).unwrap();
        assert_eq!(
            base.with_exfat_offset(0x80 | 36).utc_offset_minutes,
            Some(540)
        );
        // -8 時間 = -32 * 15 分。7bit の 2 の補数で 0x60。
        assert_eq!(
            base.with_exfat_offset(0x80 | 0x60).utc_offset_minutes,
            Some(-480)
        );
        assert_eq!(base.with_exfat_offset(0).utc_offset_minutes, None);
    }
}
