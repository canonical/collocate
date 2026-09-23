use crate::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::io::Read;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContainerId([u8; 6]);

impl ContainerId {
    pub fn from_bytes(bytes: [u8; 6]) -> Self {
        ContainerId(bytes)
    }

    pub fn random() -> Result<Self> {
        let mut b = [0u8; 6];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
        Ok(ContainerId(b))
    }

    pub fn parse(s: &str) -> Result<Self> {
        let bad = || Error::InvalidId(s.to_string());
        if s.len() != 12 || !s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(bad());
        }
        let mut b = [0u8; 6];
        for (i, slot) in b.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| bad())?;
        }
        Ok(ContainerId(b))
    }

    pub fn short(&self) -> String {
        self.to_string()
    }

    pub fn as_bytes(&self) -> &[u8; 6] {
        &self.0
    }
}

impl fmt::Display for ContainerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ContainerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContainerId({self})")
    }
}

impl Serialize for ContainerId {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ContainerId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        ContainerId::parse(&s).map_err(serde::de::Error::custom)
    }
}

pub fn resolve_ref(reference: &str, all: &[(ContainerId, String)]) -> Result<ContainerId> {
    if let Some((id, _)) = all.iter().find(|(_, n)| n == reference) {
        return Ok(*id);
    }
    let matches: Vec<ContainerId> =
        all.iter().map(|(id, _)| *id).filter(|id| !reference.is_empty() && id.to_string().starts_with(reference)).collect();
    match matches.as_slice() {
        [] => Err(Error::NotFound(reference.to_string())),
        [one] => Ok(*one),
        many => Err(Error::Ambiguous(many.iter().map(|i| i.to_string()).collect())),
    }
}

pub fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}
