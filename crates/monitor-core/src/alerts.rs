use crate::{time::minute_bucket, AppDeltas};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use unicode_casefold::UnicodeCaseFold;

pub const WINDOW_SECONDS: f64 = 900.0;
pub const MAX_THRESHOLD_MIB: u64 = 1_000_000;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertSettings {
    pub enabled: bool,
    pub threshold_mib: u64,
}
impl Default for AlertSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_mib: 256,
        }
    }
}
impl AlertSettings {
    pub fn validate(&self) -> Result<(), String> {
        if (1..=MAX_THRESHOLD_MIB).contains(&self.threshold_mib) {
            Ok(())
        } else {
            Err("阈值超出范围".into())
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpikeAlert {
    pub start: f64,
    pub end: f64,
    pub total: i64,
    pub contributors: Vec<String>,
    pub peak_at: f64,
}
pub struct SpikeDetector {
    pub settings: AlertSettings,
    pub last_alert_at: Option<f64>,
    pub total: i64,
    start: Option<f64>,
    samples: VecDeque<(f64, f64, AppDeltas, i64)>,
}
impl SpikeDetector {
    pub fn new(settings: AlertSettings) -> Self {
        Self {
            settings,
            last_alert_at: None,
            total: 0,
            start: None,
            samples: VecDeque::new(),
        }
    }
    pub fn reset(&mut self) {
        self.samples.clear();
        self.start = None;
        self.total = 0;
    }
    pub fn add(&mut self, start: f64, end: f64, apps: &AppDeltas) -> Option<SpikeAlert> {
        if !self.settings.enabled {
            return None;
        }
        self.start.get_or_insert(start);
        let amount = apps.values().map(|(up, down)| up + down).sum::<i64>();
        if amount != 0 {
            self.samples.push_back((start, end, apps.clone(), amount));
            self.total += amount;
        }
        let cutoff = end - WINDOW_SECONDS;
        while self.samples.front().is_some_and(|sample| sample.0 < cutoff) {
            self.total -= self.samples.pop_front().unwrap().3;
        }
        if self.start.unwrap() > cutoff
            || self.total < (self.settings.threshold_mib * 1024 * 1024) as i64
        {
            return None;
        }
        if self
            .last_alert_at
            .is_some_and(|last| (0.0..WINDOW_SECONDS).contains(&(end - last)))
        {
            return None;
        }
        self.last_alert_at = Some(end);
        let mut totals: BTreeMap<String, (String, i64)> = BTreeMap::new();
        let mut buckets: BTreeMap<i64, i64> = BTreeMap::new();
        for (_, timestamp, deltas, volume) in &self.samples {
            *buckets.entry(minute_bucket(*timestamp)).or_default() += volume;
            for (name, (up, down)) in deltas {
                let entry = totals
                    .entry(name.as_str().case_fold().collect::<String>())
                    .or_insert((name.clone(), 0));
                entry.1 += up + down;
            }
        }
        let mut ranked = totals.into_values().collect::<Vec<_>>();
        ranked.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.as_str().case_fold().cmp(b.0.as_str().case_fold()))
        });
        let peak = buckets
            .into_iter()
            .max_by_key(|(bucket, volume)| (*volume, *bucket))?
            .0;
        Some(SpikeAlert {
            start: cutoff,
            end,
            total: self.total,
            contributors: ranked.into_iter().take(3).map(|(name, _)| name).collect(),
            peak_at: peak as f64,
        })
    }
}
