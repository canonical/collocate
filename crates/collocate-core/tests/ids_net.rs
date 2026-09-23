use collocate_core::id::{resolve_ref, ContainerId};
use collocate_core::net::{Proto, Publish, Volume};
use collocate_core::Error;

fn id(s: &str) -> ContainerId {
    ContainerId::parse(s).unwrap()
}

#[test]
fn id_roundtrips_through_hex() {
    let i = ContainerId::from_bytes([0x7f, 0x3a, 0x9c, 0x02, 0xe1, 0xb4]);
    assert_eq!(i.to_string(), "7f3a9c02e1b4");
    assert_eq!(id("7f3a9c02e1b4"), i);
}

#[test]
fn id_rejects_bad_input() {
    assert!(ContainerId::parse("zz").is_err());
    assert!(ContainerId::parse("7f3a9c02e1b").is_err());
    assert!(ContainerId::parse("7F3A9C02E1B4").is_err());
}

#[test]
fn short_form_is_first_twelve() {
    assert_eq!(id("7f3a9c02e1b4").short(), "7f3a9c02e1b4");
}

#[test]
fn resolve_prefers_exact_name() {
    let all = vec![(id("aaaaaaaaaaaa"), "web".to_string()), (id("bbbbbbbbbbbb"), "db".to_string())];
    assert_eq!(resolve_ref("db", &all).unwrap(), id("bbbbbbbbbbbb"));
}

#[test]
fn resolve_accepts_unique_prefix() {
    let all = vec![(id("aaaaaaaaaaaa"), "web".to_string()), (id("abbbbbbbbbbb"), "db".to_string())];
    assert_eq!(resolve_ref("aa", &all).unwrap(), id("aaaaaaaaaaaa"));
}

#[test]
fn resolve_ambiguous_prefix_lists_candidates() {
    let all = vec![(id("aaaaaaaaaaaa"), "web".to_string()), (id("abbbbbbbbbbb"), "db".to_string())];
    match resolve_ref("a", &all) {
        Err(Error::Ambiguous(c)) => assert_eq!(c.len(), 2),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn resolve_not_found() {
    let all = vec![(id("aaaaaaaaaaaa"), "web".to_string())];
    assert!(matches!(resolve_ref("nope", &all), Err(Error::NotFound(_))));
}

#[test]
fn name_beats_prefix_collision() {
    let all = vec![(id("abababababab"), "aa".to_string()), (id("aaaaaaaaaaaa"), "x".to_string())];
    assert_eq!(resolve_ref("aa", &all).unwrap(), id("abababababab"));
}

#[test]
fn publish_parses_default_tcp() {
    let p = Publish::parse("8080:80").unwrap();
    assert_eq!((p.host, p.container, p.proto), (8080, 80, Proto::Tcp));
}

#[test]
fn publish_parses_udp() {
    let p = Publish::parse("53:53/udp").unwrap();
    assert_eq!(p.proto, Proto::Udp);
}

#[test]
fn publish_rejects_bad_values() {
    for bad in ["abc", "0:80", "80:0", "70000:80", "80:80/sctp", "80", "80:80:80"] {
        assert!(Publish::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn publish_display_roundtrips() {
    let p = Publish::parse("8080:80/udp").unwrap();
    assert_eq!(p.to_string(), "8080:80/udp");
}

#[test]
fn volume_parses_rw_and_ro() {
    let v = Volume::parse("/srv/web1:/data").unwrap();
    assert_eq!((v.src.as_str(), v.dst.as_str(), v.ro), ("/srv/web1", "/data", false));
    let v = Volume::parse("/srv/web1:/data:ro").unwrap();
    assert!(v.ro);
}

#[test]
fn volume_names_are_distinguished_from_paths() {
    let v = Volume::parse("pgdata:/var/lib/postgresql").unwrap();
    assert!(v.is_named());
    assert!(!Volume::parse("/abs:/x").unwrap().is_named());
}

#[test]
fn volume_rejects_relative_destination_and_junk() {
    assert!(Volume::parse("/a:rel").is_err());
    assert!(Volume::parse("/a").is_err());
    assert!(Volume::parse("/a:/b:rw:extra").is_err());
    assert!(Volume::parse("/a:/b:zz").is_err());
}
