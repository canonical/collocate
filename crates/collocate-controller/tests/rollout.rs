use collocate_controller::rollout::{next_step, Action, Health, Lifecycle, Replica, State};
use collocate_core::policy::{OnFailure, Strategy, UpdatePolicy};

fn policy() -> UpdatePolicy {
    UpdatePolicy { min_ready_secs: 10, delay_secs: 0, ..UpdatePolicy::default() }
}

fn rep(index: u32, rev: &str, ready: bool) -> Replica {
    Replica {
        index,
        revision: rev.into(),
        health: if ready { Health::Healthy { for_secs: 60 } } else { Health::Starting },
        lifecycle: Lifecycle::Live,
        failed: false,
        connections: 0,
    }
}

fn state(desired: u32, replicas: Vec<Replica>) -> State {
    State {
        desired,
        target: "new".into(),
        previous: Some("old".into()),
        policy: policy(),
        drain_secs: 30,
        replicas,
        elapsed_secs: 0,
        since_last_action_secs: 100,
        paused: false,
    }
}

fn olds(n: u32) -> Vec<Replica> {
    (0..n).map(|i| rep(i, "old", true)).collect()
}

#[test]
fn steady_state_is_complete() {
    let s = state(3, (0..3).map(|i| rep(i, "new", true)).collect());
    assert_eq!(next_step(&s), vec![Action::Complete]);
}

#[test]
fn rollout_starts_by_surging_one_new_replica() {
    let s = state(3, olds(3));
    assert_eq!(next_step(&s), vec![Action::Start { index: 3, revision: "new".into() }]);
}

#[test]
fn nothing_is_drained_until_the_new_replica_is_ready() {
    let mut r = olds(3);
    r.push(rep(3, "new", false));
    assert_eq!(next_step(&state(3, r)), vec![]);
    let mut r = olds(3);
    r.push(Replica { health: Health::Healthy { for_secs: 3 }, ..rep(3, "new", true) });
    assert_eq!(next_step(&state(3, r)), vec![]);
}

#[test]
fn a_ready_new_replica_lets_one_old_replica_drain() {
    let mut r = olds(3);
    r.push(rep(3, "new", true));
    assert_eq!(next_step(&state(3, r)), vec![Action::Drain { index: 2 }]);
}

#[test]
fn a_freed_slot_starts_the_next_new_replica() {
    let mut r = olds(2);
    r.push(rep(3, "new", true));
    assert_eq!(next_step(&state(3, r)), vec![Action::Start { index: 2, revision: "new".into() }]);
}

#[test]
fn indexes_of_draining_replicas_are_not_reused() {
    let mut r = olds(2);
    r.push(Replica { lifecycle: Lifecycle::Draining { for_secs: 1 }, ..rep(2, "old", true) });
    r.push(rep(3, "new", true));
    assert_eq!(next_step(&state(3, r)), vec![]);
}

#[test]
fn draining_replicas_are_removed_once_drained() {
    let mut r = vec![rep(0, "new", true), rep(1, "new", true), rep(2, "new", true)];
    r.push(Replica { lifecycle: Lifecycle::Draining { for_secs: 30 }, ..rep(3, "old", true) });
    r.push(Replica { lifecycle: Lifecycle::Draining { for_secs: 5 }, ..rep(4, "old", true) });
    assert_eq!(next_step(&state(3, r)), vec![Action::Remove { index: 3 }]);
}

#[test]
fn max_unavailable_permits_draining_before_replacements_are_ready() {
    let mut p = policy();
    p.max_unavailable = 1;
    p.max_surge = 0;
    let mut s = state(3, olds(3));
    s.policy = p;
    assert_eq!(next_step(&s), vec![Action::Drain { index: 2 }]);
}

#[test]
fn unhealthy_old_replicas_are_replaced_first() {
    let mut r = olds(3);
    r[0].health = Health::Unhealthy;
    r.push(rep(3, "new", true));
    let mut s = state(3, r);
    s.policy.max_surge = 1;
    assert_eq!(next_step(&s), vec![Action::Drain { index: 0 }]);
}

#[test]
fn delay_between_steps_is_respected() {
    let mut s = state(3, olds(3));
    s.policy.delay_secs = 5;
    s.since_last_action_secs = 2;
    assert_eq!(next_step(&s), vec![]);
    s.since_last_action_secs = 5;
    assert_eq!(next_step(&s).len(), 1);
}

#[test]
fn a_failed_new_replica_triggers_rollback_or_pause() {
    let mut r = olds(3);
    r.push(Replica { failed: true, ..rep(3, "new", false) });
    let s = state(3, r.clone());
    assert_eq!(next_step(&s), vec![Action::Rollback { to: "old".into() }]);
    let mut s = state(3, r.clone());
    s.policy.on_failure = OnFailure::Pause;
    assert!(matches!(next_step(&s).as_slice(), [Action::Pause { .. }]));
    let mut s = state(3, r);
    s.previous = None;
    assert!(matches!(next_step(&s).as_slice(), [Action::Pause { .. }]));
}

#[test]
fn exceeding_the_progress_deadline_fails_the_rollout() {
    let mut s = state(3, olds(3));
    s.elapsed_secs = s.policy.progress_deadline_secs + 1;
    assert_eq!(next_step(&s), vec![Action::Rollback { to: "old".into() }]);
}

#[test]
fn a_rollout_that_cannot_make_progress_is_refused() {
    let mut s = state(3, olds(3));
    s.policy.max_surge = 0;
    s.policy.max_unavailable = 0;
    assert!(matches!(next_step(&s).as_slice(), [Action::Pause { .. }]));
}

#[test]
fn paused_rollouts_take_no_action() {
    let mut s = state(3, olds(3));
    s.paused = true;
    assert_eq!(next_step(&s), vec![]);
}

#[test]
fn recreate_stops_everything_before_starting() {
    let mut s = state(3, olds(3));
    s.policy.strategy = Strategy::Recreate;
    assert_eq!(next_step(&s), vec![Action::Drain { index: 0 }, Action::Drain { index: 1 }, Action::Drain { index: 2 }]);
    let mut r: Vec<Replica> = (0..3).map(|i| Replica { lifecycle: Lifecycle::Draining { for_secs: 40 }, ..rep(i, "old", true) }).collect();
    let mut s = state(3, r.clone());
    s.policy.strategy = Strategy::Recreate;
    let acts = next_step(&s);
    assert_eq!(acts.iter().filter(|a| matches!(a, Action::Remove { .. })).count(), 3);
    assert!(!acts.iter().any(|a| matches!(a, Action::Start { .. })));
    r.clear();
    let mut s = state(3, r);
    s.policy.strategy = Strategy::Recreate;
    assert_eq!(next_step(&s).len(), 3);
}

#[test]
fn steady_state_self_heals_missing_replicas() {
    let s = state(3, vec![rep(0, "new", true), rep(2, "new", true)]);
    assert_eq!(next_step(&s), vec![Action::Start { index: 1, revision: "new".into() }]);
}

#[test]
fn steady_state_scale_down_picks_the_idlest_replica() {
    let mut r: Vec<Replica> = (0..3).map(|i| rep(i, "new", true)).collect();
    r[0].connections = 50;
    r[1].connections = 2;
    r[2].connections = 9;
    assert_eq!(next_step(&state(2, r)), vec![Action::Drain { index: 1 }]);
}

#[test]
fn scale_up_during_a_rollout_only_adds_new_revision_replicas() {
    let s = state(5, olds(3));
    let acts = next_step(&s);
    assert!(acts.iter().all(|a| matches!(a, Action::Start { revision, .. } if revision == "new")));
    assert_eq!(acts.len(), 3);
}
