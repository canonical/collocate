use collocate_controller::autoscaler::{Decision, MetricReading, Scaler};
use collocate_core::policy::{Autoscale, MetricKind};

fn policy(min: u32, max: u32, metrics: &str) -> Autoscale {
    serde_json::from_str(&format!(
        r#"{{"min":{min},"max":{max},"metrics":{metrics},"up":{{"stabilization":"30s","max_step":4}},"down":{{"stabilization":"5m","max_step":1}},"tolerance":"10%"}}"#
    ))
    .unwrap()
}

fn cpu(target: u32) -> String {
    format!(r#"[{{"type":"cpu","target":{target}}}]"#)
}

fn read(kind: MetricKind, value: f64) -> Vec<MetricReading> {
    vec![MetricReading { kind, value }]
}

fn to(d: &Decision) -> Option<u32> {
    match d {
        Decision::Scale { to } => Some(*to),
        Decision::Hold(_) => None,
    }
}

#[test]
fn scales_up_proportionally() {
    let mut s = Scaler::new(policy(1, 10, &cpu(65)));
    assert_eq!(to(&s.decide(0, 2, &read(MetricKind::Cpu, 130.0))), Some(4));
}

#[test]
fn holds_within_tolerance() {
    let mut s = Scaler::new(policy(1, 10, &cpu(65)));
    assert!(matches!(s.decide(0, 3, &read(MetricKind::Cpu, 68.0)), Decision::Hold(_)));
    assert!(matches!(s.decide(10, 3, &read(MetricKind::Cpu, 60.0)), Decision::Hold(_)));
}

#[test]
fn scale_up_is_capped_by_max_step_and_max() {
    let mut s = Scaler::new(policy(1, 10, &cpu(50)));
    assert_eq!(to(&s.decide(0, 2, &read(MetricKind::Cpu, 500.0))), Some(6));
    let mut s = Scaler::new(policy(1, 5, &cpu(50)));
    assert_eq!(to(&s.decide(0, 4, &read(MetricKind::Cpu, 500.0))), Some(5));
}

#[test]
fn scale_down_waits_for_the_stabilization_window_and_steps_slowly() {
    let mut s = Scaler::new(policy(1, 10, &cpu(65)));
    assert!(matches!(s.decide(0, 4, &read(MetricKind::Cpu, 65.0)), Decision::Hold(_)));
    assert!(matches!(s.decide(100, 4, &read(MetricKind::Cpu, 10.0)), Decision::Hold(_)));
    assert_eq!(to(&s.decide(400, 4, &read(MetricKind::Cpu, 10.0))), Some(3));
}

#[test]
fn scale_up_requires_the_load_to_persist_through_the_up_window() {
    let mut s = Scaler::new(policy(1, 10, &cpu(65)));
    assert_eq!(to(&s.decide(0, 2, &read(MetricKind::Cpu, 65.0))), None);
    assert_eq!(to(&s.decide(10, 2, &read(MetricKind::Cpu, 130.0))), None);
    assert_eq!(to(&s.decide(20, 2, &read(MetricKind::Cpu, 130.0))), None);
    assert_eq!(to(&s.decide(45, 2, &read(MetricKind::Cpu, 130.0))), Some(4));
}

#[test]
fn oscillating_load_does_not_flap() {
    let mut s = Scaler::new(policy(1, 10, &cpu(65)));
    let mut current = 3;
    let mut changes = 0;
    for step in 0..30u64 {
        assert_eq!(to(&s.decide(step * 10, current, &read(MetricKind::Cpu, 65.0))), None);
    }
    for step in 30..390u64 {
        let load = if step % 2 == 0 { 200.0 } else { 20.0 };
        if let Some(n) = to(&s.decide(step * 10, current, &read(MetricKind::Cpu, load))) {
            current = n;
            changes += 1;
        }
    }
    assert_eq!(changes, 0);
}

#[test]
fn bounds_are_enforced_immediately() {
    let mut s = Scaler::new(policy(3, 6, &cpu(65)));
    assert_eq!(to(&s.decide(0, 1, &read(MetricKind::Cpu, 65.0))), Some(3));
    assert_eq!(to(&s.decide(1, 9, &read(MetricKind::Cpu, 65.0))), Some(6));
    assert_eq!(to(&s.decide(2, 0, &[])), Some(3));
}

#[test]
fn the_largest_recommendation_across_metrics_wins() {
    let m = r#"[{"type":"cpu","target":50},{"type":"connections","target":100}]"#;
    let mut s = Scaler::new(policy(1, 20, m));
    let readings =
        vec![MetricReading { kind: MetricKind::Cpu, value: 60.0 }, MetricReading { kind: MetricKind::Connections, value: 300.0 }];
    assert_eq!(to(&s.decide(0, 2, &readings)), Some(6));
}

#[test]
fn missing_metrics_are_ignored_and_all_missing_holds() {
    let m = r#"[{"type":"cpu","target":50},{"type":"memory","target":50}]"#;
    let mut s = Scaler::new(policy(1, 20, m));
    assert_eq!(to(&s.decide(0, 2, &read(MetricKind::Memory, 100.0))), Some(4));
    let mut s = Scaler::new(policy(1, 20, m));
    assert!(matches!(s.decide(0, 2, &[]), Decision::Hold(_)));
}

#[test]
fn non_finite_readings_are_ignored() {
    let mut s = Scaler::new(policy(1, 10, &cpu(65)));
    assert!(matches!(s.decide(0, 2, &read(MetricKind::Cpu, f64::NAN)), Decision::Hold(_)));
    assert!(matches!(s.decide(1, 2, &read(MetricKind::Cpu, f64::INFINITY)), Decision::Hold(_)));
}
