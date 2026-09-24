use collocate_core::auth::{Caller, Role};
use collocate_core::client::Api;
use collocate_core::request::{Request, Response};
use collocate_core::wire::{read_frame, write_frame};
use collocate_core::Error;
use collocate_gateway::Gateway;
use collocate_remote::client::{enroll, Endpoint, HttpsApi};
use collocate_remote::tls::server_config;
use collocate_trust::{fingerprint_pem, generate_client, generate_server, Token};
use std::collections::HashMap;
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Fake {
    trusted: HashMap<String, Caller>,
    seen: Vec<(String, String)>,
}

fn fake_daemon(path: &std::path::Path, state: Arc<Mutex<Fake>>) {
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let state = state.clone();
            std::thread::spawn(move || {
                while let Ok(req) = read_frame::<_, Request>(&mut s) {
                    let resp = {
                        let mut st = state.lock().unwrap();
                        match req {
                            Request::TrustLookup { fingerprint } => match st.trusted.get(&fingerprint) {
                                Some(c) => Response::Json { value: serde_json::to_value(c).unwrap() },
                                None => Response::error(&Error::NotFound("not trusted".into())),
                            },
                            Request::TrustEnroll { secret, certificate, name } if secret == "good" => {
                                let fp = fingerprint_pem(&certificate).unwrap();
                                let caller = Caller {
                                    name: name.unwrap_or_else(|| "ci".into()),
                                    fingerprint: fp.clone(),
                                    role: Role::Operator,
                                    projects: vec![],
                                };
                                st.trusted.insert(fp, caller.clone());
                                Response::Json {
                                    value: serde_json::json!({"name": caller.name, "role": "operator", "certificate": certificate}),
                                }
                            }
                            Request::TrustEnroll { .. } => {
                                Response::error(&Error::Forbidden("the trust token is unknown or was already used".into()))
                            }
                            Request::As { caller, request } => {
                                st.seen.push((caller.name.clone(), request.verb()));
                                match *request {
                                    Request::Info => Response::Text { text: "{\"initialized\":true}".into() },
                                    Request::Stop { .. } => Response::error(&Error::Forbidden("restricted".into())),
                                    _ => Response::Ok,
                                }
                            }
                            other => Response::error(&Error::Invalid(format!("unexpected {}", other.verb()))),
                        }
                    };
                    if write_frame(&mut s, &resp).is_err() {
                        break;
                    }
                }
            });
        }
    });
}

struct Setup {
    _dir: tempfile::TempDir,
    state: Arc<Mutex<Fake>>,
    address: String,
    fingerprint: String,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("d.sock");
    let state = Arc::new(Mutex::new(Fake::default()));
    fake_daemon(&sock, state.clone());
    let server = generate_server(&["127.0.0.1".into()]).unwrap();
    let config = server_config(&server).unwrap();
    let gateway = Gateway::new(sock, server.fingerprint.clone());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            gateway.try_serve(stream, config.clone());
        }
    });
    Setup { _dir: dir, state, address, fingerprint: server.fingerprint }
}

fn token(s: &Setup, secret: &str) -> Token {
    Token {
        client_name: "ci".into(),
        fingerprint: s.fingerprint.clone(),
        addresses: vec![s.address.clone()],
        secret: secret.into(),
        expires_at: None,
        role: Role::Operator,
        projects: vec![],
    }
}

#[test]
fn clients_enroll_with_a_token_and_then_act_as_themselves() {
    let s = setup();
    let id = generate_client("ci").unwrap();
    let mut api = HttpsApi::new(Endpoint { addresses: vec![s.address.clone()], fingerprint: s.fingerprint.clone() }, id.clone());
    let info = api.server_info().unwrap();
    assert_eq!(info["auth"], "untrusted");
    assert_eq!(info["server_fingerprint"], s.fingerprint.as_str());
    assert_eq!(info["client_fingerprint"], id.fingerprint.as_str());
    assert!(matches!(api.call(Request::Info), Err(Error::Forbidden(_))));

    assert!(matches!(enroll(&token(&s, "bad"), &id, None), Err(Error::Forbidden(_))));
    let mut forged = token(&s, "good");
    forged.fingerprint = "0".repeat(64);
    let body = serde_json::to_vec(&serde_json::json!({"token": forged.encode().unwrap()})).unwrap();
    let (status, text) = api.request("POST", "/1.0/certificates", &body).unwrap();
    assert_eq!(status, 400, "{}", String::from_utf8_lossy(&text));
    assert!(String::from_utf8_lossy(&text).contains("different server"));

    let enrolled = enroll(&token(&s, "good"), &id, Some("builder")).unwrap();
    assert_eq!(enrolled["name"], "builder");
    assert!(enrolled.get("certificate").is_none());
    assert_eq!(api.server_info().unwrap()["auth"], "trusted");
    assert_eq!(api.call(Request::Info).unwrap(), Response::Text { text: "{\"initialized\":true}".into() });
    assert!(matches!(api.call(Request::Stop { target: "w".into(), timeout_secs: None }), Err(Error::Forbidden(_))));
    assert!(matches!(api.call(Request::ImageImport), Err(Error::Invalid(m)) if m.contains("/1.0/images")));
    let seen = s.state.lock().unwrap().seen.clone();
    assert_eq!(seen, vec![("builder".to_string(), "info".to_string()), ("builder".to_string(), "stop".to_string())]);

    let (status, _) = api.request("GET", "/1.0/nothing", b"").unwrap();
    assert_eq!(status, 404);
    let (status, _) = api.request("DELETE", "/1.0/call", b"").unwrap();
    assert_eq!(status, 405);
    let (status, _) = api.request("POST", "/1.0/call", b"{not json").unwrap();
    assert_eq!(status, 400);
    let (status, _) = api.request("POST", "/1.0/call", &vec![b' '; 2 * 1024 * 1024]).unwrap();
    assert_eq!(status, 413);
}

#[test]
fn the_client_refuses_a_server_with_another_fingerprint() {
    let s = setup();
    let id = generate_client("ci").unwrap();
    let mut api = HttpsApi::new(Endpoint { addresses: vec![s.address.clone()], fingerprint: "f".repeat(64) }, id);
    match api.call(Request::Info) {
        Err(Error::Unreachable(m)) => assert!(m.contains("fingerprint"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn repeated_bad_enrollments_are_throttled() {
    let s = setup();
    let id = generate_client("ci").unwrap();
    for _ in 0..10 {
        assert!(matches!(enroll(&token(&s, "bad"), &id, None), Err(Error::Forbidden(_))));
    }
    let mut api = HttpsApi::new(Endpoint { addresses: vec![s.address.clone()], fingerprint: s.fingerprint.clone() }, id.clone());
    let body = serde_json::to_vec(&serde_json::json!({"token": token(&s, "good").encode().unwrap()})).unwrap();
    let (status, _) = api.request("POST", "/1.0/certificates", &body).unwrap();
    assert_eq!(status, 429);
}
