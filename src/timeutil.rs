use time::format_description::well_known::{Rfc2822, Rfc3339};
pub use time::{Duration, OffsetDateTime};

pub fn now_utc() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

pub fn now_unix() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

pub fn now_rfc3339() -> String {
    format_rfc3339(OffsetDateTime::now_utc())
}

fn to_utc_safe(dt: OffsetDateTime) -> OffsetDateTime {
    dt.checked_to_offset(time::UtcOffset::UTC).unwrap_or(dt)
}

fn format_year(year: i32) -> String {
    if year < 0 {
        format!("-{:04}", year.unsigned_abs())
    } else {
        format!("{year:04}")
    }
}

/// Format an instant in fixed-width UTC RFC 3339 (`YYYY-MM-DDTHH:MM:SS.nnnnnnnnn+00:00`)
/// so ASCII ordering matches chronology across subseconds and pre-upgrade `+00:00` records.
pub fn format_rfc3339(dt: OffsetDateTime) -> String {
    let utc = to_utc_safe(dt);
    let (year, month, day) = utc.to_calendar_date();
    let (hour, minute, second, nano) = utc.to_hms_nano();
    let month = u8::from(month);
    let sign_offset = if utc.offset() == time::UtcOffset::UTC {
        "+00:00".to_string()
    } else {
        let total = utc.offset().whole_seconds();
        let sign = if total < 0 { '-' } else { '+' };
        let abs = total.unsigned_abs();
        format!("{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60)
    };
    format!(
        "{}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{nano:09}{sign_offset}",
        format_year(year)
    )
}

pub fn format_rfc3339_secs_z(dt: OffsetDateTime) -> String {
    let utc = to_utc_safe(dt);
    let (year, month, day) = utc.to_calendar_date();
    let (hour, minute, second, _) = utc.to_hms_nano();
    let month = u8::from(month);
    format!(
        "{}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z",
        format_year(year)
    )
}

pub fn parse_rfc3339(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339)
        .ok()
        .and_then(|dt| dt.checked_to_offset(time::UtcOffset::UTC))
        .filter(|dt| (0..=9999).contains(&dt.year()))
}

pub fn parse_rfc2822(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc2822)
        .ok()
        .and_then(|dt| dt.checked_to_offset(time::UtcOffset::UTC))
        .filter(|dt| (0..=9999).contains(&dt.year()))
}

pub fn format_rfc2822(dt: OffsetDateTime) -> Option<String> {
    dt.checked_to_offset(time::UtcOffset::UTC)?
        .format(&Rfc2822)
        .ok()
}

pub fn from_unix_secs(secs: i64) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .filter(|dt| (0..=9999).contains(&dt.year()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_rfc3339_is_fixed_width_and_lexicographically_sorted() {
        let base = parse_rfc3339("2026-04-17T12:00:00Z").unwrap();
        let t0 = format_rfc3339(base);
        let t1 = format_rfc3339(base + Duration::milliseconds(100));
        let t2 = format_rfc3339(base + Duration::milliseconds(120));
        assert_eq!(t0, "2026-04-17T12:00:00.000000000+00:00");
        assert_eq!(t1, "2026-04-17T12:00:00.100000000+00:00");
        assert_eq!(t2, "2026-04-17T12:00:00.120000000+00:00");
        assert!(t0 < t1 && t1 < t2);

        // Pre-upgrade `+00:00` record in an earlier second or same whole second
        assert!("2026-04-17T12:00:00+00:00" < t1.as_str());
        assert!("2026-04-17T11:59:59.9+00:00" < t0.as_str());
    }

    #[test]
    fn extreme_dates_do_not_panic() {
        assert!(parse_rfc3339("9999-12-31T23:30:00-01:00").is_none());
        assert!(from_unix_secs(i64::MAX).is_none());
        assert!(from_unix_secs(-100_000_000_000).is_none());
        let formatted = format_rfc3339(OffsetDateTime::UNIX_EPOCH);
        assert_eq!(formatted, "1970-01-01T00:00:00.000000000+00:00");
        assert_eq!(
            format_rfc3339_secs_z(OffsetDateTime::UNIX_EPOCH),
            "1970-01-01T00:00:00Z"
        );
    }
}
