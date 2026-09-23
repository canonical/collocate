use collocate_core::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

pub fn topo_order(deps: &BTreeMap<String, Vec<String>>) -> Result<Vec<String>> {
    for (svc, ds) in deps {
        for d in ds {
            if !deps.contains_key(d) {
                return Err(Error::InvalidSpec(format!("service {svc} depends on unknown service {d}")));
            }
        }
    }
    let mut remaining: BTreeMap<&String, BTreeSet<&String>> = deps.iter().map(|(k, v)| (k, v.iter().collect())).collect();
    let mut order = Vec::new();
    loop {
        let ready: Vec<&String> = remaining.iter().filter(|(_, ds)| ds.is_empty()).map(|(k, _)| *k).collect();
        if ready.is_empty() {
            break;
        }
        for r in ready {
            remaining.remove(r);
            order.push(r.clone());
            for ds in remaining.values_mut() {
                ds.remove(r);
            }
        }
    }
    if remaining.is_empty() {
        return Ok(order);
    }
    let leftover: BTreeMap<String, BTreeSet<String>> =
        remaining.into_iter().map(|(k, v)| (k.clone(), v.into_iter().cloned().collect())).collect();
    let start = leftover.keys().next().cloned().unwrap_or_default();
    let mut path: Vec<String> = vec![start.clone()];
    let mut cur = start.clone();
    loop {
        let next = leftover.get(&cur).and_then(|ds| ds.iter().next().cloned()).unwrap_or_else(|| start.clone());
        if let Some(pos) = path.iter().position(|p| *p == next) {
            let mut cycle: Vec<String> = path[pos..].to_vec();
            cycle.push(next);
            return Err(Error::InvalidSpec(format!("cycle among services {}", cycle.join(" -> "))));
        }
        path.push(next.clone());
        cur = next;
    }
}

pub fn shutdown_order(order: &[String]) -> Vec<String> {
    order.iter().rev().cloned().collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actual {
    pub name: String,
    pub hash: String,
    pub running: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Plan {
    pub create: Vec<String>,
    pub recreate: Vec<String>,
    pub start: Vec<String>,
    pub keep: Vec<String>,
    pub remove: Vec<String>,
}

impl Plan {
    pub fn is_noop(&self) -> bool {
        self.create.is_empty() && self.recreate.is_empty() && self.start.is_empty() && self.remove.is_empty()
    }
}

pub fn diff(desired: &[Item], actual: &[Actual]) -> Plan {
    let have: BTreeMap<&str, &Actual> = actual.iter().map(|a| (a.name.as_str(), a)).collect();
    let mut plan = Plan::default();
    for item in desired {
        match have.get(item.name.as_str()) {
            None => plan.create.push(item.name.clone()),
            Some(a) if a.hash != item.hash => plan.recreate.push(item.name.clone()),
            Some(a) if !a.running => plan.start.push(item.name.clone()),
            Some(_) => plan.keep.push(item.name.clone()),
        }
    }
    let wanted: BTreeSet<&str> = desired.iter().map(|i| i.name.as_str()).collect();
    let mut extra: Vec<String> = actual.iter().filter(|a| !wanted.contains(a.name.as_str())).map(|a| a.name.clone()).collect();
    extra.sort();
    plan.remove = extra;
    plan
}
