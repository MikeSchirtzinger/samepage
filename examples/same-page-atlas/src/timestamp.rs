//! Small UTC timestamp helper shared by board authorship and cement receipts.
//!
//! The Atlas already avoids pulling a date/time crate into example hosts. This
//! keeps that property while still refusing a clock that predates Unix time
//! instead of silently manufacturing a timestamp.

/// Current UTC time as an RFC 3339 string.
pub fn now_iso() -> Result<String, String> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("system clock is before Unix epoch: {error}"))?
        .as_secs();
    Ok(unix_seconds_iso(secs))
}

fn unix_seconds_iso(secs: u64) -> String {
    // Howard Hinnant's civil_from_days.
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3_600,
        (tod % 3_600) / 60,
        tod % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_epoch_is_rfc3339() {
        assert_eq!(unix_seconds_iso(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn leap_days_are_preserved() {
        assert_eq!(unix_seconds_iso(1_709_251_200), "2024-03-01T00:00:00Z");
    }
}
