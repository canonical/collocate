use collocate_sys::caps::{default_set, Cap, CapSet};

#[test]
fn names_map_to_kernel_numbers() {
    for (name, n) in [
        ("CHOWN", 0),
        ("DAC_OVERRIDE", 1),
        ("DAC_READ_SEARCH", 2),
        ("FOWNER", 3),
        ("FSETID", 4),
        ("KILL", 5),
        ("SETGID", 6),
        ("SETUID", 7),
        ("SETPCAP", 8),
        ("NET_BIND_SERVICE", 10),
        ("NET_ADMIN", 12),
        ("NET_RAW", 13),
        ("SYS_MODULE", 16),
        ("SYS_RAWIO", 17),
        ("SYS_CHROOT", 18),
        ("SYS_PTRACE", 19),
        ("SYS_ADMIN", 21),
        ("SYS_TIME", 25),
        ("MKNOD", 27),
        ("AUDIT_WRITE", 29),
        ("SETFCAP", 31),
    ] {
        assert_eq!(Cap::from_name(name).unwrap().number(), n, "{name}");
    }
}

#[test]
fn names_are_forgiving() {
    assert_eq!(Cap::from_name("CAP_CHOWN"), Cap::from_name("chown"));
    assert!(Cap::from_name("cap_sys_admin").is_some());
    assert!(Cap::from_name("NOPE").is_none());
}

#[test]
fn default_set_matches_the_documented_list() {
    let d = default_set();
    let expected = [
        "CHOWN",
        "DAC_OVERRIDE",
        "FOWNER",
        "FSETID",
        "KILL",
        "SETGID",
        "SETUID",
        "SETPCAP",
        "NET_BIND_SERVICE",
        "SYS_CHROOT",
        "MKNOD",
        "AUDIT_WRITE",
        "SETFCAP",
    ];
    assert_eq!(d.names().len(), expected.len());
    for n in expected {
        assert!(d.contains(Cap::from_name(n).unwrap()), "{n}");
    }
    for n in ["NET_ADMIN", "NET_RAW", "SYS_ADMIN", "SYS_PTRACE", "SYS_MODULE", "SYS_RAWIO", "SYS_TIME", "DAC_READ_SEARCH"] {
        assert!(!d.contains(Cap::from_name(n).unwrap()), "{n}");
    }
}

#[test]
fn policy_adds_and_drops() {
    let s = CapSet::with_policy(&["NET_RAW".to_string()], &["CHOWN".to_string()]).unwrap();
    assert!(s.contains(Cap::from_name("NET_RAW").unwrap()));
    assert!(!s.contains(Cap::from_name("CHOWN").unwrap()));
    assert!(s.contains(Cap::from_name("KILL").unwrap()));
}

#[test]
fn drop_all_empties_the_set() {
    let s = CapSet::with_policy(&[], &["ALL".to_string()]).unwrap();
    assert!(s.names().is_empty());
    let s = CapSet::with_policy(&["SYS_TIME".to_string()], &["ALL".to_string()]).unwrap();
    assert_eq!(s.names(), vec!["SYS_TIME".to_string()]);
}

#[test]
fn unknown_names_are_rejected() {
    assert!(CapSet::with_policy(&["BOGUS".to_string()], &[]).is_err());
    assert!(CapSet::with_policy(&[], &["BOGUS".to_string()]).is_err());
}

#[test]
fn kernel_words_split_at_bit_32() {
    let s = CapSet::with_policy(&["CHOWN".to_string(), "SETFCAP".to_string()], &["ALL".to_string()]).unwrap();
    let (lo, hi) = s.words();
    assert_eq!(lo, 1 | (1 << 31));
    assert_eq!(hi, 0);
    let s = CapSet::with_policy(&["SYSLOG".to_string()], &["ALL".to_string()]).unwrap();
    assert_eq!(s.words(), (0, 1 << (34 - 32)));
}
