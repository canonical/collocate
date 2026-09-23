use collocate_core::spec::RestartPolicy;
use collocated::restart::{restart_delay, should_restart};
use std::time::Duration;

#[test]
fn backoff_doubles_and_caps() {
    assert_eq!(restart_delay(0), Duration::from_millis(100));
    assert_eq!(restart_delay(1), Duration::from_millis(200));
    assert_eq!(restart_delay(4), Duration::from_millis(1600));
    assert_eq!(restart_delay(20), Duration::from_secs(60));
    assert_eq!(restart_delay(u32::MAX), Duration::from_secs(60));
}

#[test]
fn never_restart_when_policy_is_no_or_user_stopped() {
    assert!(!should_restart(RestartPolicy::No, 1, 0, false));
    assert!(!should_restart(RestartPolicy::Always, 0, 0, true));
    assert!(!should_restart(RestartPolicy::OnFailure { max: 3 }, 1, 0, true));
}

#[test]
fn always_restarts_regardless_of_status() {
    assert!(should_restart(RestartPolicy::Always, 0, 100, false));
    assert!(should_restart(RestartPolicy::Always, 137, 0, false));
}

#[test]
fn on_failure_only_after_nonzero_exits_and_respects_the_limit() {
    let p = RestartPolicy::OnFailure { max: 3 };
    assert!(!should_restart(p, 0, 0, false));
    assert!(should_restart(p, 1, 0, false));
    assert!(should_restart(p, 1, 2, false));
    assert!(!should_restart(p, 1, 3, false));
    assert!(should_restart(RestartPolicy::OnFailure { max: 0 }, 1, 999, false));
}
