use collocate_core::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ids {
    pub uid: u32,
    pub gid: u32,
    pub groups: Vec<u32>,
}

struct PasswdEntry {
    name: String,
    uid: u32,
    gid: u32,
}

struct GroupEntry {
    name: String,
    gid: u32,
    members: Vec<String>,
}

fn parse_passwd(text: &str) -> Vec<PasswdEntry> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            Some(PasswdEntry { name: f.first()?.to_string(), uid: f.get(2)?.parse().ok()?, gid: f.get(3)?.parse().ok()? })
        })
        .collect()
}

fn parse_group(text: &str) -> Vec<GroupEntry> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            Some(GroupEntry {
                name: f.first()?.to_string(),
                gid: f.get(2)?.parse().ok()?,
                members: f.get(3).map(|m| m.split(',').filter(|s| !s.is_empty()).map(String::from).collect()).unwrap_or_default(),
            })
        })
        .collect()
}

pub fn resolve_user(user: &str, passwd: &str, group: &str) -> Result<Ids> {
    if user.is_empty() {
        return Err(Error::Invalid("empty user".into()));
    }
    let users = parse_passwd(passwd);
    let groups = parse_group(group);
    let (uname, gname) = match user.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (user, None),
    };
    let entry = match uname.parse::<u32>() {
        Ok(uid) => users.iter().find(|p| p.uid == uid).map(|p| (p.name.clone(), p.uid, p.gid)).or(Some((String::new(), uid, 0))),
        Err(_) => users.iter().find(|p| p.name == uname).map(|p| (p.name.clone(), p.uid, p.gid)),
    };
    let (name, uid, default_gid) = entry.ok_or_else(|| Error::Invalid(format!("unknown user {uname}")))?;
    let gid = match gname {
        None => default_gid,
        Some(g) => match g.parse::<u32>() {
            Ok(n) => n,
            Err(_) => groups.iter().find(|e| e.name == g).map(|e| e.gid).ok_or_else(|| Error::Invalid(format!("unknown group {g}")))?,
        },
    };
    let mut supplementary: Vec<u32> =
        if name.is_empty() { Vec::new() } else { groups.iter().filter(|g| g.members.contains(&name)).map(|g| g.gid).collect() };
    supplementary.push(gid);
    supplementary.sort_unstable();
    supplementary.dedup();
    Ok(Ids { uid, gid, groups: supplementary })
}
