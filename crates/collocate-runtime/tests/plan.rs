use collocate_core::spec::{Mount, RootSource, Series, Spec};
use collocate_runtime::env::{build_env, with_home};
use collocate_runtime::mountplan::{mount_plan, Extras, MountOp};
use collocate_runtime::overlay::OverlayPlan;
use collocate_runtime::user::resolve_user;
use std::collections::HashMap;
use std::path::PathBuf;

fn spec() -> Spec {
    Spec::new("web", RootSource::Base { series: Series::Noble, build_id: "b".into() }, vec!["/bin/sh".into()])
}

fn extras() -> Extras {
    Extras {
        resolv: "/run/x/resolv.conf".into(),
        hosts: "/run/x/hosts".into(),
        hostname: "/run/x/hostname".into(),
        init: "/usr/libexec/collocate/collocate-init".into(),
        secrets_dir: "/run/collocate/secrets/abc".into(),
        volumes: HashMap::new(),
    }
}

#[test]
fn overlay_options_preserve_layer_order_and_flags() {
    let plan = OverlayPlan {
        lowers: vec!["/l/top".into(), "/l/mid".into(), "/l/base".into()],
        upper: "/u".into(),
        work: "/w".into(),
        volatile: true,
    };
    let opts = plan.options().unwrap();
    let keys: Vec<&str> = opts.iter().map(|o| o.key()).collect();
    assert_eq!(keys, vec!["lowerdir+", "lowerdir+", "lowerdir+", "upperdir", "workdir", "volatile"]);
    assert_eq!(opts[0].value(), Some("/l/top"));
    assert_eq!(opts[2].value(), Some("/l/base"));
    assert_eq!(opts[5].value(), None);
}

#[test]
fn overlay_without_volatile_and_without_lowers() {
    let plan = OverlayPlan { lowers: vec!["/l".into()], upper: "/u".into(), work: "/w".into(), volatile: false };
    assert!(!plan.options().unwrap().iter().any(|o| o.key() == "volatile"));
    let none = OverlayPlan { lowers: vec![], upper: "/u".into(), work: "/w".into(), volatile: false };
    assert!(none.options().is_err());
}

fn position(ops: &[MountOp], f: impl Fn(&MountOp) -> bool) -> usize {
    ops.iter().position(f).unwrap_or_else(|| panic!("op missing in {ops:?}"))
}

#[test]
fn plan_contains_the_standard_filesystems_in_a_safe_order() {
    let ops = mount_plan(&spec(), &extras()).unwrap();
    let proc_ = position(&ops, |o| matches!(o, MountOp::Proc));
    let dev = position(&ops, |o| matches!(o, MountOp::DevTmpfs));
    let node = position(&ops, |o| matches!(o, MountOp::DevNode(n) if *n == "null"));
    let pts = position(&ops, |o| matches!(o, MountOp::DevPts));
    let run = position(&ops, |o| matches!(o, MountOp::RunTmpfs));
    let mask = position(&ops, |o| matches!(o, MountOp::MaskFile(p) if p == "/proc/kcore"));
    assert!(proc_ < mask && dev < node && dev < pts && run < ops.len());
    assert!(ops.iter().any(|o| matches!(o, MountOp::SysfsRo)));
    assert!(ops.iter().any(|o| matches!(o, MountOp::Cgroup2Ro)));
    assert!(ops.iter().any(|o| matches!(o, MountOp::ProcSysRo)));
    for n in ["null", "zero", "full", "random", "urandom", "tty"] {
        assert!(ops.iter().any(|o| matches!(o, MountOp::DevNode(x) if *x == n)), "{n}");
    }
    for p in ["/proc/kcore", "/proc/keys", "/proc/sysrq-trigger"] {
        assert!(ops.iter().any(|o| matches!(o, MountOp::MaskFile(x) if x == p)), "{p}");
    }
    assert!(ops.iter().any(|o| matches!(o, MountOp::MaskDir(x) if x == "/sys/firmware")));
}

#[test]
fn plan_binds_generated_files_and_init_read_only() {
    let ops = mount_plan(&spec(), &extras()).unwrap();
    for (src, dst) in [
        ("/run/x/resolv.conf", "/etc/resolv.conf"),
        ("/run/x/hosts", "/etc/hosts"),
        ("/run/x/hostname", "/etc/hostname"),
        ("/usr/libexec/collocate/collocate-init", "/.collocate/init"),
    ] {
        assert!(
            ops.iter().any(|o| matches!(o, MountOp::Bind { src: s, dst: d, ro: true } if s == &PathBuf::from(src) && d == dst)),
            "{dst}"
        );
    }
}

#[test]
fn plan_maps_spec_mounts() {
    let mut s = spec();
    s.mounts.push(Mount::Bind { src: "/srv/data".into(), dst: "/data".into(), ro: false });
    s.mounts.push(Mount::Bind { src: "/srv/ro".into(), dst: "/ro".into(), ro: true });
    s.mounts.push(Mount::Tmpfs { dst: "/tmp".into(), size: Some(64 << 20) });
    s.mounts.push(Mount::Secret { name: "pw".into(), dst: "/run/secrets/pw".into() });
    s.mounts.push(Mount::Config { src: "/run/collocate/configs/app".into(), dst: "/etc/app.yaml".into() });
    s.mounts.push(Mount::Volume { name: "pg".into(), dst: "/var/lib/pg".into() });
    let mut e = extras();
    e.volumes.insert("pg".into(), "/var/lib/collocate/volumes/pg".into());
    let ops = mount_plan(&s, &e).unwrap();
    let run = position(&ops, |o| matches!(o, MountOp::RunTmpfs));
    let secret = position(&ops, |o| matches!(o, MountOp::Bind { dst, .. } if dst == "/run/secrets/pw"));
    assert!(run < secret);
    assert!(ops.contains(&MountOp::Bind { src: "/srv/data".into(), dst: "/data".into(), ro: false }));
    assert!(ops.contains(&MountOp::Bind { src: "/srv/ro".into(), dst: "/ro".into(), ro: true }));
    assert!(ops.contains(&MountOp::Tmpfs { dst: "/tmp".into(), size: Some(64 << 20) }));
    assert!(ops.contains(&MountOp::Bind { src: "/run/collocate/secrets/abc/pw".into(), dst: "/run/secrets/pw".into(), ro: true }));
    assert!(ops.contains(&MountOp::Bind { src: "/run/collocate/configs/app".into(), dst: "/etc/app.yaml".into(), ro: true }));
    assert!(ops.contains(&MountOp::Bind { src: "/var/lib/collocate/volumes/pg".into(), dst: "/var/lib/pg".into(), ro: false }));
}

#[test]
fn tmp_is_always_a_writable_tmpfs_by_default() {
    let ops = mount_plan(&spec(), &extras()).unwrap();
    assert!(ops.contains(&MountOp::Tmpfs { dst: "/tmp".into(), size: None }));
}

#[test]
fn an_explicit_tmp_mount_is_not_duplicated() {
    let mut s = spec();
    s.mounts.push(Mount::Tmpfs { dst: "/tmp".into(), size: Some(64 << 20) });
    let ops = mount_plan(&s, &extras()).unwrap();
    assert_eq!(ops.iter().filter(|o| matches!(o, MountOp::Tmpfs { dst, .. } if dst == "/tmp")).count(), 1);
    assert!(ops.contains(&MountOp::Tmpfs { dst: "/tmp".into(), size: Some(64 << 20) }));
}

#[test]
fn unknown_named_volumes_are_errors() {
    let mut s = spec();
    s.mounts.push(Mount::Volume { name: "ghost".into(), dst: "/x".into() });
    assert!(mount_plan(&s, &extras()).is_err());
}

#[test]
fn env_has_defaults_and_spec_values_win() {
    let mut s = spec();
    let env = build_env(&s);
    assert!(env.contains(&"PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string()));
    assert!(env.contains(&"HOSTNAME=web".to_string()));
    assert!(!env.iter().any(|e| e.starts_with("HOME=")));
    s.process.env.push(("PATH".into(), "/custom".into()));
    s.process.env.push(("FOO".into(), "bar".into()));
    let env = build_env(&s);
    assert_eq!(env.iter().filter(|e| e.starts_with("PATH=")).count(), 1);
    assert!(env.contains(&"PATH=/custom".to_string()));
    assert!(env.contains(&"FOO=bar".to_string()));
}

const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\napp:x:1000:1000::/home/app:/bin/sh\nnobody:x:65534:65534::/:/bin/false\n";
const GROUP: &str = "root:x:0:\napp:x:1000:\nstaff:x:50:app\ndocker:x:999:app,root\n";

#[test]
fn users_resolve_by_number_and_name() {
    let r = resolve_user("0", PASSWD, GROUP).unwrap();
    assert_eq!((r.uid, r.gid), (0, 0));
    let r = resolve_user("1000:1000", PASSWD, GROUP).unwrap();
    assert_eq!((r.uid, r.gid), (1000, 1000));
    let r = resolve_user("app", PASSWD, GROUP).unwrap();
    assert_eq!((r.uid, r.gid), (1000, 1000));
    let r = resolve_user("app:staff", PASSWD, GROUP).unwrap();
    assert_eq!((r.uid, r.gid), (1000, 50));
}

#[test]
fn supplementary_groups_come_from_the_group_file() {
    let r = resolve_user("app", PASSWD, GROUP).unwrap();
    let mut g = r.groups.clone();
    g.sort();
    assert_eq!(g, vec![50, 999, 1000]);
    let explicit = resolve_user("app:staff", PASSWD, GROUP).unwrap();
    assert!(explicit.groups.contains(&50));
}

#[test]
fn numeric_uids_without_passwd_entries_default_to_gid_zero() {
    let r = resolve_user("4242", PASSWD, GROUP).unwrap();
    assert_eq!((r.uid, r.gid), (4242, 0));
}

#[test]
fn home_comes_from_the_passwd_entry() {
    assert_eq!(resolve_user("app", PASSWD, GROUP).unwrap().home.as_deref(), Some("/home/app"));
    assert_eq!(resolve_user("0", PASSWD, GROUP).unwrap().home.as_deref(), Some("/root"));
    assert_eq!(resolve_user("4242", PASSWD, GROUP).unwrap().home, None);
}

#[test]
fn home_defaults_to_root_only_when_unset() {
    let c = |s: &str| std::ffi::CString::new(s).unwrap();
    let base = vec![c("PATH=/bin")];
    assert!(with_home(&base, Some("/home/app")).contains(&c("HOME=/home/app")));
    assert!(with_home(&base, None).contains(&c("HOME=/root")));
    let explicit = vec![c("HOME=/srv")];
    assert_eq!(with_home(&explicit, Some("/home/app")), explicit);
}

#[test]
fn unknown_names_are_errors() {
    assert!(resolve_user("ghost", PASSWD, GROUP).is_err());
    assert!(resolve_user("app:ghost", PASSWD, GROUP).is_err());
    assert!(resolve_user("", PASSWD, GROUP).is_err());
}

#[test]
fn merging_env_overrides_by_key_and_appends_new_keys() {
    use collocate_runtime::env::merge_env;
    let base = vec!["A=1".to_string(), "B=2".to_string()];
    let merged = merge_env(base, &[("B".into(), "9".into()), ("C".into(), "3".into())]);
    assert_eq!(merged, vec!["A=1", "B=9", "C=3"]);
}
