use collocate_compose::convert::convert_docker_compose;
use collocate_core::policy::Strategy;
use std::path::Path;

const DOCKER: &str = r#"
name: shop
services:
  web:
    image: nginx:1.27
    ports: ["8080:80", "53:53/udp"]
    environment:
      A: "1"
      B: two
    volumes: ["./html:/usr/share/nginx/html:ro", "data:/var/lib/x"]
    depends_on:
      db:
        condition: service_healthy
    restart: unless-stopped
    cap_add: [NET_RAW]
    tmpfs: ["/tmp"]
    mem_limit: 512m
    cpus: 1.5
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost"]
      interval: 30s
      timeout: 5s
      retries: 3
    deploy:
      replicas: 3
    networks: [front]
    build: .
  db:
    image: postgres:16
    environment:
      - POSTGRES_PASSWORD=x
    command: ["postgres", "-c", "fsync=off"]
    privileged: true
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres"]
volumes:
  data: {}
networks:
  front: {}
"#;

fn convert() -> collocate_compose::convert::Converted {
    convert_docker_compose(DOCKER, "shop", Path::new("/srv/shop")).unwrap()
}

#[test]
fn services_map_to_collocate_fields() {
    let c = convert();
    assert_eq!(c.file.project, "shop");
    let web = &c.file.services["web"];
    assert_eq!(web.image.as_deref(), Some("nginx:1.27"));
    assert_eq!(web.publish, vec!["8080:80", "53:53/udp"]);
    assert_eq!(web.env["A"], "1");
    assert_eq!(web.replicas, 3);
    assert_eq!(web.memory.as_deref(), Some("512m"));
    assert_eq!(web.cpus, Some(1.5));
    assert_eq!(web.cap_add, vec!["NET_RAW"]);
    assert_eq!(web.tmpfs, vec!["/tmp"]);
    assert_eq!(web.restart.as_deref(), Some("always"));
    let db = &c.file.services["db"];
    assert_eq!(db.command, vec!["postgres", "-c", "fsync=off"]);
    assert_eq!(db.env["POSTGRES_PASSWORD"], "x");
}

#[test]
fn relative_bind_paths_become_absolute_and_named_volumes_stay_named() {
    let web = &convert().file.services["web"];
    assert!(web.volumes.contains(&"/srv/shop/html:/usr/share/nginx/html:ro".to_string()));
    assert!(web.volumes.contains(&"data:/var/lib/x".to_string()));
}

#[test]
fn healthchecks_translate_cmd_and_cmd_shell() {
    let c = convert();
    let web = c.file.services["web"].healthcheck.as_ref().unwrap();
    assert_eq!(web.exec.as_ref().unwrap(), &vec!["curl", "-f", "http://localhost"]);
    assert_eq!((web.interval.as_str(), web.retries), ("30s", 3));
    let db = c.file.services["db"].healthcheck.as_ref().unwrap();
    assert_eq!(db.exec.as_ref().unwrap(), &vec!["/bin/sh", "-c", "pg_isready -U postgres"]);
}

#[test]
fn healthy_conditions_carry_the_dependency_healthcheck() {
    let dep = &convert().file.services["web"].depends_on[0];
    assert_eq!(dep.service, "db");
    assert_eq!(dep.healthcheck, vec!["/bin/sh", "-c", "pg_isready -U postgres"]);
}

#[test]
fn report_lists_everything_that_could_not_be_translated() {
    let r = convert().report;
    let all = format!("{:?}", r);
    for needle in ["web: build", "db: privileged", "networks", "unless-stopped"] {
        assert!(all.contains(needle), "{needle} missing in {all}");
    }
    assert!(r.unsupported.iter().any(|m| m.contains("privileged")));
    assert!(r.approximated.iter().any(|m| m.contains("unless-stopped")));
}

#[test]
fn converted_output_needs_series_or_image_and_defaults_update_policy() {
    let c = convert();
    assert!(c.file.services.values().all(|s| s.image.is_some()));
    assert_eq!(c.file.services["web"].update.unwrap_or_default().strategy, Strategy::Rolling);
}

#[test]
fn invalid_yaml_and_empty_services_are_errors() {
    assert!(convert_docker_compose("services: [", "p", Path::new("/")).is_err());
    assert!(convert_docker_compose("version: '3'\n", "p", Path::new("/")).is_err());
}

#[test]
fn list_form_commands_and_string_commands_are_supported() {
    let y = "services:\n  a:\n    image: busybox\n    command: sleep 100\n    entrypoint: [\"/bin/sh\", \"-c\"]\n    ports:\n      - target: 80\n        published: 8080\n        protocol: udp\n";
    let c = convert_docker_compose(y, "p", Path::new("/")).unwrap();
    let a = &c.file.services["a"];
    assert_eq!(a.command, vec!["sleep", "100"]);
    assert_eq!(a.entrypoint, vec!["/bin/sh", "-c"]);
    assert_eq!(a.publish, vec!["8080:80/udp"]);
}
