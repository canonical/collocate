use collocate_sys::mount::{is_mount_point, parse_mountinfo};
use collocate_sys::probe::probe;

const MOUNTINFO: &str = "22 27 0:21 / /sys rw,nosuid,nodev,noexec,relatime shared:7 - sysfs sysfs rw
23 27 0:22 / /proc rw,nosuid,nodev,noexec,relatime shared:13 - proc proc rw
27 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw,discard
100 27 0:50 / /var/lib/with\\040space rw,relatime - tmpfs tmpfs rw
";

#[test]
fn mountinfo_is_parsed() {
    let m = parse_mountinfo(MOUNTINFO);
    assert_eq!(m.len(), 4);
    assert_eq!(m[0].mount_point, "/sys");
    assert_eq!(m[0].fstype, "sysfs");
    assert_eq!(m[2].fstype, "ext4");
    assert_eq!(m[3].mount_point, "/var/lib/with space");
}

#[test]
fn mount_point_lookup() {
    let m = parse_mountinfo(MOUNTINFO);
    assert!(is_mount_point(&m, "/proc"));
    assert!(!is_mount_point(&m, "/proc/self"));
}

#[test]
fn probe_reports_each_prerequisite() {
    let report = probe();
    let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
    for expected in ["cgroup2", "cgroup-controllers", "overlayfs", "fuse-overlayfs", "clone3", "pidfd"] {
        assert!(names.contains(&expected), "{expected} missing from {names:?}");
    }
    assert!(report.check("cgroup2").unwrap().ok);
    assert!(report.check("clone3").unwrap().ok);
    assert!(report.check("pidfd").unwrap().ok);
    assert_eq!(report.all_ok(), report.checks.iter().all(|c| c.ok));
    assert!(!report.check("clone3").unwrap().detail.is_empty());
}
