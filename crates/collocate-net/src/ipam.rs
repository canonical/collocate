use collocate_core::{Error, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::net::Ipv4Addr;

const VIP_CAP: u32 = 255;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subnet {
    network: u32,
    prefix: u8,
}

impl Subnet {
    pub fn parse(s: &str) -> Result<Self> {
        let bad = || Error::Invalid(format!("invalid subnet {s}"));
        let (addr, prefix) = s.split_once('/').ok_or_else(bad)?;
        let addr: Ipv4Addr = addr.parse().map_err(|_| bad())?;
        let prefix: u8 = prefix.parse().map_err(|_| bad())?;
        if !(8..=30).contains(&prefix) {
            return Err(bad());
        }
        let mask = u32::MAX << (32 - prefix);
        Ok(Subnet { network: u32::from(addr) & mask, prefix })
    }

    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    pub fn network(&self) -> Ipv4Addr {
        Ipv4Addr::from(self.network)
    }

    fn size(&self) -> u32 {
        1u32 << (32 - self.prefix)
    }

    fn broadcast_u32(&self) -> u32 {
        self.network + (self.size() - 1)
    }

    pub fn gateway(&self) -> Ipv4Addr {
        Ipv4Addr::from(self.network + 1)
    }

    pub fn broadcast(&self) -> Ipv4Addr {
        Ipv4Addr::from(self.broadcast_u32())
    }

    pub fn contains(&self, addr: Ipv4Addr) -> bool {
        let a = u32::from(addr);
        a >= self.network && a <= self.broadcast_u32()
    }
}

impl fmt::Display for Subnet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", Ipv4Addr::from(self.network), self.prefix)
    }
}

#[derive(Debug, Clone)]
pub struct Ipam {
    subnet: Subnet,
    by_owner: BTreeMap<String, Ipv4Addr>,
    by_addr: BTreeMap<Ipv4Addr, String>,
}

fn hash_u64(parts: &[&str]) -> u64 {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    let d = h.finalize();
    u64::from_be_bytes(d[..8].try_into().unwrap_or([0; 8]))
}

impl Ipam {
    pub fn new(subnet: Subnet) -> Self {
        Ipam { subnet, by_owner: BTreeMap::new(), by_addr: BTreeMap::new() }
    }

    pub fn rebuild(subnet: Subnet, entries: impl IntoIterator<Item = (String, Ipv4Addr)>) -> Result<Self> {
        let mut ipam = Ipam::new(subnet);
        for (owner, addr) in entries {
            ipam.reserve(addr, &owner)?;
        }
        Ok(ipam)
    }

    pub fn subnet(&self) -> &Subnet {
        &self.subnet
    }

    fn first(&self) -> u32 {
        self.subnet.network + 2
    }

    fn last(&self) -> u32 {
        self.subnet.broadcast_u32() - 1
    }

    fn vip_count(&self) -> u32 {
        let usable = self.last() - self.first() + 1;
        (usable / 8).min(VIP_CAP)
    }

    fn pool_end(&self) -> u32 {
        self.last() - self.vip_count()
    }

    pub fn is_vip(&self, addr: Ipv4Addr) -> bool {
        let a = u32::from(addr);
        self.vip_count() > 0 && a > self.pool_end() && a <= self.last()
    }

    pub fn owner_of(&self, addr: Ipv4Addr) -> Option<&str> {
        self.by_addr.get(&addr).map(String::as_str)
    }

    pub fn address_of(&self, owner: &str) -> Option<Ipv4Addr> {
        self.by_owner.get(owner).copied()
    }

    pub fn reserve(&mut self, addr: Ipv4Addr, owner: &str) -> Result<()> {
        let a = u32::from(addr);
        if !self.subnet.contains(addr) || a < self.first() || a > self.last() {
            return Err(Error::Invalid(format!("{addr} is not assignable in {}", self.subnet)));
        }
        match self.by_addr.get(&addr) {
            Some(existing) if existing != owner => {
                return Err(Error::Conflict(format!("{addr} is already assigned to {existing}")));
            }
            _ => {}
        }
        if let Some(old) = self.by_owner.insert(owner.to_string(), addr) {
            if old != addr {
                self.by_addr.remove(&old);
            }
        }
        self.by_addr.insert(addr, owner.to_string());
        Ok(())
    }

    pub fn release(&mut self, owner: &str) {
        if let Some(addr) = self.by_owner.remove(owner) {
            self.by_addr.remove(&addr);
        }
    }

    pub fn allocate(&mut self, owner: &str) -> Result<Ipv4Addr> {
        if let Some(a) = self.by_owner.get(owner) {
            return Ok(*a);
        }
        let mut cur = self.first();
        while cur <= self.pool_end() {
            let addr = Ipv4Addr::from(cur);
            if !self.by_addr.contains_key(&addr) {
                self.reserve(addr, owner)?;
                return Ok(addr);
            }
            cur += 1;
        }
        Err(Error::Conflict(format!("subnet {} is exhausted", self.subnet)))
    }

    fn probe(&mut self, owner: String, start: u32, len: u32, base: u32) -> Result<Ipv4Addr> {
        if len == 0 {
            return Err(Error::Conflict(format!("no address range available in {}", self.subnet)));
        }
        for step in 0..len {
            let addr = Ipv4Addr::from(base + ((start + step) % len));
            if !self.by_addr.contains_key(&addr) {
                self.reserve(addr, &owner)?;
                return Ok(addr);
            }
        }
        Err(Error::Conflict(format!("address range in {} is exhausted", self.subnet)))
    }

    pub fn for_service(&mut self, project: &str, service: &str, index: u32) -> Result<Ipv4Addr> {
        let owner = format!("{project}/{service}/{index}");
        if let Some(a) = self.by_owner.get(&owner) {
            return Ok(*a);
        }
        let len = self.pool_end() - self.first() + 1;
        let start = (hash_u64(&[project, service, &index.to_string()]) % u64::from(len)) as u32;
        self.probe(owner, start, len, self.first())
    }

    pub fn vip(&mut self, project: &str, lb: &str) -> Result<Ipv4Addr> {
        let owner = format!("vip:{project}/{lb}");
        if let Some(a) = self.by_owner.get(&owner) {
            return Ok(*a);
        }
        let len = self.vip_count();
        if len == 0 {
            return Err(Error::Conflict(format!("subnet {} has no room for virtual addresses", self.subnet)));
        }
        let start = (hash_u64(&[project, lb, "vip"]) % u64::from(len)) as u32;
        self.probe(owner, start, len, self.pool_end() + 1)
    }
}
