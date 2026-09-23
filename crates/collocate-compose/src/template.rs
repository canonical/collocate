use collocate_core::{Error, Result};
use std::collections::HashMap;
use std::net::Ipv4Addr;

#[derive(Debug, Clone, Default)]
pub struct Context {
    pub secrets: HashMap<String, String>,
    pub addresses: HashMap<String, Vec<Ipv4Addr>>,
    pub lb_addresses: HashMap<String, Ipv4Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    Secret(String),
    ServiceAddress(String),
    ServiceAddresses(String),
    LbAddress(String),
}

fn parse_reference(body: &str) -> Result<Reference> {
    let parts: Vec<&str> = body.split('.').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(Error::Invalid(format!("malformed reference ${{{body}}}")));
    }
    match parts.as_slice() {
        ["secrets", name] => Ok(Reference::Secret((*name).to_string())),
        ["services", name, "address"] => Ok(Reference::ServiceAddress((*name).to_string())),
        ["services", name, "addresses"] => Ok(Reference::ServiceAddresses((*name).to_string())),
        ["loadbalancers", name, "address"] => Ok(Reference::LbAddress((*name).to_string())),
        _ => Err(Error::Invalid(format!("unsupported reference ${{{body}}}"))),
    }
}

fn scan(input: &str, mut on_ref: impl FnMut(Reference) -> Result<String>) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        if let Some(stripped) = after.strip_prefix('$') {
            out.push('$');
            rest = stripped;
        } else if let Some(stripped) = after.strip_prefix('{') {
            let end = stripped.find('}').ok_or_else(|| Error::Invalid("unterminated ${ reference".into()))?;
            out.push_str(&on_ref(parse_reference(&stripped[..end])?)?);
            rest = &stripped[end + 1..];
        } else {
            out.push('$');
            rest = after;
        }
    }
    out.push_str(rest);
    Ok(out)
}

pub fn references(input: &str) -> Result<Vec<Reference>> {
    let mut found = Vec::new();
    scan(input, |r| {
        found.push(r);
        Ok(String::new())
    })?;
    Ok(found)
}

pub fn resolve(input: &str, ctx: &Context) -> Result<String> {
    scan(input, |r| match r {
        Reference::Secret(n) => ctx.secrets.get(&n).cloned().ok_or_else(|| Error::Invalid(format!("unknown secret {n}"))),
        Reference::ServiceAddress(n) => ctx
            .addresses
            .get(&n)
            .and_then(|v| v.first())
            .map(ToString::to_string)
            .ok_or_else(|| Error::Invalid(format!("no address known for service {n}"))),
        Reference::ServiceAddresses(n) => match ctx.addresses.get(&n) {
            Some(v) if !v.is_empty() => Ok(v.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")),
            _ => Err(Error::Invalid(format!("no addresses known for service {n}"))),
        },
        Reference::LbAddress(n) => ctx
            .lb_addresses
            .get(&n)
            .map(ToString::to_string)
            .ok_or_else(|| Error::Invalid(format!("no address known for load balancer {n}"))),
    })
}
