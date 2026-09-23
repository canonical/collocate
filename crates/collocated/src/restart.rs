use collocate_core::spec::RestartPolicy;
use std::time::Duration;

const BASE_MS: u64 = 100;
const CAP: Duration = Duration::from_secs(60);

pub fn restart_delay(attempt: u32) -> Duration {
    if attempt >= 20 {
        return CAP;
    }
    Duration::from_millis(BASE_MS << attempt).min(CAP)
}

pub fn should_restart(policy: RestartPolicy, exit_code: i32, attempts: u32, user_stopped: bool) -> bool {
    if user_stopped {
        return false;
    }
    match policy {
        RestartPolicy::No => false,
        RestartPolicy::Always => true,
        RestartPolicy::OnFailure { max } => exit_code != 0 && (max == 0 || attempts < max),
    }
}
