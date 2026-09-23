use collocate_core::policy::{Autoscale, MetricKind, OnFailure, Strategy, UpdatePolicy};
use collocate_core::size::parse_duration_secs;

#[test]
fn durations_accept_units() {
    assert_eq!(parse_duration_secs("30s").unwrap(), 30);
    assert_eq!(parse_duration_secs("5m").unwrap(), 300);
    assert_eq!(parse_duration_secs("2h").unwrap(), 7200);
    assert_eq!(parse_duration_secs("45").unwrap(), 45);
    assert_eq!(parse_duration_secs("1d").unwrap(), 86400);
}

#[test]
fn durations_reject_garbage() {
    for bad in ["", "s", "1x", "-5s", "1.5m", "abc"] {
        assert!(parse_duration_secs(bad).is_err(), "{bad}");
    }
}

#[test]
fn autoscale_defaults_and_shape() {
    let a: Autoscale = serde_json::from_str(
        r#"{"min":2,"max":10,"metrics":[{"type":"cpu","target":65},{"type":"connections","target":200}],
            "up":{"stabilization":"30s","max_step":4},"down":{"stabilization":"5m","max_step":1},"tolerance":"10%"}"#,
    )
    .unwrap();
    assert_eq!((a.min, a.max), (2, 10));
    assert_eq!(a.metrics[0].kind, MetricKind::Cpu);
    assert_eq!(a.metrics[1].kind, MetricKind::Connections);
    assert_eq!(a.up.stabilization_secs, 30);
    assert_eq!(a.down.stabilization_secs, 300);
    assert_eq!(a.up.max_step, 4);
    assert!((a.tolerance_pct - 10.0).abs() < f64::EPSILON);
}

#[test]
fn autoscale_minimal_uses_sensible_defaults() {
    let a: Autoscale = serde_json::from_str(r#"{"min":1,"max":3,"metrics":[{"type":"memory","target":70}]}"#).unwrap();
    assert!(a.up.stabilization_secs < a.down.stabilization_secs);
    assert!(a.tolerance_pct > 0.0);
    assert!(a.up.max_step >= 1 && a.down.max_step >= 1);
}

#[test]
fn autoscale_rejects_unknown_metric_kinds() {
    assert!(serde_json::from_str::<Autoscale>(r#"{"min":1,"max":3,"metrics":[{"type":"disk","target":1}]}"#).is_err());
}

#[test]
fn update_policy_defaults_to_safe_rolling() {
    let u: UpdatePolicy = serde_json::from_str("{}").unwrap();
    assert_eq!(u.strategy, Strategy::Rolling);
    assert_eq!(u.max_surge, 1);
    assert_eq!(u.max_unavailable, 0);
    assert_eq!(u.on_failure, OnFailure::Rollback);
    assert!(u.progress_deadline_secs > 0);
}

#[test]
fn update_policy_parses_durations_and_strategy() {
    let u: UpdatePolicy = serde_json::from_str(
        r#"{"strategy":"recreate","max_surge":2,"max_unavailable":1,"min_ready":"10s","delay":"5s","progress_deadline":"10m","on_failure":"pause"}"#,
    )
    .unwrap();
    assert_eq!(u.strategy, Strategy::Recreate);
    assert_eq!((u.min_ready_secs, u.delay_secs, u.progress_deadline_secs), (10, 5, 600));
    assert_eq!(u.on_failure, OnFailure::Pause);
}
