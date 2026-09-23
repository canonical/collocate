use collocate_controller::rollout::{next_step, Action, Health, Lifecycle, Replica, State};
use collocate_core::policy::UpdatePolicy;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

struct Sim {
    state: State,
    bad_revision: Option<String>,
    start_secs: u64,
    age: Vec<(u32, u64)>,
    last_action: u64,
    now: u64,
    max_live_seen: usize,
    min_ready_seen: usize,
    completed: bool,
    rolled_back: bool,
}

impl Sim {
    fn new(desired: u32, policy: UpdatePolicy, bad: Option<&str>) -> Sim {
        let replicas = (0..desired)
            .map(|i| Replica {
                index: i,
                revision: "old".into(),
                health: Health::Healthy { for_secs: 1000 },
                lifecycle: Lifecycle::Live,
                failed: false,
                connections: 0,
            })
            .collect();
        Sim {
            state: State {
                desired,
                target: "new".into(),
                previous: Some("old".into()),
                policy,
                drain_secs: 3,
                replicas,
                elapsed_secs: 0,
                since_last_action_secs: 1000,
                paused: false,
            },
            bad_revision: bad.map(String::from),
            start_secs: 4,
            age: (0..desired).map(|i| (i, 1000)).collect(),
            last_action: 0,
            now: 0,
            max_live_seen: 0,
            min_ready_seen: usize::MAX,
            completed: false,
            rolled_back: false,
        }
    }

    fn ready(&self) -> usize {
        self.state
            .replicas
            .iter()
            .filter(|r| {
                matches!(r.lifecycle, Lifecycle::Live)
                    && matches!(r.health, Health::Healthy { for_secs } if for_secs >= self.state.policy.min_ready_secs)
            })
            .count()
    }

    fn live(&self) -> usize {
        self.state.replicas.iter().filter(|r| matches!(r.lifecycle, Lifecycle::Live)).count()
    }

    fn tick(&mut self) {
        self.now += 1;
        self.state.elapsed_secs += 1;
        self.state.since_last_action_secs = self.now - self.last_action;
        let bad = self.bad_revision.clone();
        let start = self.start_secs;
        for r in &mut self.state.replicas {
            match &mut r.lifecycle {
                Lifecycle::Draining { for_secs } => *for_secs += 1,
                Lifecycle::Live => {
                    let age = self.age.iter_mut().find(|(i, _)| *i == r.index).map(|(_, a)| a);
                    if let Some(a) = age {
                        *a += 1;
                        if Some(&r.revision) == bad.as_ref() {
                            r.failed = *a > start;
                            r.health = Health::Starting;
                        } else if *a >= start {
                            let since = *a - start;
                            r.health = Health::Healthy { for_secs: since };
                        }
                    }
                }
            }
        }
        let actions = next_step(&self.state);
        if !actions.is_empty() && !matches!(actions.as_slice(), [Action::Complete]) {
            self.last_action = self.now;
        }
        for a in actions {
            match a {
                Action::Start { index, revision } => {
                    assert!(!self.state.replicas.iter().any(|r| r.index == index), "index {index} reused while live");
                    self.state.replicas.push(Replica {
                        index,
                        revision,
                        health: Health::Starting,
                        lifecycle: Lifecycle::Live,
                        failed: false,
                        connections: 0,
                    });
                    self.age.retain(|(i, _)| *i != index);
                    self.age.push((index, 0));
                }
                Action::Drain { index } => {
                    if let Some(r) = self.state.replicas.iter_mut().find(|r| r.index == index) {
                        r.lifecycle = Lifecycle::Draining { for_secs: 0 };
                    }
                }
                Action::Remove { index } => self.state.replicas.retain(|r| r.index != index),
                Action::Rollback { to } => {
                    self.rolled_back = true;
                    self.state.target = to;
                    self.state.previous = None;
                    self.state.elapsed_secs = 0;
                }
                Action::Pause { .. } => self.state.paused = true,
                Action::Complete => self.completed = true,
            }
        }
        self.max_live_seen = self.max_live_seen.max(self.live());
        self.min_ready_seen = self.min_ready_seen.min(self.ready());
    }
}

#[test]
fn rollouts_converge_within_bounds_for_random_configurations() {
    let mut rng = Lcg(42);
    for case in 0..300 {
        let desired = 1 + rng.next(8) as u32;
        let mut surge = rng.next(4) as u32;
        let mut unavail = rng.next(3) as u32;
        unavail = unavail.min(desired.saturating_sub(1));
        if surge + unavail == 0 {
            surge = 1;
        }
        let policy = UpdatePolicy {
            max_surge: surge,
            max_unavailable: unavail,
            min_ready_secs: rng.next(6),
            delay_secs: rng.next(3),
            ..UpdatePolicy::default()
        };
        let mut sim = Sim::new(desired, policy, None);
        for _ in 0..2000 {
            sim.tick();
            assert!(sim.live() as u32 <= desired + surge, "case {case}: surge exceeded");
            assert!(sim.ready() as u32 + unavail >= desired || sim.now < 1, "case {case}: availability broken at t={}", sim.now);
            if sim.completed && sim.state.replicas.iter().all(|r| r.revision == "new") {
                break;
            }
        }
        assert!(sim.completed, "case {case}: did not converge");
        assert!(sim.state.replicas.iter().all(|r| r.revision == "new" && matches!(r.lifecycle, Lifecycle::Live)), "case {case}");
        assert_eq!(sim.state.replicas.len() as u32, desired, "case {case}");
    }
}

#[test]
fn a_bad_revision_rolls_back_without_losing_availability() {
    let mut rng = Lcg(7);
    for case in 0..100 {
        let desired = 2 + rng.next(5) as u32;
        let policy = UpdatePolicy { max_surge: 1, max_unavailable: 0, min_ready_secs: 2, delay_secs: 0, ..UpdatePolicy::default() };
        let mut sim = Sim::new(desired, policy, Some("new"));
        for _ in 0..3000 {
            sim.tick();
            assert!(sim.ready() as u32 >= desired, "case {case}: availability broken at t={}", sim.now);
            if sim.rolled_back && sim.state.replicas.iter().all(|r| r.revision == "old") && sim.state.replicas.len() as u32 == desired {
                break;
            }
        }
        assert!(sim.rolled_back, "case {case}: never rolled back");
        assert!(sim.state.replicas.iter().all(|r| r.revision == "old"), "case {case}");
        assert_eq!(sim.state.replicas.len() as u32, desired, "case {case}");
    }
}
