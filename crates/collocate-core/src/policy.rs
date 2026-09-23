use crate::size::parse_duration_secs;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Deserialize)]
#[serde(untagged)]
enum NumOrStr {
    Num(f64),
    Str(String),
}

pub fn duration_secs<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    match NumOrStr::deserialize(d)? {
        NumOrStr::Num(n) if n >= 0.0 && n.fract() == 0.0 => Ok(n as u64),
        NumOrStr::Num(n) => Err(serde::de::Error::custom(format!("invalid duration {n}"))),
        NumOrStr::Str(s) => parse_duration_secs(&s).map_err(serde::de::Error::custom),
    }
}

pub fn percent<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    match NumOrStr::deserialize(d)? {
        NumOrStr::Num(n) => Ok(n),
        NumOrStr::Str(s) => {
            s.trim().trim_end_matches('%').parse::<f64>().map_err(|_| serde::de::Error::custom(format!("invalid percentage {s:?}")))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricKind {
    Cpu,
    Memory,
    Connections,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricTarget {
    #[serde(rename = "type")]
    pub kind: MetricKind,
    pub target: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScaleRule {
    #[serde(rename = "stabilization", deserialize_with = "duration_secs")]
    pub stabilization_secs: u64,
    pub max_step: u32,
}

fn up_default() -> ScaleRule {
    ScaleRule { stabilization_secs: 30, max_step: 4 }
}

fn down_default() -> ScaleRule {
    ScaleRule { stabilization_secs: 300, max_step: 1 }
}

fn tolerance_default() -> f64 {
    10.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Autoscale {
    pub min: u32,
    pub max: u32,
    pub metrics: Vec<MetricTarget>,
    #[serde(default = "up_default")]
    pub up: ScaleRule,
    #[serde(default = "down_default")]
    pub down: ScaleRule,
    #[serde(default = "tolerance_default", rename = "tolerance", deserialize_with = "percent")]
    pub tolerance_pct: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    Rolling,
    Recreate,
    Canary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnFailure {
    Rollback,
    Pause,
}

fn one() -> u32 {
    1
}

fn ten() -> u64 {
    10
}

fn five() -> u64 {
    5
}

fn deadline() -> u64 {
    600
}

fn rollback() -> OnFailure {
    OnFailure::Rollback
}

fn rolling() -> Strategy {
    Strategy::Rolling
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePolicy {
    #[serde(default = "rolling")]
    pub strategy: Strategy,
    #[serde(default = "one")]
    pub max_surge: u32,
    #[serde(default)]
    pub max_unavailable: u32,
    #[serde(default = "ten", rename = "min_ready", deserialize_with = "duration_secs")]
    pub min_ready_secs: u64,
    #[serde(default = "five", rename = "delay", deserialize_with = "duration_secs")]
    pub delay_secs: u64,
    #[serde(default = "deadline", rename = "progress_deadline", deserialize_with = "duration_secs")]
    pub progress_deadline_secs: u64,
    #[serde(default = "rollback")]
    pub on_failure: OnFailure,
}

impl Default for UpdatePolicy {
    fn default() -> Self {
        UpdatePolicy {
            strategy: Strategy::Rolling,
            max_surge: 1,
            max_unavailable: 0,
            min_ready_secs: 10,
            delay_secs: 5,
            progress_deadline_secs: 600,
            on_failure: OnFailure::Rollback,
        }
    }
}
