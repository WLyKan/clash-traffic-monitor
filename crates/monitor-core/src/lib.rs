//! Framework-independent sampling and attribution. No SQLite or window dependency.
pub mod alerts;
pub mod sampling;
pub mod source;
pub mod time;
pub use alerts::{AlertSettings, SpikeAlert, SpikeDetector};
pub use sampling::{traffic_delta, SampleRecord, SamplingState};
use serde::{Deserialize, Serialize};
pub use source::{target_name, MihomoSource};
use std::collections::BTreeMap;

pub type BytePair = (i64, i64);
pub type AppDeltas = BTreeMap<String, BytePair>;
pub type TargetDeltas = BTreeMap<(String, String), BytePair>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    Proxy,
    Direct,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub started_at: Option<f64>,
    pub app: Option<String>,
    pub route: Route,
    pub uploaded: i64,
    pub downloaded: i64,
    #[serde(default = "unknown_target")]
    pub target: String,
}
fn unknown_target() -> String {
    "目标未知".into()
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub sampled_at: f64,
    pub completed_at: Option<f64>,
    pub upload_total: i64,
    pub download_total: i64,
    pub generation: u64,
    pub connections: Vec<Connection>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Delta {
    pub apps: AppDeltas,
    pub targets: TargetDeltas,
    pub unknown_route: BytePair,
    pub unmatched: BytePair,
}
