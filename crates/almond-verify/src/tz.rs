//! Meeting time zones: IANA names (`Meeting::tz`, job param `tz`) to the fixed UTC offset in
//! effect at the meeting, which is what the date normalizers work with.

use almond_types::Meeting;
use chrono::{Offset, TimeZone};
use serde_json::Value;

/// Offset from UTC in minutes of IANA zone `tz` at `at_ms` (Unix ms). `None` for unknown zones.
pub fn iana_offset_min(tz: &str, at_ms: i64) -> Option<i32> {
    let zone: chrono_tz::Tz = tz.trim().parse().ok()?;
    let t = zone.timestamp_millis_opt(at_ms).single()?;
    Some(t.offset().fix().local_minus_utc() / 60)
}

/// Effective offset for a job: `params.tz_offset_min`, else `params.tz` (IANA), else
/// `meeting.tz` (IANA), each evaluated at the meeting's start. `None` = use the local zone.
pub fn offset_for(params: &Value, meeting: &Meeting) -> Option<i32> {
    if let Some(m) = params.get("tz_offset_min").and_then(Value::as_i64) {
        return Some(m as i32);
    }
    params
        .get("tz")
        .and_then(Value::as_str)
        .and_then(|z| iana_offset_min(z, meeting.started_at))
        .or_else(|| {
            meeting
                .tz
                .as_deref()
                .and_then(|z| iana_offset_min(z, meeting.started_at))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::mock::fixtures;
    use serde_json::json;

    #[test]
    fn offsets_from_params_and_meeting() {
        // 2025-11-09 00:00 UTC
        let mut m = fixtures::meeting(1_762_646_400_000);
        m.tz = None;
        assert_eq!(offset_for(&Value::Null, &m), None);
        m.tz = Some("Asia/Ho_Chi_Minh".into());
        assert_eq!(offset_for(&Value::Null, &m), Some(420));
        assert_eq!(offset_for(&json!({"tz": "Europe/Berlin"}), &m), Some(60));
        assert_eq!(offset_for(&json!({"tz_offset_min": -300}), &m), Some(-300));
        // Unknown param zone falls back to the meeting's.
        assert_eq!(offset_for(&json!({"tz": "Mars/Base"}), &m), Some(420));
        // DST: Berlin in July is UTC+2.
        assert_eq!(
            iana_offset_min("Europe/Berlin", 1_751_328_000_000),
            Some(120)
        );
    }
}
