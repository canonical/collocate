use collocate_core::id::random_hex;
use collocate_core::net::{nft_tag, veth_names};
use collocate_core::ContainerId;

#[test]
fn veth_names_fit_ifnamsiz() {
    let id = ContainerId::parse("7f3a9c02e1b4").unwrap();
    let (host, cont) = veth_names(&id);
    assert_eq!(host, "vh7f3a9c02e1");
    assert_eq!(cont, "vc7f3a9c02e1");
    assert!(host.len() <= 15 && cont.len() <= 15);
}

#[test]
fn nft_tags_are_stable_and_prefixed() {
    let id = ContainerId::parse("7f3a9c02e1b4").unwrap();
    assert_eq!(nft_tag(&id), "collocate:7f3a9c02e1b4");
}

#[test]
fn random_hex_has_requested_length_and_varies() {
    let a = random_hex(16).unwrap();
    let b = random_hex(16).unwrap();
    assert_eq!(a.len(), 32);
    assert_ne!(a, b);
    assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
}
