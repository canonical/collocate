mod common;
use collocate_compose::model::ComposeFile;
use common::FULL;

fn load(yaml: &str) -> Result<ComposeFile, collocate_core::Error> {
    ComposeFile::load(yaml)
}

fn err(yaml: &str) -> String {
    load(yaml).unwrap_err().to_string()
}

const MINI: &str = "version: 1\nproject: p\nservices:\n  a:\n    series: \"24.04\"\n    command: [/bin/true]\n";

#[test]
fn full_example_parses_and_validates() {
    let f = load(FULL).unwrap();
    assert_eq!(f.project, "myapp");
    assert_eq!(f.services.len(), 3);
    assert_eq!(f.nodes["edge-2"].target.as_deref(), Some("host-a"));
    let server = &f.services["server"];
    assert_eq!(server.replicas, 3);
    assert_eq!(server.depends_on.len(), 2);
    assert_eq!(server.depends_on[0].service, "db");
    assert_eq!(server.depends_on[1].healthcheck, vec!["redis-cli", "ping"]);
    assert_eq!(server.autoscale.as_ref().unwrap().max, 10);
    assert_eq!(server.update.unwrap().min_ready_secs, 10);
    assert_eq!(server.healthcheck.as_ref().unwrap().tcp, Some(8080));
    assert_eq!(f.loadbalancers["web"].backends.port, 8080);
    assert_eq!(f.loadbalancers["web"].drain_secs, 30);
    assert_eq!(f.secrets["db_password"].length, Some(24));
}

#[test]
fn defaults_apply() {
    let f = load(MINI).unwrap();
    let a = &f.services["a"];
    assert_eq!(a.replicas, 1);
    assert!(!a.persistent);
    assert!(a.depends_on.is_empty());
}

#[test]
fn unknown_fields_are_rejected() {
    let y = format!("{MINI}    bogus: 1\n");
    assert!(ComposeFile::parse(&y).is_err());
}

#[test]
fn unsupported_version_is_rejected() {
    assert!(err(&MINI.replace("version: 1", "version: 9")).contains("version"));
}

#[test]
fn series_and_image_are_mutually_exclusive_and_one_is_required() {
    let both = MINI.replace("series: \"24.04\"", "series: \"24.04\"\n    image: redis:7");
    assert!(err(&both).contains("series"));
    let neither = MINI.replace("    series: \"24.04\"\n", "");
    assert!(err(&neither).contains("series"));
}

#[test]
fn dependency_cycles_are_reported_with_the_cycle() {
    let y = "version: 1\nproject: p\nservices:\n  a:\n    series: \"24.04\"\n    depends_on: [b]\n  b:\n    series: \"24.04\"\n    depends_on: [a]\n";
    let e = err(y);
    assert!(e.contains("cycle") && e.contains('a') && e.contains('b'), "{e}");
}

#[test]
fn unknown_references_are_rejected() {
    let dep = MINI.replace("    command", "    depends_on: [ghost]\n    command");
    assert!(err(&dep).contains("ghost"));
    let sec = MINI.replace("    command", "    secrets: [nope]\n    command");
    assert!(err(&sec).contains("nope"));
    let node = MINI.replace("    command", "    node: nowhere\n    command");
    assert!(err(&node).contains("nowhere"));
    let cfg = MINI.replace("    command", "    configs:\n      missing: /x\n    command");
    assert!(err(&cfg).contains("missing"));
}

#[test]
fn template_references_must_resolve_to_declared_things() {
    let y = MINI.replace("    command", "    env:\n      X: \"${secrets.absent}\"\n    command");
    assert!(err(&y).contains("absent"));
    let y = MINI.replace("    command", "    env:\n      X: \"${services.absent.address}\"\n    command");
    assert!(err(&y).contains("absent"));
    let y = MINI.replace("    command", "    env:\n      X: \"${loadbalancers.absent.address}\"\n    command");
    assert!(err(&y).contains("absent"));
}

#[test]
fn replicas_and_autoscale_are_validated() {
    let zero = MINI.replace("    command", "    replicas: 0\n    command");
    assert!(err(&zero).contains("replicas"));
    let base = "    memory: 256m\n    autoscale:\n      min: 1\n      max: 3\n      metrics:\n        - type: cpu\n          target: 50\n";
    let ok = MINI.replace("    command", &format!("{base}    command"));
    assert!(load(&ok).is_ok());
    let inverted = ok.replace("min: 1", "min: 5");
    assert!(err(&inverted).contains("autoscale"));
    let no_mem = ok.replace("    memory: 256m\n", "");
    assert!(err(&no_mem).contains("memory"));
    let persistent = ok.replace("    memory: 256m\n", "    memory: 256m\n    persistent: true\n");
    assert!(err(&persistent).contains("persistent"));
    let no_metrics = ok.replace("      metrics:\n        - type: cpu\n          target: 50\n", "      metrics: []\n");
    assert!(err(&no_metrics).contains("metrics"));
}

#[test]
fn load_balancers_reference_real_services_and_unique_ports() {
    let y = format!("{MINI}loadbalancers:\n  web:\n    listen: 80\n    backends:\n      service: ghost\n      port: 80\n");
    assert!(err(&y).contains("ghost"));
    let dup = format!(
        "{MINI}loadbalancers:\n  x:\n    listen: 80\n    backends: {{service: a, port: 80}}\n  y:\n    listen: 80\n    backends: {{service: a, port: 81}}\n"
    );
    assert!(err(&dup).contains("listen"));
}

#[test]
fn duplicate_published_host_ports_on_a_node_conflict() {
    let y = "version: 1\nproject: p\nservices:\n  a:\n    series: \"24.04\"\n    publish: [\"80:80\"]\n  b:\n    series: \"24.04\"\n    publish: [\"80:8080\"]\n";
    assert!(err(y).contains("80"));
}

#[test]
fn bad_field_values_are_rejected() {
    let y = MINI.replace("    command", "    publish: [\"abc\"]\n    command");
    assert!(load(&y).is_err());
    let y = MINI.replace("    command", "    memory: lots\n    command");
    assert!(load(&y).is_err());
    let y = MINI.replace("    command", "    volume: relative:/x\n    volume2: 1\n    command");
    assert!(ComposeFile::parse(&y).is_err());
}

#[test]
fn pull_policy_is_validated_and_only_applies_to_images() {
    let img = "version: 1\nproject: p\nservices:\n  a:\n    image: redis:7\n";
    for ok in ["missing", "always", "never"] {
        assert!(load(&format!("{img}    pull_policy: {ok}\n")).is_ok(), "{ok}");
    }
    assert!(load(&format!("{img}    pull_policy: daily\n")).is_err());
    assert!(load(&format!("{MINI}    pull_policy: always\n")).is_err());
}
