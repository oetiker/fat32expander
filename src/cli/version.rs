//! The `version` subcommand's text: package version, build time and commit.

use std::time::{Duration, UNIX_EPOCH};

const BUILD_TIMESTAMP: u64 = const_parse_u64(env!("BUILD_TIMESTAMP"));
const GIT_HASH: &str = env!("GIT_HASH");

const fn const_parse_u64(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let mut result: u64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        result = result * 10 + (bytes[i] - b'0') as u64;
        i += 1;
    }
    result
}

fn format_build_time() -> String {
    let dt = UNIX_EPOCH + Duration::from_secs(BUILD_TIMESTAMP);
    let secs = dt.duration_since(UNIX_EPOCH).unwrap().as_secs();
    // Simple UTC formatting: YYYY-MM-DD HH:MM:SS
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Calculate year/month/day from days since 1970-01-01
    let (year, month, day) = days_to_ymd(days_since_epoch);

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        year, month, day, hours, minutes, seconds
    )
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let mut remaining = days as i64;
    let mut year = 1970i64;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        year += 1;
    }

    let days_in_months: [i64; 12] = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1i64;
    for days in days_in_months {
        if remaining < days {
            break;
        }
        remaining -= days;
        month += 1;
    }

    (year as u64, month as u64, (remaining + 1) as u64)
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

pub fn version_long() -> String {
    format!(
        "{} (built {} git:{})",
        env!("CARGO_PKG_VERSION"),
        format_build_time(),
        GIT_HASH
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Safety net for [F-25]: the calendar arithmetic behind the build date
    /// that `version` prints, across year ends and leap rules.
    #[test]
    fn days_since_the_epoch_convert_to_calendar_dates() {
        for (days, date) in [
            (0, (1970, 1, 1)),
            (364, (1970, 12, 31)),
            (789, (1972, 2, 29)),
            (790, (1972, 3, 1)),
            (11016, (2000, 2, 29)),
            (11322, (2000, 12, 31)),
            (20735, (2026, 10, 9)),
            (47540, (2100, 2, 28)),
            (47541, (2100, 3, 1)),
        ] {
            assert_eq!(days_to_ymd(days), date, "day {days}");
        }
    }
}
