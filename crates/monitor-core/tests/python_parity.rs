use monitor_core::{target_name, traffic_delta, AlertSettings, AppDeltas, Snapshot, SpikeDetector};
use serde_json::{json, Value};

#[test]
fn frozen_original_python_core_contract() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/rust-core/python-core.json"
    ))
    .unwrap();
    for case in fixture["deltas"].as_array().unwrap() {
        let previous: Snapshot = serde_json::from_value(case["previous"].clone()).unwrap();
        let current: Snapshot = serde_json::from_value(case["current"].clone()).unwrap();
        let result = traffic_delta(&previous, &current);
        let targets: Vec<_> = result
            .targets
            .iter()
            .map(|((app, target), (up, down))| json!([app, target, up, down]))
            .collect();
        assert_eq!(
            json!({"apps":result.apps,"targets":targets,"unknown_route":result.unknown_route,"unmatched":result.unmatched}),
            case["expected"],
            "{}",
            case["name"]
        );
    }
    for case in fixture["targets"].as_array().unwrap() {
        assert_eq!(
            target_name(&case["metadata"]),
            case["expected"].as_str().unwrap(),
            "{}",
            case["metadata"]
        );
    }
    let alert_fixture = &fixture["alerts"];
    let start = alert_fixture["start"].as_f64().unwrap();
    let mut detector = SpikeDetector::new(AlertSettings {
        enabled: true,
        threshold_mib: 1,
    });
    let mut alerts = Vec::new();
    for second in 0..alert_fixture["seconds"].as_u64().unwrap() {
        let apps: AppDeltas = alert_fixture["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["second"].as_u64() == Some(second))
            .map(|event| serde_json::from_value(event["apps"].clone()).unwrap())
            .unwrap_or_default();
        if let Some(alert) = detector.add(start + second as f64, start + second as f64 + 1.0, &apps)
        {
            alerts.push(alert);
        }
    }
    let expected: Vec<monitor_core::SpikeAlert> =
        serde_json::from_value(alert_fixture["expected"].clone()).unwrap();
    assert_eq!(alerts, expected);
}
