use chrono::{DateTime, Local, Timelike, Utc};

/// Unix timestamp with subsecond precision, interpreted using the system timezone.
pub fn local_at(timestamp: f64) -> DateTime<Local> {
    let seconds = timestamp.floor() as i64;
    let nanos = ((timestamp - timestamp.floor()) * 1_000_000_000.0)
        .round()
        .clamp(0.0, 999_999_999.0) as u32;
    DateTime::<Utc>::from_timestamp(seconds, nanos)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        .with_timezone(&Local)
}

/// Preserve the legacy wall-clock bucket including each DST-fold instance.
pub fn minute_bucket(timestamp: f64) -> i64 {
    let local = local_at(timestamp);
    timestamp.floor() as i64 - local.second() as i64 - (local.minute() % 5) as i64 * 60
}

pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}
