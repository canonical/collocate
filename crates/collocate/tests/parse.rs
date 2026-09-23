use clap::Parser;
use collocate_cli::cli::{Cli, Command};
use collocate_cli::output::{Format, Verbosity};
use collocate_cli::runspec::build_spec;
use collocate_core::net::Proto;
use collocate_core::spec::{Mount, RestartPolicy, RootSource, Series};

fn image_lookup(name: &str) -> collocate_core::Result<collocate_image::config::ImageMeta> {
    if name == "myapp:1" {
        collocate_image::config::parse_config(
            name,
            "sha256:cfg",
            r#"{"architecture":"amd64","os":"linux","config":{"Entrypoint":["/entry"],"Cmd":["serve"],"Env":["A=1"],"ExposedPorts":{"8080/tcp":{}}},"rootfs":{"type":"layers","diff_ids":["sha256:l1"]}}"#,
        )
    } else {
        Err(collocate_core::Error::NotFound(format!("image {name}")))
    }
}

fn run(args: &[&str]) -> collocate_core::Result<collocate_core::spec::Spec> {
    let mut full = vec!["collocate", "run"];
    full.extend_from_slice(args);
    let cli = Cli::try_parse_from(full).unwrap();
    match cli.command {
        Command::Run(a) => build_spec(
            &a,
            Series::Noble,
            &|k| if k == "HOSTVAR" { Some("fromhost".into()) } else { None },
            &|_| Ok(String::new()),
            &image_lookup,
        ),
        _ => panic!("not a run command"),
    }
}

#[test]
fn documented_example_maps_onto_the_spec() {
    let s = run(&[
        "--series",
        "24.04",
        "--name",
        "web1",
        "--persistent",
        "--cpus",
        "1.5",
        "--memory",
        "512m",
        "-v",
        "/srv/web1:/data",
        "-p",
        "8080:80",
        "--",
        "/usr/bin/myserver",
        "--listen",
        "80",
    ])
    .unwrap();
    assert_eq!(s.name, "web1");
    assert!(s.persistent);
    assert_eq!(s.limits.cpus_milli, Some(1500));
    assert_eq!(s.limits.memory, Some(512 << 20));
    assert_eq!(s.limits.swap, None);
    assert_eq!(s.process.argv, vec!["/usr/bin/myserver", "--listen", "80"]);
    assert_eq!(s.net.publish[0].host, 8080);
    assert_eq!(s.net.publish[0].proto, Proto::Tcp);
    assert!(s.mounts.contains(&Mount::Bind { src: "/srv/web1".into(), dst: "/data".into(), ro: false }));
    match s.root {
        RootSource::Base { series, ref build_id } => {
            assert_eq!(series, Series::Noble);
            assert_eq!(build_id, "latest");
        }
        _ => panic!(),
    }
}

#[test]
fn series_defaults_to_the_host_series_and_can_be_overridden() {
    let s = run(&["--", "/bin/true"]).unwrap();
    assert!(matches!(s.root, RootSource::Base { series: Series::Noble, .. }));
    let s = run(&["--series", "26.04", "--", "/bin/true"]).unwrap();
    assert!(matches!(s.root, RootSource::Base { series: Series::Resolute, .. }));
    assert!(run(&["--series", "20.04", "--", "/bin/true"]).is_err());
}

#[test]
fn environment_sources_are_merged_in_order() {
    let s = run(&["-e", "A=1", "-e", "HOSTVAR", "-e", "A=2", "--", "/bin/true"]).unwrap();
    let env: std::collections::HashMap<_, _> = s.process.env.iter().cloned().collect();
    assert_eq!(env["A"], "2");
    assert_eq!(env["HOSTVAR"], "fromhost");
    assert_eq!(s.process.env.iter().filter(|(k, _)| k == "A").count(), 1);
}

#[test]
fn env_files_are_read_and_comments_skipped() {
    let mut full = vec!["collocate", "run", "--env-file", "vars.env", "--", "/bin/true"];
    let cli = Cli::try_parse_from(full.drain(..)).unwrap();
    let Command::Run(a) = cli.command else { panic!() };
    let s = build_spec(&a, Series::Noble, &|_| None, &|_| Ok("# comment\nX=1\n\nY=two words\n".into()), &image_lookup).unwrap();
    let env: std::collections::HashMap<_, _> = s.process.env.iter().cloned().collect();
    assert_eq!(env["X"], "1");
    assert_eq!(env["Y"], "two words");
}

#[test]
fn hardening_and_process_options() {
    let s = run(&[
        "--read-only",
        "--cap-add",
        "NET_RAW",
        "--cap-drop",
        "CHOWN",
        "--tmpfs",
        "/tmp:size=64m",
        "--restart",
        "on-failure:3",
        "-u",
        "app",
        "-w",
        "/srv",
        "--hostname",
        "box",
        "--stop-signal",
        "SIGINT",
        "--stop-timeout",
        "30",
        "--dns",
        "1.1.1.1",
        "--pids-max",
        "50",
        "--",
        "/bin/true",
    ])
    .unwrap();
    assert!(s.read_only_rootfs);
    assert_eq!(s.caps.add, vec!["NET_RAW"]);
    assert_eq!(s.caps.drop, vec!["CHOWN"]);
    assert!(s.mounts.contains(&Mount::Tmpfs { dst: "/tmp".into(), size: Some(64 << 20) }));
    assert_eq!(s.restart, RestartPolicy::OnFailure { max: 3 });
    assert_eq!((s.process.user.as_str(), s.process.workdir.as_str(), s.hostname.as_str()), ("app", "/srv", "box"));
    assert_eq!((s.process.stop_signal, s.process.stop_timeout_secs), (2, 30));
    assert_eq!(s.dns[0].to_string(), "1.1.1.1");
    assert_eq!(s.limits.pids_max, 50);
}

#[test]
fn secrets_and_named_volumes() {
    let s =
        run(&["--project", "app", "--secret", "pw", "--secret", "key:/etc/key", "-v", "pgdata:/var/lib/pg", "--", "/bin/true"]).unwrap();
    assert!(s.mounts.contains(&Mount::Secret { name: "pw".into(), dst: "/run/secrets/pw".into() }));
    assert!(s.mounts.contains(&Mount::Secret { name: "key".into(), dst: "/etc/key".into() }));
    assert!(s.mounts.contains(&Mount::Volume { name: "pgdata".into(), dst: "/var/lib/pg".into() }));
    assert_eq!(s.labels.project.as_deref(), Some("app"));
}

#[test]
fn invalid_values_are_rejected_with_usage_errors() {
    for bad in [
        &["--memory", "lots", "--", "/bin/true"][..],
        &["-p", "abc", "--", "/bin/true"],
        &["-v", "nocolon", "--", "/bin/true"],
        &["--restart", "sometimes", "--", "/bin/true"],
        &["--cap-add", "BOGUS", "--", "/bin/true"],
    ] {
        assert!(run(bad).is_err(), "{bad:?}");
    }
    assert!(run(&[]).is_err());
}

#[test]
fn unknown_images_are_reported_as_not_found() {
    let e = run(&["--image", "nope:1"]).unwrap_err();
    assert!(matches!(e, collocate_core::Error::NotFound(_)), "{e}");
}

#[test]
fn images_supply_the_command_environment_and_root() {
    let s = run(&["--image", "myapp:1", "--name", "o1"]).unwrap();
    assert_eq!(s.process.argv, vec!["/entry", "serve"]);
    assert!(matches!(s.root, RootSource::Oci { .. }));
    let env: std::collections::HashMap<_, _> = s.process.env.iter().cloned().collect();
    assert_eq!(env["A"], "1");
    let s = run(&["--image", "myapp:1", "-e", "A=2", "--memory", "64m", "--", "other"]).unwrap();
    assert_eq!(s.process.argv, vec!["/entry", "other"]);
    assert_eq!(s.limits.memory, Some(64 << 20));
    assert!(s.process.env.contains(&("A".to_string(), "2".to_string())));
}

#[test]
fn entrypoint_override_and_exposed_port_publishing() {
    let s = run(&["--image", "myapp:1", "--entrypoint", "/bin/sh -c", "--", "id"]).unwrap();
    assert_eq!(s.process.argv, vec!["/bin/sh", "-c", "id"]);
    let s = run(&["--image", "myapp:1", "--publish-exposed"]).unwrap();
    assert_eq!(s.net.publish[0].to_string(), "8080:8080/tcp");
}

#[test]
fn series_and_image_are_mutually_exclusive() {
    assert!(Cli::try_parse_from(["collocate", "run", "--image", "myapp:1", "--series", "24.04"]).is_err());
}

fn parses(args: &[&str]) -> Cli {
    Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"))
}

#[test]
fn other_subcommands_parse() {
    for args in [
        vec!["collocate", "list", "-a"],
        vec!["collocate", "stop", "a", "b", "-t", "5"],
        vec!["collocate", "kill", "a", "-s", "TERM"],
        vec!["collocate", "delete", "-f", "a"],
        vec!["collocate", "logs", "-f", "--tail", "10", "a"],
        vec!["collocate", "exec", "-e", "K=V", "-u", "root", "a", "--", "ls", "-l"],
        vec!["collocate", "cp", "a.txt", "c:/b.txt"],
        vec!["collocate", "status", "--watch", "--json"],
        vec!["collocate", "secret", "list"],
        vec!["collocate", "secret", "set", "app", "pw", "--from-file", "-"],
        vec!["collocate", "load-balancer", "list"],
        vec!["collocate", "up", "-f", "x.yaml", "--regenerate-secrets", "pw", "--dry-run"],
        vec!["collocate", "down", "-f", "x.yaml"],
        vec!["collocate", "plan"],
        vec!["collocate", "config", "--from-docker-compose", "d.yml", "--project", "p"],
        vec!["collocate", "info"],
        vec!["collocate", "image", "import", "app.tar"],
        vec!["collocate", "image", "list"],
        vec!["collocate", "image", "show", "app:1"],
        vec!["collocate", "image", "delete", "app:1"],
        vec!["collocate", "image", "prune"],
        vec!["collocate", "--state-dir", "/tmp/s", "image", "list"],
        vec!["collocate", "doctor"],
        vec!["collocate", "--host", "/tmp/x.sock", "--format", "json", "list"],
        vec!["collocate", "-y", "delete", "a"],
        vec!["collocate", "--yes", "delete", "a"],
        vec!["collocate", "-q", "delete", "a"],
        vec!["collocate", "--verbose", "--verbose", "--verbose", "list"],
        vec!["collocate", "--verbosity", "trace", "list"],
        vec!["collocate", "list", "--columns", "NAME,STATE", "--no-headers", "--no-truncate"],
        vec!["collocate", "cluster", "list"],
        vec!["collocate", "run", "--label", "team=platform", "--", "/bin/true"],
        vec!["collocate", "completion", "bash"],
        vec!["collocate", "completion", "zsh"],
        vec!["collocate", "completion", "fish"],
        vec!["collocate", "node", "reinit"],
        vec!["collocate", "node", "adopt"],
    ] {
        parses(&args);
    }
}

#[test]
fn label_is_rejected_without_an_equals_sign() {
    assert!(run(&["--label", "noequals", "--", "/bin/true"]).is_err());
}

#[test]
fn label_populates_the_extra_labels_map() {
    let s = run(&["--label", "team=platform", "--label", "tier=web", "--", "/bin/true"]).unwrap();
    assert_eq!(s.labels.extra.get("team").map(String::as_str), Some("platform"));
    assert_eq!(s.labels.extra.get("tier").map(String::as_str), Some("web"));
}

#[test]
fn list_has_the_ls_and_ps_aliases() {
    for args in [vec!["collocate", "list"], vec!["collocate", "ls"], vec!["collocate", "ps"]] {
        assert!(matches!(parses(&args).command, Command::List { .. }), "{args:?}");
    }
}

#[test]
fn delete_has_the_rm_alias() {
    for args in [vec!["collocate", "delete", "a"], vec!["collocate", "rm", "a"]] {
        assert!(matches!(parses(&args).command, Command::Delete { .. }), "{args:?}");
    }
}

#[test]
fn secret_list_and_delete_have_short_aliases() {
    assert!(Cli::try_parse_from(["collocate", "secret", "ls"]).is_ok());
    assert!(Cli::try_parse_from(["collocate", "secret", "rm", "app", "pw"]).is_ok());
}

#[test]
fn load_balancer_has_the_lb_alias_and_a_short_form_for_its_subcommands() {
    for args in [vec!["collocate", "load-balancer", "list"], vec!["collocate", "lb", "list"], vec!["collocate", "lb", "ls"]] {
        assert!(matches!(parses(&args).command, Command::LoadBalancer(_)), "{args:?}");
    }
}

#[test]
fn load_balancer_create_and_its_update_alias_parse_the_same_fields() {
    let create =
        ["collocate", "load-balancer", "create", "shop", "web", "--listen", "80", "--backend-service", "server", "--backend-port", "8080"];
    let update =
        ["collocate", "load-balancer", "update", "shop", "web", "--listen", "80", "--backend-service", "server", "--backend-port", "8080"];
    assert!(Cli::try_parse_from(create).is_ok());
    assert!(Cli::try_parse_from(update).is_ok());
}

#[test]
fn load_balancer_delete_has_the_rm_alias() {
    assert!(Cli::try_parse_from(["collocate", "lb", "delete", "shop", "web"]).is_ok());
    assert!(Cli::try_parse_from(["collocate", "lb", "rm", "shop", "web"]).is_ok());
}

#[test]
fn image_delete_has_the_rm_alias_and_list_has_ls() {
    assert!(Cli::try_parse_from(["collocate", "image", "rm", "app:1"]).is_ok());
    assert!(Cli::try_parse_from(["collocate", "image", "ls"]).is_ok());
}

#[test]
fn cluster_list_replaces_nodes() {
    assert!(Cli::try_parse_from(["collocate", "cluster", "list"]).is_ok());
    assert!(Cli::try_parse_from(["collocate", "cluster", "nodes"]).is_err());
}

#[test]
fn image_gc_is_gone_in_favour_of_prune() {
    assert!(Cli::try_parse_from(["collocate", "image", "prune"]).is_ok());
    assert!(Cli::try_parse_from(["collocate", "image", "gc"]).is_err());
}

#[test]
fn format_flag_replaces_output() {
    let cli = parses(&["collocate", "--format", "json", "list"]);
    assert_eq!(cli.format, Format::Json);
    assert!(Cli::try_parse_from(["collocate", "-o", "json", "list"]).is_err());
    assert!(Cli::try_parse_from(["collocate", "--output", "json", "list"]).is_err());
}

#[test]
fn verbosity_flags_resolve_through_the_cli() {
    assert_eq!(parses(&["collocate", "list"]).verbosity(), Verbosity::Brief);
    assert_eq!(parses(&["collocate", "-q", "list"]).verbosity(), Verbosity::Quiet);
    assert_eq!(parses(&["collocate", "--verbose", "list"]).verbosity(), Verbosity::Verbose);
    assert_eq!(parses(&["collocate", "--verbose", "--verbose", "list"]).verbosity(), Verbosity::Debug);
    assert_eq!(parses(&["collocate", "--verbosity", "trace", "list"]).verbosity(), Verbosity::Trace);
}

#[test]
fn yes_flag_defaults_to_false() {
    assert!(!parses(&["collocate", "list"]).yes);
    assert!(parses(&["collocate", "-y", "list"]).yes);
}
