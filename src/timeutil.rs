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

pub fn format_rfc3339(dt: OffsetDateTime) -> String {
    dt.to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .expect("UTC RFC3339 formatting cannot fail")
}

pub fn parse_rfc3339(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339)
        .ok()
        .map(|dt| dt.to_offset(time::UtcOffset::UTC))
}

pub fn parse_rfc2822(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc2822)
        .ok()
        .map(|dt| dt.to_offset(time::UtcOffset::UTC))
}

pub fn format_rfc2822(dt: OffsetDateTime) -> String {
    dt.to_offset(time::UtcOffset::UTC)
        .format(&Rfc2822)
        .expect("UTC RFC2822 formatting cannot fail")
}

pub fn from_unix_secs(secs: i64) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(secs).ok()
}
