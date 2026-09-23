use collocate_controller::placement::{pick_node, Need, NodeCapacity};

fn node(name: &str, mem_free_gib: u64, cpu_free: u32, replicas: u32) -> NodeCapacity {
    NodeCapacity {
        name: name.into(),
        memory_total: 16 << 30,
        memory_committed: (16 - mem_free_gib) << 30,
        cpu_total_milli: 8000,
        cpu_committed_milli: 8000 - cpu_free,
        service_replicas: replicas,
    }
}

const NEED: Need = Need { memory: 512 << 20, cpu_milli: 500 };

#[test]
fn picks_the_node_with_most_free_memory() {
    let nodes = vec![node("a", 4, 4000, 0), node("b", 8, 4000, 0)];
    assert_eq!(pick_node(&nodes, NEED).as_deref(), Some("b"));
}

#[test]
fn ties_prefer_fewer_replicas_then_name() {
    let nodes = vec![node("b", 8, 4000, 2), node("a", 8, 4000, 1), node("c", 8, 4000, 1)];
    assert_eq!(pick_node(&nodes, NEED).as_deref(), Some("a"));
}

#[test]
fn refuses_nodes_without_headroom() {
    let nodes = vec![node("a", 0, 4000, 0)];
    assert_eq!(pick_node(&nodes, NEED), None);
    let nodes = vec![node("a", 8, 100, 0)];
    assert_eq!(pick_node(&nodes, NEED), None);
}

#[test]
fn keeps_a_reserve_for_the_host() {
    let mut n = node("a", 0, 4000, 0);
    n.memory_committed = n.memory_total - (600 << 20);
    assert_eq!(pick_node(&[n.clone()], Need { memory: 512 << 20, cpu_milli: 100 }), None);
    n.memory_committed = 0;
    assert_eq!(pick_node(&[n], Need { memory: 512 << 20, cpu_milli: 100 }).as_deref(), Some("a"));
}

#[test]
fn empty_candidates_yield_none() {
    assert_eq!(pick_node(&[], NEED), None);
}
