//! Calendar dates, just enough to name the trash's day folders and count the
//! days since. Days are UTC: the folders only need to sort and subtract
//! consistently, and pulling in a time-zone database for that is not worth it.

use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

/// A day, held as days since 1970-01-01.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date(i64);

impl Date {
    pub fn today() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    pub fn from_system_time(time: SystemTime) -> Self {
        let secs = match time.duration_since(UNIX_EPOCH) {
            Ok(after) => after.as_secs() as i64,
            Err(before) => -(before.duration().as_secs() as i64) - 1,
        };
        Self(secs.div_euclid(86_400))
    }

    pub fn from_ymd(year: i64, month: u32, day: u32) -> Option<Self> {
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        Some(Self(days_from_civil(year, month, day)))
    }

    /// Seconds since the epoch at the start of this day, UTC.
    pub fn unix_seconds(self) -> i64 {
        self.0 * 86_400
    }

    /// Whole days from `earlier` to `self`.
    pub fn days_since(self, earlier: Date) -> i64 {
        self.0 - earlier.0
    }

    fn ymd(self) -> (i64, u32, u32) {
        civil_from_days(self.0)
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (y, m, d) = self.ymd();
        write!(f, "{y:04}-{m:02}-{d:02}")
    }
}

#[derive(Debug, thiserror::Error)]
#[error("not a YYYY-MM-DD date: {0}")]
pub struct ParseDateError(String);

impl FromStr for Date {
    type Err = ParseDateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let fail = || ParseDateError(s.to_owned());
        let bytes = s.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return Err(fail());
        }
        let digits = |range: std::ops::Range<usize>| {
            let part = &s[range];
            if part.bytes().all(|b| b.is_ascii_digit()) {
                part.parse::<u32>().ok()
            } else {
                None
            }
        };
        let (Some(y), Some(m), Some(d)) = (digits(0..4), digits(5..7), digits(8..10)) else {
            return Err(fail());
        };
        Self::from_ymd(i64::from(y), m, d).ok_or_else(fail)
    }
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

// Howard Hinnant's algorithms for the proleptic Gregorian calendar.

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn round_trips_through_its_text_form() {
        for text in ["1970-01-01", "2000-02-29", "2026-11-24", "2100-12-31"] {
            assert_eq!(text.parse::<Date>().unwrap().to_string(), text);
        }
    }

    #[test]
    fn refuses_what_is_not_a_date() {
        for text in [
            "2026-02-29",
            "2026-13-01",
            "2026-1-01",
            "trash",
            "2026-11-2x",
            "",
        ] {
            assert!(text.parse::<Date>().is_err(), "{text}");
        }
    }

    #[test]
    fn counts_days_across_month_and_year_ends() {
        let a: Date = "2026-12-30".parse().unwrap();
        let b: Date = "2027-01-29".parse().unwrap();
        assert_eq!(b.days_since(a), 30);
    }

    #[test]
    fn reads_the_day_from_a_system_time() {
        let time = UNIX_EPOCH + Duration::from_secs(1_795_000_000);
        assert_eq!(Date::from_system_time(time).to_string(), "2026-11-18");
    }
}
