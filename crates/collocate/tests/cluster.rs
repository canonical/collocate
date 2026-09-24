use collocate_cluster::transport::LxcApi;
use collocate_core::client::Api;
use collocate_core::request::{Request, Response};
use collocate_core::wire::{read_frame, write_frame};
use collocate_core::Error;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;

const RELAY: &str = env!("CARGO_BIN_EXE_collocate-relay");

fn stub_lxc(dir: &std::path::Path, sock: &std::path::Path) -> String {
    let path = dir.join("lxc");
    std::fs::write(&path, format!("#!/bin/sh\nshift 3\nexec env COLLOCATE_HOST={} \"$@\"\n", sock.display())).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

fn fake_daemon(sock: &std::path::Path) -> std::thread::JoinHandle<()> {
    let listener = UnixListener::bind(sock).unwrap();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        while let Ok(req) = read_frame::<_, Request>(&mut s) {
            let resp = match req {
                Request::Info => Response::Text { text: "{\"node\":\"edge-1\"}".into() },
                Request::Ps { .. } => Response::Containers(vec![]),
                Request::Kill { .. } => Response::error(&Error::NotFound("ghost".into())),
                _ => Response::Ok,
            };
            if write_frame(&mut s, &resp).is_err() {
                break;
            }
        }
    })
}

#[test]
fn requests_travel_through_lxc_exec_and_the_relay() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("d.sock");
    let server = fake_daemon(&sock);
    let lxc = stub_lxc(dir.path(), &sock);
    let mut api = LxcApi::spawn(&lxc, "edge-1", &[RELAY.to_string()]).unwrap();
    assert_eq!(api.call(Request::Info).unwrap(), Response::Text { text: "{\"node\":\"edge-1\"}".into() });
    assert_eq!(api.call(Request::Ps { all: true, project: None }).unwrap(), Response::Containers(vec![]));
    assert!(matches!(api.call(Request::Kill { target: "x".into(), signal: 9 }), Err(Error::NotFound(_))));
    drop(api);
    let _ = server.join();
}

#[test]
fn a_missing_lxc_binary_is_reported_as_unreachable() {
    assert!(matches!(LxcApi::spawn("/nonexistent/lxc", "n", &[]), Err(Error::Unreachable(_))));
}

#[test]
fn a_relay_that_cannot_reach_the_daemon_surfaces_an_unreachable_error() {
    let dir = tempfile::tempdir().unwrap();
    let lxc = stub_lxc(dir.path(), &dir.path().join("absent.sock"));
    let mut api = LxcApi::spawn(&lxc, "edge-1", &[RELAY.to_string()]).unwrap();
    assert!(matches!(api.call(Request::Info), Err(Error::Unreachable(_))));
}

#[test]
fn cluster_subcommands_parse() {
    use clap::Parser;
    for args in [
        vec!["collocate", "cluster", "list"],
        vec!["collocate", "cluster", "up", "-f", "c.yaml", "--install", "deb:./c.deb"],
        vec!["collocate", "cluster", "status", "-f", "c.yaml", "--lxc", "/usr/bin/lxc"],
        vec!["collocate", "cluster", "add-node", "edge-3", "--target", "lxd2", "--memory", "2g"],
        vec!["collocate", "cluster", "remove-node", "edge-3", "--keep-instance"],
        vec!["collocate", "init", "--auto", "--mode", "lxd", "--nodes", "2", "--install", "channel:latest/edge"],
        vec!["collocate", "init", "--preseed"],
        vec!["collocate", "init", "--dump", "--format", "json"],
        vec!["collocate", "up", "--managed"],
    ] {
        collocate_cli::cli::Cli::try_parse_from(args.clone()).unwrap_or_else(|e| panic!("{args:?}: {e}"));
    }
    for args in [
        vec!["collocate", "cluster", "up", "--deb", "./c.deb"],
        vec!["collocate", "init", "--auto", "--preseed"],
        vec!["collocate", "init", "--lxd-url", "https://x"],
        vec!["collocate", "init", "--mode", "cloud"],
    ] {
        assert!(collocate_cli::cli::Cli::try_parse_from(args.clone()).is_err(), "{args:?} should not parse");
    }
}
