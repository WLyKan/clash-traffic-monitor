//! Local-time bucketing. Epoch bucket keys keep repeated DST minutes distinct;
//! hour/day keys deliberately retain the Python monitor's wall-clock grouping.
use crate::{Result, StoreError};
use chrono::{DateTime, Local, LocalResult, NaiveDate, TimeZone, Timelike};

pub fn local_timestamp(timestamp: f64) -> Result<DateTime<Local>> {
    if !timestamp.is_finite() {
        return Err(StoreError::Invalid("时间戳无效".into()));
    }
    let seconds = timestamp.floor();
    if seconds < i64::MIN as f64 || seconds > i64::MAX as f64 {
        return Err(StoreError::Invalid("时间戳超出范围".into()));
    }
    let nanos = ((timestamp - seconds) * 1_000_000_000.0)
        .round()
        .clamp(0.0, 999_999_999.0) as u32;
    Local
        .timestamp_opt(seconds as i64, nanos)
        .single()
        .ok_or_else(|| StoreError::Invalid("时间戳超出本地日期范围".into()))
}

pub fn minute_bucket(timestamp: f64) -> Result<i64> {
    let local = local_timestamp(timestamp)?;
    Ok(timestamp.floor() as i64 - local.second() as i64 - (local.minute() % 5) as i64 * 60)
}

pub fn minute_buckets(day: NaiveDate, hour: u32, minute: u32) -> Result<Vec<i64>> {
    let local = day
        .and_hms_opt(hour, minute, 0)
        .ok_or_else(|| StoreError::Invalid("小时或分钟无效".into()))?;
    let mut buckets = match Local.from_local_datetime(&local) {
        LocalResult::None => vec![],
        LocalResult::Single(value) => vec![value.timestamp()],
        LocalResult::Ambiguous(first, second) => vec![first.timestamp(), second.timestamp()],
    };
    buckets.sort_unstable();
    buckets.dedup();
    Ok(buckets)
}

pub fn local_day_start(day: NaiveDate) -> Result<f64> {
    // Some zones have historically moved their clocks at midnight. The first
    // valid instant of that local date bounds its observations in that case.
    for minute in 0..1440 {
        let local = day.and_hms_opt(minute / 60, minute % 60, 0).unwrap();
        if let Some(value) = Local.from_local_datetime(&local).earliest() {
            return Ok(value.timestamp() as f64);
        }
    }
    Err(StoreError::Invalid("该本地日期不存在".into()))
}

pub fn minute_intervals(mut start: f64, end: f64) -> Result<Vec<(i64, f64)>> {
    if !start.is_finite() || !end.is_finite() {
        return Err(StoreError::Invalid("时间区间无效".into()));
    }
    let mut result = vec![];
    while start < end {
        let bucket = minute_bucket(start)?;
        let boundary = end.min((bucket + 300) as f64);
        if boundary <= start {
            return Err(StoreError::Invalid("五分钟边界无效".into()));
        }
        result.push((bucket, boundary - start));
        start = boundary;
    }
    Ok(result)
}

pub fn hour_intervals(mut start: f64, end: f64) -> Result<Vec<(String, u32, f64)>> {
    if !start.is_finite() || !end.is_finite() {
        return Err(StoreError::Invalid("时间区间无效".into()));
    }
    let mut result = vec![];
    while start < end {
        let local = local_timestamp(start)?;
        let elapsed = local.minute() as f64 * 60.0
            + local.second() as f64
            + local.nanosecond() as f64 / 1_000_000_000.0;
        let boundary = end.min(start + 3600.0 - elapsed);
        if boundary <= start {
            return Err(StoreError::Invalid("小时边界无效".into()));
        }
        result.push((
            local.date_naive().to_string(),
            local.hour(),
            boundary - start,
        ));
        start = boundary;
    }
    Ok(result)
}
