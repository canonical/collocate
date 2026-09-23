mod common;
mod fake;
use collocate_compose::model::ComposeFile;
use collocate_compose::up::{down, plan_only, up, UpOptions};
use collocate_core::request::State;
use collocate_core::Error;
use fake::FakeDaemon;
use std::time::Duration;

fn opts() -> UpOptions {
    UpOptions {
        subnet: "172.30.0.0/16".into(),
        base_dir: std::env::temp_dir(),
        state_dir: std::env::temp_dir(),
        regenerate_secrets: None,
        dry_run: false,
        ready_timeout: Duration::from_millis(300),
        poll_interval: Duration::from_millis(1),
    }
}

const STACK: &str = r#"
version: 1
project: shop
services:
  db:
    series: "24.04"
    command: ["/bin/db"]
    env:
      PASSWORD: ${secrets.pw}
    secrets: [pw]
  cache:
    series: "24.04"
    command: ["/bin/cache"]
  server:
    series: "24.04"
    depends_on: [db, cache]
    command: ["/bin/server"]
    replicas: 2
    env:
      DB: "${services.db.address}"
      CACHE: "${services.cache.address}"
      FRONT: "${loadbalancers.web.address}"
    healthcheck:
      tcp: 8080
secrets:
  pw:
    generate: password
    length: 16
loadbalancers:
  web:
    listen: 80
    backends:
      service: server
      port: 8080
"#;

fn file() -> ComposeFile {
    ComposeFile::load(STACK).unwrap()
}

fn run_names(d: &FakeDaemon) -> Vec<String> {
    d.log.iter().filter_map(|l| l.strip_prefix("run ").map(String::from)).collect()
}

#[test]
fn up_creates_services_in_dependency_order_with_secrets_ensured_first() {
    let mut d = FakeDaemon::default();
    let report = up(&mut d, &file(), &opts()).unwrap();
    let runs = run_names(&d);
    let pos = |n: &str| runs.iter().position(|r| r == n).unwrap_or_else(|| panic!("{n} missing in {runs:?}"));
    assert!(pos("shop-db-1") < pos("shop-server-1"));
    assert!(pos("shop-cache-1") < pos("shop-server-1"));
    assert_eq!(runs.len(), 4);
    assert!(d.log.iter().position(|l| l == "secret-ensure pw").unwrap() < d.log.iter().position(|l| l == "run shop-db-1").unwrap());
    assert_eq!(report.created.len(), 4);
}

#[test]
fn addresses_are_deterministic_and_injected() {
    let mut d1 = FakeDaemon::default();
    let mut d2 = FakeDaemon::default();
    up(&mut d1, &file(), &opts()).unwrap();
    up(&mut d2, &file(), &opts()).unwrap();
    let addr = |d: &FakeDaemon, n: &str| d.containers.iter().find(|(c, _)| c.name == n).unwrap().0.net.addr.unwrap();
    assert_eq!(addr(&d1, "shop-db-1"), addr(&d2, "shop-db-1"));
    let server = &d1.containers.iter().find(|(c, _)| c.name == "shop-server-1").unwrap().0;
    let env: std::collections::HashMap<_, _> = server.process.env.iter().cloned().collect();
    assert_eq!(env["DB"], addr(&d1, "shop-db-1").to_string());
    assert_eq!(env["CACHE"], addr(&d1, "shop-cache-1").to_string());
    assert_eq!(env["FRONT"], d1.lbs[0].vip.unwrap().to_string());
}

#[test]
fn secrets_reach_the_env_and_are_never_logged() {
    let mut d = FakeDaemon::default();
    up(&mut d, &file(), &opts()).unwrap();
    let db = &d.containers.iter().find(|(c, _)| c.name == "shop-db-1").unwrap().0;
    assert!(db.process.env.contains(&("PASSWORD".into(), "generated-pw".into())));
    assert!(!d.log.iter().any(|l| l.contains("generated-pw")));
}

#[test]
fn load_balancer_is_registered_after_its_backends_exist() {
    let mut d = FakeDaemon::default();
    up(&mut d, &file(), &opts()).unwrap();
    assert_eq!(d.lbs.len(), 1);
    assert_eq!(d.lbs[0].backend_service, "server");
    let lb_at = d.log.iter().position(|l| l == "lb-set web").unwrap();
    let last_server = d.log.iter().rposition(|l| l.starts_with("run shop-server")).unwrap();
    assert!(lb_at > last_server);
}

#[test]
fn a_second_up_changes_nothing() {
    let mut d = FakeDaemon::default();
    up(&mut d, &file(), &opts()).unwrap();
    let before = d.log.len();
    let report = up(&mut d, &file(), &opts()).unwrap();
    assert!(report.created.is_empty() && report.recreated.is_empty() && report.removed.is_empty());
    assert_eq!(report.kept.len(), 4);
    assert!(!d.log[before..].iter().any(|l| l.starts_with("run") || l.starts_with("stop") || l.starts_with("rm")));
}

#[test]
fn changed_services_are_recreated_and_others_untouched() {
    let mut d = FakeDaemon::default();
    up(&mut d, &file(), &opts()).unwrap();
    let changed = ComposeFile::load(&STACK.replace("/bin/cache", "/bin/cache2")).unwrap();
    let before = d.log.len();
    let report = up(&mut d, &changed, &opts()).unwrap();
    assert_eq!(report.recreated, vec!["shop-cache-1"]);
    let tail = &d.log[before..];
    assert!(
        tail.contains(&"stop shop-cache-1".to_string())
            && tail.contains(&"rm shop-cache-1".to_string())
            && tail.contains(&"run shop-cache-1".to_string())
    );
    assert!(!tail.iter().any(|l| l.contains("shop-db-1")));
}

#[test]
fn stopped_containers_are_started_not_recreated() {
    let mut d = FakeDaemon::default();
    up(&mut d, &file(), &opts()).unwrap();
    for (c, st) in &mut d.containers {
        if c.name == "shop-cache-1" {
            *st = State::Stopped;
        }
    }
    let report = up(&mut d, &file(), &opts()).unwrap();
    assert_eq!(report.started, vec!["shop-cache-1"]);
    assert!(d.running_names().contains(&"shop-cache-1".to_string()));
}

#[test]
fn removed_services_are_torn_down() {
    let mut d = FakeDaemon::default();
    up(&mut d, &file(), &opts()).unwrap();
    let smaller = STACK
        .replace("  cache:\n    series: \"24.04\"\n    command: [\"/bin/cache\"]\n", "")
        .replace("depends_on: [db, cache]", "depends_on: [db]")
        .replace("      CACHE: \"${services.cache.address}\"\n", "");
    let report = up(&mut d, &ComposeFile::load(&smaller).unwrap(), &opts()).unwrap();
    assert_eq!(report.removed, vec!["shop-cache-1"]);
    assert!(!d.running_names().contains(&"shop-cache-1".to_string()));
}

#[test]
fn dry_run_and_plan_only_change_nothing() {
    let mut d = FakeDaemon::default();
    let plan = plan_only(&mut d, &file(), &opts()).unwrap();
    assert_eq!(plan.create.len(), 4);
    let mut o = opts();
    o.dry_run = true;
    let report = up(&mut d, &file(), &o).unwrap();
    assert_eq!(report.created.len(), 4);
    assert!(d.containers.is_empty());
    assert!(d.log.iter().all(|l| !l.starts_with("run") && !l.starts_with("secret-ensure")));
}

#[test]
fn readiness_gates_dependents_and_times_out_with_the_service_name() {
    let mut d = FakeDaemon::default();
    let f =
        ComposeFile::load(&STACK.replace("command: [\"/bin/db\"]", "command: [\"/bin/db\"]\n    healthcheck:\n      tcp: 5432")).unwrap();
    d.health_after_polls.insert("shop-db-1".into(), 1_000_000);
    let err = up(&mut d, &f, &opts()).unwrap_err();
    assert!(matches!(err, Error::Timeout(ref m) if m.contains("db")), "{err:?}");
    assert!(!run_names(&d).iter().any(|r| r.starts_with("shop-server")));
}

#[test]
fn dependents_start_once_health_arrives() {
    let mut d = FakeDaemon::default();
    let f =
        ComposeFile::load(&STACK.replace("command: [\"/bin/db\"]", "command: [\"/bin/db\"]\n    healthcheck:\n      tcp: 5432")).unwrap();
    d.health_after_polls.insert("shop-db-1".into(), 3);
    up(&mut d, &f, &opts()).unwrap();
    assert!(d.running_names().contains(&"shop-server-1".to_string()));
}

fn stack_with_cache_healthcheck_override() -> ComposeFile {
    ComposeFile::load(&STACK.replace(
        "depends_on: [db, cache]",
        "depends_on:\n      - db\n      - service: cache\n        healthcheck: [\"redis-cli\", \"ping\"]",
    ))
    .unwrap()
}

#[test]
fn dependency_healthcheck_override_gates_readiness_without_its_own_healthcheck() {
    let mut d = FakeDaemon::default();
    let f = stack_with_cache_healthcheck_override();
    d.probe_ready_after.insert("shop-cache-1".into(), 1_000_000);
    let err = up(&mut d, &f, &opts()).unwrap_err();
    assert!(matches!(err, Error::Timeout(ref m) if m.contains("cache")), "{err:?}");
    assert!(!run_names(&d).iter().any(|r| r.starts_with("shop-server")));
}

#[test]
fn dependency_healthcheck_override_passes_once_the_probe_succeeds() {
    let mut d = FakeDaemon::default();
    let f = stack_with_cache_healthcheck_override();
    d.probe_ready_after.insert("shop-cache-1".into(), 2);
    up(&mut d, &f, &opts()).unwrap();
    assert!(d.running_names().contains(&"shop-server-1".to_string()));
}

#[test]
fn launch_failures_abort_and_report_the_error() {
    let mut d = FakeDaemon::default();
    d.fail_run = Some("boom".into());
    assert!(up(&mut d, &file(), &opts()).is_err());
}

#[test]
fn regenerating_secrets_rotates_and_relaunches_consumers() {
    let mut d = FakeDaemon::default();
    up(&mut d, &file(), &opts()).unwrap();
    let mut o = opts();
    o.regenerate_secrets = Some(vec![]);
    d.secrets.insert("shop/pw".into(), "old-value".into());
    let report = up(&mut d, &file(), &o).unwrap();
    assert!(d.log.contains(&"secret-rm pw".to_string()));
    assert!(report.recreated.contains(&"shop-db-1".to_string()), "{report:?}");
    assert_eq!(d.secrets["shop/pw"], "generated-pw");
}

#[test]
fn file_secrets_are_imported_and_only_updated_when_changed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("license.txt"), "KEY-123\n").unwrap();
    let yaml = "version: 1\nproject: p\nservices:\n  a:\n    series: \"24.04\"\n    command: [/bin/a]\n    env:\n      LICENSE: ${secrets.license}\nsecrets:\n  license:\n    file: ./license.txt\n";
    let f = ComposeFile::load(yaml).unwrap();
    let mut o = opts();
    o.base_dir = dir.path().to_path_buf();
    let mut d = FakeDaemon::default();
    up(&mut d, &f, &o).unwrap();
    assert_eq!(d.secrets["p/license"], "KEY-123");
    let sets = d.log.iter().filter(|l| *l == "secret-set license").count();
    up(&mut d, &f, &o).unwrap();
    assert_eq!(d.log.iter().filter(|l| *l == "secret-set license").count(), sets);
}

#[test]
fn down_stops_and_removes_in_reverse_dependency_order() {
    let mut d = FakeDaemon::default();
    up(&mut d, &file(), &opts()).unwrap();
    let removed = down(&mut d, &file(), &opts()).unwrap();
    assert_eq!(removed.len(), 4);
    assert!(d.containers.is_empty());
    let stop_at = |n: &str| d.log.iter().position(|l| l == &format!("stop {n}")).unwrap();
    assert!(stop_at("shop-server-1") < stop_at("shop-db-1"));
    assert!(stop_at("shop-server-2") < stop_at("shop-cache-1"));
    assert!(d.lbs.is_empty());
}

#[test]
fn plan_after_up_reports_no_changes_even_with_secrets() {
    let mut d = FakeDaemon::default();
    up(&mut d, &file(), &opts()).unwrap();
    let plan = plan_only(&mut d, &file(), &opts()).unwrap();
    assert!(plan.is_noop(), "{plan:?}");
    assert_eq!(plan.keep.len(), 4);
}
