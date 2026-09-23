use collocate_core::policy::{Autoscale, MetricKind};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricReading {
    pub kind: MetricKind,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    NoMetrics,
    WithinTolerance,
    Stabilizing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Hold(HoldReason),
    Scale { to: u32 },
}

#[derive(Debug, Clone)]
pub struct Scaler {
    policy: Autoscale,
    history: VecDeque<(u64, u32)>,
}

impl Scaler {
    pub fn new(policy: Autoscale) -> Self {
        Scaler { policy, history: VecDeque::new() }
    }

    pub fn policy(&self) -> &Autoscale {
        &self.policy
    }

    fn recommendation(&self, current: u32, readings: &[MetricReading]) -> Option<u32> {
        let tolerance = self.policy.tolerance_pct / 100.0;
        let mut best: Option<u32> = None;
        for target in &self.policy.metrics {
            if target.target <= 0.0 {
                continue;
            }
            let Some(reading) = readings.iter().find(|r| r.kind == target.kind && r.value.is_finite() && r.value >= 0.0) else {
                continue;
            };
            let ratio = reading.value / target.target;
            let wanted =
                if (ratio - 1.0).abs() <= tolerance { current } else { (f64::from(current) * ratio - 1e-9).ceil().max(0.0) as u32 };
            best = Some(best.map_or(wanted, |b| b.max(wanted)));
        }
        best
    }

    pub fn decide(&mut self, now: u64, current: u32, readings: &[MetricReading]) -> Decision {
        let (min, max) = (self.policy.min, self.policy.max);
        if current < min {
            return Decision::Scale { to: min };
        }
        if current > max {
            return Decision::Scale { to: max };
        }
        let Some(wanted) = self.recommendation(current, readings) else {
            return Decision::Hold(HoldReason::NoMetrics);
        };
        let wanted = wanted.clamp(min, max);
        self.history.push_back((now, wanted));
        let longest = self.policy.up.stabilization_secs.max(self.policy.down.stabilization_secs);
        while self.history.front().is_some_and(|(t, _)| *t + longest < now) {
            self.history.pop_front();
        }
        let window = |secs: u64| self.history.iter().filter(move |(t, _)| *t >= now.saturating_sub(secs)).map(|(_, d)| *d);
        if wanted > current {
            let stable = window(self.policy.up.stabilization_secs).min().unwrap_or(wanted);
            if stable > current {
                let to = stable.min(current.saturating_add(self.policy.up.max_step)).min(max);
                return Decision::Scale { to };
            }
            Decision::Hold(HoldReason::Stabilizing)
        } else if wanted < current {
            let stable = window(self.policy.down.stabilization_secs).max().unwrap_or(wanted);
            if stable < current {
                let to = stable.max(current.saturating_sub(self.policy.down.max_step)).max(min);
                return Decision::Scale { to };
            }
            Decision::Hold(HoldReason::Stabilizing)
        } else {
            Decision::Hold(HoldReason::WithinTolerance)
        }
    }
}
