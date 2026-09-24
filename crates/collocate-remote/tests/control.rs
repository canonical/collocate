use collocate_core::auth::{Caller, Role};
use collocate_core::request::{ContainerInfo, LogSource, Request, Response, State};
use collocate_core::spec::{ImageKind, Mount, RestartPolicy, RootSource, Series};
use collocate_core::wire::{read_frame, write_frame};
use collocate_core::{ContainerId, Error};
use collocate_image::config::{ImageConfig, ImageMeta};
use collocate_remote::client::Endpoint;
use collocate_remote::tls::server_config;
use collocate_remote::{Collocate, ExecOptions, Logs};
use collocate_sys::fdpass::recv_fd;
use collocate_trust::{generate_client, generate_server};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

fn image_meta(name: &str) -> ImageMeta {
    ImageMeta {
        name: name.into(),
        digest: "sha256:1111111111111111111111111111111111111111111111111111111111111111".into(),
        layers: vec!["sha256:2222222222222222222222222222222222222222222222222222222222222222".into()],
        config: ImageConfig {
            entrypoint: vec!["/app".into()],
            cmd: vec!["serve".into()],
            env: vec!["PORT=8080".into()],
            working_dir: "/".into(),
            user: "".into(),
            exposed: vec!["8080/tcp".into()],
            volumes: vec![],
            stop_signal: "TERM".into(),
            healthcheck: None,
        },
        kind: ImageKind::Oci,
    }
}

fn fake_daemon(path: &Path, handler: Arc<dyn Fn(Request) -> Response + Send + Sync>) {
    let listener = UnixListener::bind(path).unwrap();
    eprintln!("fake_daemon bound {path:?}");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { eprintln!("fake_daemon accept err"); continue };
            let handler = handler.clone();
            eprintln!("fake_daemon accepted a connection");
            std::thread::spawn(move || {
                while let Ok(req) = read_frame::<_, Request>(&mut s) {
                    eprintln!("fake_daemon got request {}", req.verb());
                    let resp = handler(req);
                    if write_frame(&mut s, &resp).is_err() {
                        break;
                    }
                }
            });
        }
    });
}

#[test]
fn run_builder_builds_a_rich_base_spec() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Collocate::at(dir.path().join("d.sock"));
    let mut b = c.specify("web");
    b.command(&["/sbin/init"]);
    b.cpu(2.5);
    b.ram("512Mi");
    b.ram_bytes(2048 * 1024);
    b.env("PORT", "8080");
    b.envs(&[("PORT", "9090"), ("HOST", "frontend")]);
    b.persistent();
    b.volume("/tmp:/data:ro");
    b.bind("/cfg", "/etc/cfg", true);
    b.tmpfs("/run:size=32Mi");
    b.publish("8080:80/tcp");
    b.publish_tcp(9090, 90);
    b.dns("1.1.1.1");
    b.label("tier", "frontend");
    b.project("shop");
    b.restart_always();
    b.stop_signal(9);
    let spec = b.build().unwrap();
    assert_eq!(spec.name, "web");
    assert_eq!(spec.hostname, "web");
    assert_eq!(spec.root, RootSource::Base { series: Series::Noble, build_id: "latest".into() });
    assert_eq!(spec.process.argv, vec!["/sbin/init"]);
    assert!(spec.persistent);
    assert_eq!(spec.limits.cpus_milli, Some(2500));
    assert_eq!(spec.limits.memory, Some(2048 * 1024));
    assert_eq!(spec.process.env, vec![("PORT".into(), "9090".into()), ("HOST".into(), "frontend".into())]);
    assert_eq!(spec.restart, RestartPolicy::Always);
    assert_eq!(spec.process.stop_signal, 9);
    assert_eq!(spec.labels.project.as_deref(), Some("shop"));
    assert_eq!(spec.labels.extra.get("tier").map(|s| s.as_str()), Some("frontend"));
    assert!(spec.mounts.iter().any(|m| matches!(m, Mount::Bind { src, dst, ro: true } if src == "/tmp" && dst == "/data")));
    assert!(spec.mounts.iter().any(|m| matches!(m, Mount::Bind { src, dst, ro: true } if src == "/cfg" && dst == "/etc/cfg")));
    assert!(spec.mounts.iter().any(|m| matches!(m, Mount::Tmpfs { dst, size: Some(s) } if dst == "/run" && *s == 32 * 1024 * 1024)));
    assert_eq!(spec.net.publish.len(), 2);
    assert_eq!(spec.net.publish[0].host, 8080);
    assert_eq!(spec.dns, vec![std::net::IpAddr::V4(std::net::Ipv4Addr::new(1, 1, 1, 1))]);
}

#[test]
fn run_builder_keeps_image_metadata_and_applies_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("d.sock");
    let present = image_meta("gcr.io/distroless/base:latest");
    let absent = image_meta("quay.io/team/app:v1");
    let handler: Arc<dyn Fn(Request) -> Response + Send + Sync> = Arc::new(move |req| match req {
        Request::ImageShow { name } if name == "gcr.io/distroless/base:latest" => Response::Json { value: serde_json::to_value(&present).unwrap() },
        Request::ImageShow { .. } => Response::error(&Error::NotFound("missing".into())),
        Request::ImagePull { reference, .. } if reference == "quay.io/team/app:v1" => Response::Json { value: serde_json::to_value(&absent).unwrap() },
        other => Response::error(&Error::Invalid(format!("unexpected {}", other.verb()))),
    });
    fake_daemon(&sock, handler);
    let mut c = Collocate::at(&sock);

    let mut b = c.specify("from-present");
    b.image("gcr.io/distroless/base:latest");
    b.command(&["--port", "80"]);
    b.env("PORT", "9090");
    b.persistent();
    let spec = b.build().unwrap();
    assert_eq!(spec.name, "from-present");
    assert_eq!(spec.process.argv, vec!["/app", "--port", "80"]);
    assert_eq!(spec.process.env, vec![("PORT".into(), "9090".into())]);
    assert!(matches!(spec.root, RootSource::Oci { digest, .. } if digest.starts_with("sha256:")));

    let mut b = c.specify("from-absent");
    b.image("quay.io/team/app:v1");
    b.envs(&[("PORT", "9999")]);
    b.persistent();
    let spec = b.build().unwrap();
    assert_eq!(spec.name, "from-absent");
    assert_eq!(spec.process.argv, vec!["/app", "serve"]);
    assert_eq!(spec.process.env, vec![("PORT".into(), "9999".into())]);
}

#[test]
fn run_builder_rejects_bad_strings_at_build_time() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Collocate::at(dir.path().join("d.sock"));
    let mut b = c.specify("web");
    b.command(&["/sbin/init"]);
    b.ram("12x");
    assert!(matches!(b.build(), Err(Error::InvalidSize(_))));
    let mut b = c.specify("web");
    b.command(&["/sbin/init"]);
    b.publish("nope");
    assert!(matches!(b.build(), Err(Error::InvalidPublish(_))));
    let mut b = c.specify("web");
    assert!(matches!(b.build(), Err(Error::InvalidSpec(m)) if m.contains("command")));
}

#[test]
fn local_backend_dispatches_verbs() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("d.sock");
    let id = ContainerId::from_bytes([0xab; 6]);
    let handler: Arc<dyn Fn(Request) -> Response + Send + Sync> = Arc::new(move |req| match req {
        Request::Run(_) => Response::Id { id },
        Request::Ps { .. } => Response::Containers(vec![ContainerInfo {
            id,
            name: "web".into(),
            state: State::Running,
            pid: Some(42),
            address: Some(Ipv4Addr::LOCALHOST),
            project: Some("shop".into()),
            service: None,
            health: None,
            revision: None,
            series: Some("24.04".into()),
            published: vec![],
            image_kind: None,
        }]),
        Request::Logs { .. } => Response::Log { data: "hello\nworld\n".into(), next_offset: 12, source: LogSource::Captured },
        Request::Info => Response::Text { text: "{\"ok\":true}".into() },
        Request::Stats { .. } => Response::Stats(vec![]),
        Request::Wait { .. } => Response::Exit { status: 0 },
        Request::Shutdown => Response::Ok,
        other => Response::error(&Error::Invalid(format!("unexpected {}", other.verb()))),
    });
    fake_daemon(&sock, handler);
    let mut c = Collocate::at(&sock);

    let spec = c.specify("web").command(&["/sbin/init"]).build().unwrap();
    assert_eq!(c.run(spec).unwrap(), id);
    let list = c.ps(true, Some("shop")).unwrap();
    assert_eq!(list[0].name, "web");
    assert_eq!(list[0].pid, Some(42));
    let w = c.logs("web", &Logs::new().tail(10)).unwrap();
    assert_eq!(w.data, "hello\nworld\n");
    assert_eq!(w.source, LogSource::Captured);
    assert_eq!(c.info().unwrap()["ok"], true);
    assert_eq!(c.wait("web").unwrap(), 0);
    c.shutdown().unwrap();
    assert!(c.ps(false, None).is_ok());
}

#[test]
fn exec_streams_stdin_stdout_and_stderr_over_the_unix_socket() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("d.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
std::thread::spawn(move || {
                let Ok((data, mut fds)) = recv_fd(&s, 1 << 20, 3) else { return };
                if data.len() < 4 {
                    return;
                }
                let req: Request = serde_json::from_slice(&data[4..]).unwrap();
                if let Request::Exec { argv, .. } = &req {
                    assert_eq!(argv, &vec!["echo", "hi"]);
                }
                let mut stdin_reader = std::fs::File::from(fds.remove(0));
                let mut stdout_writer = std::fs::File::from(fds.remove(0));
                let mut stderr_writer = std::fs::File::from(fds.remove(0));
                let mut buf = Vec::new();
                if stdin_reader.read_to_end(&mut buf).is_err() {
                    return;
                }
                let _ = stdout_writer.write_all(&buf);
                let _ = stdout_writer.write_all(b"ping-from-out");
                let _ = stderr_writer.write_all(b"from-err");
                let _ = write_frame(&mut s, &Response::Exit { status: 7 });
            });
        }
    });

    let mut c = Collocate::at(&sock);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let input = b"ping".to_vec();
    let code = c.exec("web", &["echo", "hi"], &ExecOptions::new(), std::io::Cursor::new(input), &mut out, &mut err).unwrap();
    assert_eq!(code, 7);
    assert_eq!(out, b"pingping-from-out");
    assert_eq!(err, b"from-err");
}

#[derive(Default)]
struct Fake {
    trusted: HashMap<String, Caller>,
    seen: Vec<(String, String)>,
}

fn remote_setup() -> (Collocate, String, Arc<Mutex<Fake>>) {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("d.sock");
    let shared = Arc::new(Mutex::new(Fake::default()));
    let handler_shared = shared.clone();
    let handler: Arc<dyn Fn(Request) -> Response + Send + Sync> = Arc::new(move |req| {
        let mut st = handler_shared.lock().unwrap();
        match req {
            Request::TrustLookup { fingerprint } => match st.trusted.get(&fingerprint) {
                Some(c) => {
                    eprintln!("daemon TrustLookup {fingerprint} matched {:?}", c.name);
                    Response::Json { value: serde_json::to_value(c).unwrap() }
                }
                None => {
                    eprintln!("daemon TrustLookup {fingerprint} missed; known {:?}", st.trusted.keys().collect::<Vec<_>>());
                    Response::error(&Error::NotFound("not trusted".into()))
                }
            },
            Request::As { caller, request } => {
                st.seen.push((caller.name.clone(), request.verb().to_string()));
                match *request {
                    Request::Info => Response::Text { text: "{\"ok\":true}".into() },
                    Request::Run(_) => Response::Id { id: ContainerId::from_bytes([0xcd; 6]) },
                    Request::Ps { .. } => Response::Containers(vec![]),
                    Request::Stats { .. } => Response::Stats(vec![]),
                    _ => Response::Ok,
                }
            }
            other => Response::error(&Error::Invalid(format!("unexpected {}", other.verb()))),
        }
    });
    fake_daemon(&sock, handler);

    let server = generate_server(&["127.0.0.1".into()]).unwrap();
    let config = server_config(&server).unwrap();
    let gateway = collocate_gateway::Gateway::new(sock, server.fingerprint.clone());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            gateway.try_serve(stream, config.clone());
        }
    });

    let identity = generate_client("ci").unwrap();
    shared.lock().unwrap().trusted.insert(
        identity.fingerprint.clone(),
        Caller { name: "ci".into(), fingerprint: identity.fingerprint.clone(), role: Role::Operator, projects: vec![] },
    );
    let c = Collocate::from_endpoint(Endpoint { addresses: vec![address.clone()], fingerprint: server.fingerprint.clone() }, identity);
    std::mem::forget(dir);
    (c, address, shared)
}

#[test]
fn from_endpoint_works_against_a_gateway_and_is_routed_as_the_client() {
    let (mut c, _address, shared) = remote_setup();
    assert_eq!(c.info().unwrap()["ok"], true);
    let id = c.specify("web").command(&["/sbin/init"]).persistent().run().unwrap();
    assert_eq!(id, ContainerId::from_bytes([0xcd; 6]));
    let list = c.ps(true, None).unwrap();
    assert!(list.is_empty());
    let seen = shared.lock().unwrap().seen.clone();
    assert_eq!(seen, vec![
        ("ci".into(), "info".into()),
        ("ci".into(), "run".into()),
        ("ci".into(), "ps".into()),
    ]);
}