use collocate_core::limits::Limits;
use collocate_core::size::parse_size;
use collocate_core::Error;

#[test]
fn parses_plain_bytes() {
    assert_eq!(parse_size("100").unwrap(), 100);
}

#[test]
fn parses_suffixes_case_insensitively() {
    assert_eq!(parse_size("10k").unwrap(), 10 * 1024);
    assert_eq!(parse_size("512m").unwrap(), 512 * 1024 * 1024);
    assert_eq!(parse_size("2G").unwrap(), 2 * 1024 * 1024 * 1024);
}

#[test]
fn parses_binary_and_byte_suffixes() {
    assert_eq!(parse_size("100B").unwrap(), 100);
    assert_eq!(parse_size("64Ki").unwrap(), 64 * 1024);
    assert_eq!(parse_size("512Mi").unwrap(), 512 * 1024 * 1024);
    assert_eq!(parse_size("2GiB").unwrap(), 2 * 1024 * 1024 * 1024);
    assert_eq!(parse_size("1Ti").unwrap(), 1 << 40);
    assert_eq!(parse_size("4KB").unwrap(), 4 * 1024);
    assert_eq!(parse_size("512MB").unwrap(), 512 * 1024 * 1024);
}

#[test]
fn rejects_garbage() {
    assert!(matches!(parse_size(""), Err(Error::InvalidSize(_))));
    assert!(matches!(parse_size("12x"), Err(Error::InvalidSize(_))));
    assert!(matches!(parse_size("-5m"), Err(Error::InvalidSize(_))));
    assert!(matches!(parse_size("99999999999999999999g"), Err(Error::InvalidSize(_))));
    assert!(matches!(parse_size("k"), Err(Error::InvalidSize(_))));
    assert!(matches!(parse_size("5i"), Err(Error::InvalidSize(_))));
}

fn writes(l: &Limits) -> Vec<(String, String)> {
    l.cgroup_writes()
}

fn get<'a>(w: &'a [(String, String)], k: &str) -> Option<&'a str> {
    w.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

#[test]
fn default_limits_only_cap_pids() {
    let w = writes(&Limits::default());
    assert_eq!(get(&w, "pids.max"), Some("4096"));
    assert_eq!(get(&w, "memory.max"), None);
    assert_eq!(get(&w, "cpu.max"), None);
}

#[test]
fn cpus_translate_to_quota() {
    let l = Limits { cpus_milli: Some(1500), ..Limits::default() };
    assert_eq!(get(&writes(&l), "cpu.max"), Some("150000 100000"));
}

#[test]
fn memory_sets_oom_group_and_zero_swap_by_default() {
    let l = Limits { memory: Some(512 * 1024 * 1024), ..Limits::default() };
    let w = writes(&l);
    assert_eq!(get(&w, "memory.max"), Some("536870912"));
    assert_eq!(get(&w, "memory.swap.max"), Some("0"));
    assert_eq!(get(&w, "memory.oom.group"), Some("1"));
}

#[test]
fn explicit_swap_is_respected() {
    let l = Limits { memory: Some(1 << 30), swap: Some(1 << 29), ..Limits::default() };
    assert_eq!(get(&writes(&l), "memory.swap.max"), Some("536870912"));
}

#[test]
fn cpu_weight_written_when_set() {
    let l = Limits { cpu_weight: Some(500), ..Limits::default() };
    assert_eq!(get(&writes(&l), "cpu.weight"), Some("500"));
}

#[test]
fn validation_rejects_out_of_range() {
    assert!(Limits { cpu_weight: Some(0), ..Limits::default() }.validate().is_err());
    assert!(Limits { cpu_weight: Some(10001), ..Limits::default() }.validate().is_err());
    assert!(Limits { cpus_milli: Some(0), ..Limits::default() }.validate().is_err());
    assert!(Limits { pids_max: 0, ..Limits::default() }.validate().is_err());
    assert!(Limits { memory: Some(0), ..Limits::default() }.validate().is_err());
    assert!(Limits::default().validate().is_ok());
}
