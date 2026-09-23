#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeCapacity {
    pub name: String,
    pub memory_total: u64,
    pub memory_committed: u64,
    pub cpu_total_milli: u32,
    pub cpu_committed_milli: u32,
    pub service_replicas: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Need {
    pub memory: u64,
    pub cpu_milli: u32,
}

const RESERVE_PERCENT: u64 = 5;

pub fn pick_node(nodes: &[NodeCapacity], need: Need) -> Option<String> {
    nodes
        .iter()
        .filter_map(|n| {
            let reserve = n.memory_total / 100 * RESERVE_PERCENT;
            let free = n.memory_total.saturating_sub(n.memory_committed);
            let after = free.checked_sub(need.memory)?;
            if after < reserve {
                return None;
            }
            let cpu_free = n.cpu_total_milli.saturating_sub(n.cpu_committed_milli);
            if cpu_free < need.cpu_milli {
                return None;
            }
            Some((after, n))
        })
        .min_by(|(fa, a), (fb, b)| fb.cmp(fa).then(a.service_replicas.cmp(&b.service_replicas)).then(a.name.cmp(&b.name)))
        .map(|(_, n)| n.name.clone())
}
