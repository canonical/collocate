use collocate_core::policy::{OnFailure, Strategy, UpdatePolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Starting,
    Healthy { for_secs: u64 },
    Unhealthy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Live,
    Draining { for_secs: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replica {
    pub index: u32,
    pub revision: String,
    pub health: Health,
    pub lifecycle: Lifecycle,
    pub failed: bool,
    pub connections: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub desired: u32,
    pub target: String,
    pub previous: Option<String>,
    pub policy: UpdatePolicy,
    pub drain_secs: u64,
    pub replicas: Vec<Replica>,
    pub elapsed_secs: u64,
    pub since_last_action_secs: u64,
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Start { index: u32, revision: String },
    Drain { index: u32 },
    Remove { index: u32 },
    Rollback { to: String },
    Pause { reason: String },
    Complete,
}

fn is_live(r: &Replica) -> bool {
    matches!(r.lifecycle, Lifecycle::Live)
}

fn is_ready(r: &Replica, min_ready: u64) -> bool {
    is_live(r) && matches!(r.health, Health::Healthy { for_secs } if for_secs >= min_ready)
}

fn free_indexes(s: &State, bound: u32, want: u32) -> Vec<u32> {
    (0..bound).filter(|i| !s.replicas.iter().any(|r| r.index == *i)).take(want as usize).collect()
}

fn failure(s: &State, reason: &str) -> Action {
    match (&s.policy.on_failure, &s.previous) {
        (OnFailure::Rollback, Some(prev)) => Action::Rollback { to: prev.clone() },
        _ => Action::Pause { reason: reason.to_string() },
    }
}

pub fn next_step(s: &State) -> Vec<Action> {
    if s.paused {
        return Vec::new();
    }
    let live: Vec<&Replica> = s.replicas.iter().filter(|r| is_live(r)).collect();
    let old: Vec<&Replica> = live.iter().copied().filter(|r| r.revision != s.target).collect();
    let new: Vec<&Replica> = live.iter().copied().filter(|r| r.revision == s.target).collect();
    let rolling = !old.is_empty();
    let pol = &s.policy;

    if rolling {
        if new.iter().any(|r| r.failed) {
            return vec![failure(s, "new replica failed to become healthy")];
        }
        if s.elapsed_secs > pol.progress_deadline_secs {
            return vec![failure(s, "progress deadline exceeded")];
        }
        if pol.strategy == Strategy::Rolling && pol.max_surge == 0 && pol.max_unavailable == 0 {
            return vec![Action::Pause { reason: "max_surge and max_unavailable cannot both be zero".into() }];
        }
    }

    let mut actions: Vec<Action> = s
        .replicas
        .iter()
        .filter(|r| matches!(r.lifecycle, Lifecycle::Draining { for_secs } if for_secs >= s.drain_secs))
        .map(|r| Action::Remove { index: r.index })
        .collect();
    let draining = s.replicas.iter().any(|r| !is_live(r));
    let bound = s.desired + pol.max_surge;
    let missing_new = s.desired.saturating_sub(new.len() as u32);
    let recreate_waits = pol.strategy == Strategy::Recreate && s.replicas.iter().any(|r| !is_live(r) && r.revision != s.target);

    if !rolling {
        if new.len() as u32 > s.desired {
            let mut victims: Vec<&Replica> = new.clone();
            victims.sort_by(|a, b| a.connections.cmp(&b.connections).then(b.index.cmp(&a.index)));
            let excess = new.len() as u32 - s.desired;
            actions.extend(victims.iter().take(excess as usize).map(|r| Action::Drain { index: r.index }));
        } else if missing_new > 0 && !recreate_waits {
            actions.extend(
                free_indexes(s, bound.max(s.desired), missing_new)
                    .into_iter()
                    .map(|index| Action::Start { index, revision: s.target.clone() }),
            );
        }
        if actions.is_empty() && !draining && new.iter().all(|r| is_ready(r, pol.min_ready_secs)) {
            return vec![Action::Complete];
        }
        return actions;
    }

    if pol.strategy == Strategy::Recreate {
        if s.since_last_action_secs < pol.delay_secs && actions.is_empty() {
            return actions;
        }
        actions.extend(old.iter().map(|r| Action::Drain { index: r.index }));
        return actions;
    }

    if s.since_last_action_secs < pol.delay_secs {
        return actions;
    }

    let available = live.iter().filter(|r| is_ready(r, pol.min_ready_secs)).count() as u32;
    let required = s.desired.saturating_sub(pol.max_unavailable);
    let mut allowed = available.saturating_sub(required);
    let mut victims: Vec<&Replica> = old.iter().copied().filter(|r| !is_ready(r, pol.min_ready_secs)).collect();
    let mut ready_old: Vec<&Replica> = old.iter().copied().filter(|r| is_ready(r, pol.min_ready_secs)).collect();
    ready_old.sort_by_key(|r| std::cmp::Reverse(r.index));
    for r in ready_old {
        if allowed == 0 {
            break;
        }
        allowed -= 1;
        victims.push(r);
    }
    actions.extend(victims.iter().map(|r| Action::Drain { index: r.index }));

    let room = bound.saturating_sub(live.len() as u32);
    let can_start = missing_new.min(room);
    actions.extend(free_indexes(s, bound, can_start).into_iter().map(|index| Action::Start { index, revision: s.target.clone() }));
    actions
}
