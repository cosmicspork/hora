//! Instants are stored as RFC 3339 with the offset they were recorded in, so an
//! entry keeps the wall-clock time it was worked at even after travel, plus
//! epoch milliseconds for ordering and overlap checks.

use chrono::{
    DateTime, FixedOffset, Local, LocalResult, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone,
    Timelike,
};

pub fn now() -> DateTime<FixedOffset> {
    // Whole seconds keep stored values and spans tidy.
    let now = Local::now().fixed_offset();
    now.with_nanosecond(0).unwrap_or(now)
}

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

pub fn format(instant: &DateTime<FixedOffset>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn utc_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn parse_date(raw: &str) -> Result<NaiveDate, String> {
    let date = NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| format!("`{raw}` is not a date; use YYYY-MM-DD"))?;
    if !(2000..=2100).contains(&chrono::Datelike::year(&date)) {
        return Err(format!("`{raw}` is out of range"));
    }
    Ok(date)
}

/// RFC 3339 with an offset, or a naive `YYYY-MM-DD HH:MM[:SS]` (space or `T`)
/// read as the server's local time.
pub fn parse_instant(raw: &str) -> Result<DateTime<FixedOffset>, String> {
    parse_instant_in(raw, &Local)
}

pub fn parse_instant_in<Tz: TimeZone>(
    raw: &str,
    zone: &Tz,
) -> Result<DateTime<FixedOffset>, String> {
    let raw = raw.trim();
    if let Ok(instant) = DateTime::parse_from_rfc3339(raw) {
        return Ok(instant.with_nanosecond(0).unwrap_or(instant));
    }
    let naive = [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ]
    .iter()
    .find_map(|pattern| NaiveDateTime::parse_from_str(raw, pattern).ok())
    .ok_or_else(|| {
        format!("`{raw}` is not a time; use RFC 3339 or YYYY-MM-DD HH:MM in local time")
    })?;
    match zone.from_local_datetime(&naive) {
        LocalResult::Single(instant) => Ok(instant.fixed_offset()),
        LocalResult::Ambiguous(earlier, _) => Ok(earlier.fixed_offset()),
        LocalResult::None => Err(format!(
            "`{raw}` does not exist in local time (a clock change); give an offset"
        )),
    }
}

/// Whole minutes between two instants, rounded to nearest.
pub fn span_minutes(start: &DateTime<FixedOffset>, end: &DateTime<FixedOffset>) -> i64 {
    let seconds = (*end - *start).num_seconds();
    (seconds + 30) / 60
}
