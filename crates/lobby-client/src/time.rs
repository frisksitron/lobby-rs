//! Message times and day labels in the person's local time zone.

use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, TimeZone as _};
use lobby_protocol::UnixMs;

use crate::copy;

fn local(at: UnixMs) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(at.0).earliest()
}

pub fn local_time(at: UnixMs) -> String {
    local(at).map_or_else(String::new, |t| t.format("%H:%M").to_string())
}

pub fn local_date_time(at: UnixMs) -> String {
    local(at).map_or_else(String::new, |t| t.format("%a %-d %b, %H:%M").to_string())
}

/// The day alone, for a Member Card: "12 Mar 2026".
pub fn local_date(at: UnixMs) -> String {
    local(at).map_or_else(String::new, |t| t.format("%-d %b %Y").to_string())
}

pub fn local_day(at: UnixMs) -> Option<NaiveDate> {
    local(at).map(|t| t.date_naive())
}

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// Where a time zone has no local midnight tomorrow (a clock change at 00:00), the wait for it.
const NO_MIDNIGHT_WAIT: Duration = Duration::from_secs(60 * 60);

/// The time to the next local midnight, when [`today`] turns.
pub fn until_tomorrow() -> Duration {
    let now = Local::now();
    now.date_naive()
        .succ_opt()
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .and_then(|midnight| midnight.and_local_timezone(Local).earliest())
        .and_then(|midnight| (midnight - now).to_std().ok())
        .unwrap_or(NO_MIDNIGHT_WAIT)
}

pub fn day_label(day: NaiveDate, today: NaiveDate) -> String {
    use chrono::Datelike as _;
    if day == today {
        copy::TODAY.to_owned()
    } else if today.pred_opt() == Some(day) {
        copy::YESTERDAY.to_owned()
    } else if day.year() == today.year() {
        day.format("%A, %-d %B").to_string()
    } else {
        day.format("%A, %-d %B %Y").to_string()
    }
}

pub fn full_time(at: UnixMs) -> String {
    local(at).map_or_else(String::new, |t| {
        t.format("%A, %-d %B %Y at %H:%M").to_string()
    })
}

pub fn time_ago(at: UnixMs, now: UnixMs) -> String {
    copy::ago((now.0 - at.0).max(0) / 1000)
}
