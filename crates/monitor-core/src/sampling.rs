use crate::{AppDeltas, Delta, Snapshot, TargetDeltas};
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SampleRecord {
    pub sampled_at: f64,
    pub apps: AppDeltas,
    pub targets: TargetDeltas,
    pub unattributed: (i64, i64),
    pub unmatched: (i64, i64),
    pub covered_from: Option<f64>,
    pub gap_from: Option<f64>,
    pub segment: Option<f64>,
}

pub fn traffic_delta(previous: &Snapshot, current: &Snapshot) -> Delta {
    let before: HashMap<_, _> = previous
        .connections
        .iter()
        .map(|connection| (&connection.id, connection))
        .collect();
    let previous_end = previous
        .completed_at
        .filter(|value| *value != 0.0)
        .unwrap_or(previous.sampled_at);
    let mut result = Delta::default();
    let (mut observed_up, mut observed_down) = (0i64, 0i64);
    for connection in &current.connections {
        let (up, down) = match before.get(&connection.id) {
            Some(older) if older.started_at == connection.started_at => (
                (connection.uploaded - older.uploaded).max(0),
                (connection.downloaded - older.downloaded).max(0),
            ),
            _ if connection
                .started_at
                .is_some_and(|start| start >= previous_end) =>
            {
                (connection.uploaded, connection.downloaded)
            }
            _ => continue,
        };
        observed_up += up;
        observed_down += down;
        match connection.route {
            crate::Route::Proxy if up != 0 || down != 0 => {
                let name = connection.app.clone().unwrap_or_default();
                let app = result.apps.entry(name.clone()).or_default();
                app.0 += up;
                app.1 += down;
                let target = result
                    .targets
                    .entry((name, connection.target.clone()))
                    .or_default();
                target.0 += up;
                target.1 += down;
            }
            crate::Route::Unknown => {
                result.unknown_route.0 += up;
                result.unknown_route.1 += down;
            }
            _ => {}
        }
    }
    result.unmatched = (
        current.upload_total - previous.upload_total - observed_up,
        current.download_total - previous.download_total - observed_down,
    );
    result
}

/// A poll is prepared without mutating committed state. Call `commit` only after
/// the store transaction succeeds; failure must reset the in-memory baseline.
pub struct SamplingState {
    pub previous: Option<Snapshot>,
    pub last_sample: Option<f64>,
    pub segment: Option<f64>,
    pub interval: f64,
}
impl SamplingState {
    pub fn new(last_sample: Option<f64>, interval: f64) -> Self {
        Self {
            previous: None,
            last_sample,
            segment: None,
            interval,
        }
    }
    pub fn prepare(&self, current: &Snapshot) -> SampleRecord {
        let continuous = self.previous.as_ref().is_some_and(|previous| {
            let elapsed = current.sampled_at - previous.sampled_at;
            elapsed > 0.0
                && elapsed <= self.interval * 3.5
                && current.generation == previous.generation
                && current.upload_total >= previous.upload_total
                && current.download_total >= previous.download_total
        });
        if continuous {
            let previous = self.previous.as_ref().unwrap();
            let delta = traffic_delta(previous, current);
            SampleRecord {
                sampled_at: current.sampled_at,
                apps: delta.apps,
                targets: delta.targets,
                unattributed: delta.unknown_route,
                unmatched: delta.unmatched,
                covered_from: Some(previous.sampled_at),
                segment: self.segment,
                gap_from: None,
            }
        } else {
            SampleRecord {
                sampled_at: current.sampled_at,
                // A backwards clock makes the old gap interval empty. Do not
                // send a future gap start to the store: the new baseline and
                // checkpoint must still be committed immediately.
                gap_from: self.last_sample.filter(|last| *last <= current.sampled_at),
                segment: Some(current.sampled_at),
                ..Default::default()
            }
        }
    }
    pub fn commit(&mut self, current: Snapshot, record: &SampleRecord) {
        if record.covered_from.is_none() {
            self.segment = Some(current.sampled_at);
        }
        self.last_sample = Some(current.sampled_at);
        self.previous = Some(current);
    }
    pub fn disconnected(&mut self) {
        self.previous = None;
    }
}
