use collocate_remote::config::{config_dir_from, valid_remote_name, Remote, RemoteConfig};
use collocate_remote::http::{finish_chunks, percent_decode, percent_encode, read_request, read_response, write_chunk, Body, Buffered};
use std::io::{Cursor, Read};

#[test]
fn requests_parse_with_query_and_leave_pipelined_bytes() {
    let raw = b"POST /1.0/call?target=web%2D1&follow=1 HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhelloGET /1.0 HTTP/1.1\r\nConnection: close\r\n\r\n";
    let mut c = Buffered::new(Cursor::new(raw.to_vec()));
    let head = read_request(&mut c).unwrap().unwrap();
    assert_eq!((head.method.as_str(), head.path.as_str()), ("POST", "/1.0/call"));
    assert_eq!(head.param("target"), Some("web-1"));
    assert!(head.keep_alive());
    assert_eq!(Body::new(&mut c, &head, true).unwrap().read_all(100).unwrap(), b"hello");
    let second = read_request(&mut c).unwrap().unwrap();
    assert_eq!(second.path, "/1.0");
    assert!(!second.keep_alive());
    assert!(read_request(&mut c).unwrap().is_none());
}

#[test]
fn chunked_bodies_round_trip_and_limits_apply() {
    let mut out = Vec::new();
    write_chunk(&mut out, b"hello ").unwrap();
    write_chunk(&mut out, b"").unwrap();
    write_chunk(&mut out, b"world").unwrap();
    finish_chunks(&mut out).unwrap();
    let mut raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    raw.extend_from_slice(&out);
    let mut c = Buffered::new(Cursor::new(raw.clone()));
    let head = read_response(&mut c).unwrap();
    assert_eq!(head.status, 200);
    assert_eq!(Body::new(&mut c, &head, false).unwrap().read_all(1024).unwrap(), b"hello world");
    let mut c = Buffered::new(Cursor::new(raw));
    let head = read_response(&mut c).unwrap();
    assert!(Body::new(&mut c, &head, false).unwrap().read_all(4).is_err());
    let mut c = Buffered::new(Cursor::new(b"HTTP/1.1 200 OK\r\n\r\nuntil close".to_vec()));
    let head = read_response(&mut c).unwrap();
    let mut s = String::new();
    Body::new(&mut c, &head, false).unwrap().read_to_string(&mut s).unwrap();
    assert_eq!(s, "until close");
    assert!(read_request(&mut Buffered::new(Cursor::new(b"garbage\r\n\r\n".to_vec()))).is_err());
    assert!(read_request(&mut Buffered::new(Cursor::new(vec![b'a'; 70 * 1024]))).is_err());
}

#[test]
fn percent_coding_round_trips() {
    assert_eq!(percent_decode(&percent_encode("web 1/x?&=")), "web 1/x?&=");
    assert_eq!(percent_decode("a+b%zz%4"), "a b%zz%4");
}

#[test]
fn remotes_live_in_the_config_dir() {
    let env =
        |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
    assert_eq!(config_dir_from(&env(&[("HOME", "/home/u")])), std::path::PathBuf::from("/home/u/.config/collocate"));
    assert_eq!(
        config_dir_from(&env(&[("HOME", "/home/u"), ("SNAP_USER_COMMON", "/home/u/snap/collocate/common")])),
        std::path::PathBuf::from("/home/u/snap/collocate/common/config")
    );
    assert_eq!(config_dir_from(&env(&[("COLLOCATE_CONFIG_DIR", "/x"), ("HOME", "/h")])), std::path::PathBuf::from("/x"));
    assert!(valid_remote_name("prod-1").is_ok());
    assert!(valid_remote_name("local").is_err() && valid_remote_name("a b").is_err());
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(RemoteConfig::load(dir.path()).unwrap().default_remote(), "local");
    let mut cfg = RemoteConfig::default();
    cfg.remotes.insert("prod".into(), Remote { addresses: vec!["10.0.0.1:8443".into()], fingerprint: "a".repeat(64) });
    cfg.default = Some("prod".into());
    cfg.save(dir.path()).unwrap();
    let back = RemoteConfig::load(dir.path()).unwrap();
    assert_eq!(back, cfg);
    assert!(back.get("nope").is_err());
}
