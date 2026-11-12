use monitor_core::{AlertSettings, AppDeltas, SamplingState, Snapshot, SpikeDetector};

fn snapshot(time: f64, generation: u64, up: i64, down: i64) -> Snapshot {
    Snapshot {
        sampled_at: time,
        generation,
        upload_total: up,
        download_total: down,
        ..Default::default()
    }
}

#[test]
fn checkpoint_only_advances_after_commit_and_failures_create_gaps() {
    let mut state = SamplingState::new(Some(99.0), 1.0);
    let baseline = snapshot(100.0, 1, 10, 10);
    let first = state.prepare(&baseline);
    assert_eq!(
        (first.gap_from, first.covered_from, first.segment),
        (Some(99.0), None, Some(100.0))
    );
    assert_eq!(state.last_sample, Some(99.0));
    state.commit(baseline, &first);
    let continuous = state.prepare(&snapshot(101.0, 1, 15, 17));
    assert_eq!(
        (
            continuous.covered_from,
            continuous.segment,
            continuous.unmatched
        ),
        (Some(100.0), Some(100.0), (5, 7))
    );
    // A failed transaction is not committed. Disconnect keeps the saved checkpoint.
    state.disconnected();
    let restart = snapshot(102.0, 1, 20, 20);
    let gap = state.prepare(&restart);
    assert_eq!(
        (gap.gap_from, gap.covered_from, gap.unmatched),
        (Some(100.0), None, (0, 0))
    );
    state.commit(restart, &gap);
    assert_eq!(state.last_sample, Some(102.0));
}

#[test]
fn sleep_reset_generation_and_nonpositive_time_rebaseline() {
    for next in [
        snapshot(104.0, 1, 20, 20),
        snapshot(101.0, 2, 20, 20),
        snapshot(101.0, 1, 9, 20),
        snapshot(101.0, 1, 20, 9),
        snapshot(100.0, 1, 20, 20),
        snapshot(99.0, 1, 20, 20),
    ] {
        let mut state = SamplingState::new(None, 1.0);
        let old = snapshot(100.0, 1, 10, 10);
        let record = state.prepare(&old);
        state.commit(old, &record);
        let record = state.prepare(&next);
        assert_eq!(record.covered_from, None);
        assert_eq!(record.gap_from, (next.sampled_at >= 100.0).then_some(100.0));
        assert_eq!(record.unmatched, (0, 0));
    }
    let mut state = SamplingState::new(None, 1.0);
    let old = snapshot(100.0, 1, 10, 10);
    let record = state.prepare(&old);
    state.commit(old, &record);
    assert_eq!(
        state.prepare(&snapshot(103.5, 1, 10, 10)).covered_from,
        Some(100.0)
    );
}

#[test]
fn clock_rollback_commits_new_baseline_and_immediately_resumes() {
    for previous_in_memory in [false, true] {
        let mut state = SamplingState::new(Some(100.0), 1.0);
        if previous_in_memory {
            let previous = snapshot(100.0, 1, 10, 10);
            let record = state.prepare(&previous);
            state.commit(previous, &record);
        }
        let backwards = snapshot(99.0, 1, 20, 20);
        let baseline = state.prepare(&backwards);
        // A reverse interval is empty, rather than a future gap rejected by
        // Store::record. The checkpoint must be allowed to move backwards.
        assert_eq!(baseline.gap_from, None);
        assert_eq!(baseline.covered_from, None);
        assert_eq!(baseline.segment, Some(99.0));
        state.commit(backwards, &baseline);
        assert_eq!(state.last_sample, Some(99.0));
        let next = state.prepare(&snapshot(99.5, 1, 22, 23));
        assert_eq!(next.covered_from, Some(99.0));
        assert_eq!(next.segment, Some(99.0));
        assert_eq!(next.unmatched, (2, 3));
    }
}

#[test]
fn partial_intervals_reset_and_cooldown_match_legacy() {
    let apps = AppDeltas::from([("app.exe".into(), (1024 * 1024, 0))]);
    let mut detector = SpikeDetector::new(AlertSettings {
        enabled: true,
        threshold_mib: 1,
    });
    assert!(detector.add(0.0, 2.0, &apps).is_none());
    assert!(detector.add(899.0, 901.0, &AppDeltas::new()).is_none());
    assert_eq!(detector.total, 0);
    detector.reset();
    assert!(detector.add(1000.0, 1001.0, &apps).is_none());
    assert!(detector.add(1899.0, 1900.0, &AppDeltas::new()).is_some());
    detector.reset();
    assert_eq!(detector.last_alert_at, Some(1900.0));
    assert!(detector.add(1900.0, 1901.0, &apps).is_none());
    assert!(detector.add(2798.0, 2799.0, &AppDeltas::new()).is_none());
}

#[test]
fn contributors_use_full_unicode_casefold_and_top_three() {
    let mut detector = SpikeDetector::new(AlertSettings {
        enabled: true,
        threshold_mib: 1,
    });
    let apps = AppDeltas::from([
        ("Straße.exe".into(), (500000, 0)),
        ("STRASSE.EXE".into(), (600000, 0)),
        ("b.exe".into(), (4, 0)),
        ("c.exe".into(), (3, 0)),
        ("d.exe".into(), (2, 0)),
    ]);
    assert!(detector.add(0.0, 1.0, &apps).is_none());
    let alert = detector.add(899.0, 900.0, &AppDeltas::new()).unwrap();
    assert_eq!(alert.contributors, vec!["STRASSE.EXE", "b.exe", "c.exe"]);
}

#[test]
fn settings_are_validated_without_truncation() {
    assert!(AlertSettings {
        enabled: false,
        threshold_mib: 0
    }
    .validate()
    .is_err());
    assert!(AlertSettings {
        enabled: true,
        threshold_mib: 1_000_001
    }
    .validate()
    .is_err());
    assert!(AlertSettings {
        enabled: true,
        threshold_mib: 1_000_000
    }
    .validate()
    .is_ok());
}
