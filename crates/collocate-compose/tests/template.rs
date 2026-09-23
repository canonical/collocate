use collocate_compose::template::{references, resolve, Context, Reference};
use std::collections::HashMap;
use std::net::Ipv4Addr;

fn ctx() -> Context {
    let mut c = Context::default();
    c.secrets.insert("pw".into(), "s3cr3t".into());
    c.addresses.insert("db".into(), vec![Ipv4Addr::new(172, 30, 0, 2)]);
    c.addresses.insert("web".into(), vec![Ipv4Addr::new(172, 30, 0, 4), Ipv4Addr::new(172, 30, 0, 5)]);
    c.lb_addresses.insert("front".into(), Ipv4Addr::new(172, 30, 255, 1));
    c
}

#[test]
fn substitutes_all_reference_kinds() {
    let out = resolve("pg://u:${secrets.pw}@${services.db.address}:5432 lb=${loadbalancers.front.address}", &ctx()).unwrap();
    assert_eq!(out, "pg://u:s3cr3t@172.30.0.2:5432 lb=172.30.255.1");
}

#[test]
fn addresses_expands_to_a_comma_separated_list_and_address_is_the_first() {
    assert_eq!(resolve("${services.web.addresses}", &ctx()).unwrap(), "172.30.0.4,172.30.0.5");
    assert_eq!(resolve("${services.web.address}", &ctx()).unwrap(), "172.30.0.4");
}

#[test]
fn dollar_dollar_escapes() {
    assert_eq!(resolve("cost $$5 and $$${secrets.pw}", &ctx()).unwrap(), "cost $5 and $s3cr3t");
}

#[test]
fn text_without_references_is_untouched() {
    assert_eq!(resolve("plain $ text {x}", &ctx()).unwrap(), "plain $ text {x}");
}

#[test]
fn unknown_references_fail_and_do_not_leak_secret_values() {
    assert!(resolve("${secrets.other}", &ctx()).is_err());
    assert!(resolve("${services.nope.address}", &ctx()).is_err());
    assert!(resolve("${loadbalancers.nope.address}", &ctx()).is_err());
    let e = resolve("${secrets.other} ${secrets.pw}", &ctx()).unwrap_err().to_string();
    assert!(!e.contains("s3cr3t"));
}

#[test]
fn malformed_references_fail() {
    for bad in ["${secrets.pw", "${}", "${bogus.x}", "${services.db.color}", "${secrets}", "${services.db}"] {
        assert!(resolve(bad, &ctx()).is_err(), "{bad}");
    }
}

#[test]
fn services_with_no_addresses_fail() {
    let mut c = ctx();
    c.addresses.insert("empty".into(), vec![]);
    assert!(resolve("${services.empty.address}", &c).is_err());
}

#[test]
fn references_are_listed_without_needing_values() {
    let r = references("${secrets.a} ${services.b.address} ${services.c.addresses} ${loadbalancers.d.address} $$ ${secrets.a}").unwrap();
    assert_eq!(
        r,
        vec![
            Reference::Secret("a".into()),
            Reference::ServiceAddress("b".into()),
            Reference::ServiceAddresses("c".into()),
            Reference::LbAddress("d".into()),
            Reference::Secret("a".into()),
        ]
    );
    let _ = HashMap::<u8, u8>::new();
}
