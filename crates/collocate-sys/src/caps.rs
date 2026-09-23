use collocate_core::{Error, Result};

const NAMES: [&str; 41] = [
    "CHOWN",
    "DAC_OVERRIDE",
    "DAC_READ_SEARCH",
    "FOWNER",
    "FSETID",
    "KILL",
    "SETGID",
    "SETUID",
    "SETPCAP",
    "LINUX_IMMUTABLE",
    "NET_BIND_SERVICE",
    "NET_BROADCAST",
    "NET_ADMIN",
    "NET_RAW",
    "IPC_LOCK",
    "IPC_OWNER",
    "SYS_MODULE",
    "SYS_RAWIO",
    "SYS_CHROOT",
    "SYS_PTRACE",
    "SYS_PACCT",
    "SYS_ADMIN",
    "SYS_BOOT",
    "SYS_NICE",
    "SYS_RESOURCE",
    "SYS_TIME",
    "SYS_TTY_CONFIG",
    "MKNOD",
    "LEASE",
    "AUDIT_WRITE",
    "AUDIT_CONTROL",
    "SETFCAP",
    "MAC_OVERRIDE",
    "MAC_ADMIN",
    "SYSLOG",
    "WAKE_ALARM",
    "BLOCK_SUSPEND",
    "AUDIT_READ",
    "PERFMON",
    "BPF",
    "CHECKPOINT_RESTORE",
];

const DEFAULT: [&str; 13] = [
    "CHOWN",
    "DAC_OVERRIDE",
    "FOWNER",
    "FSETID",
    "KILL",
    "SETGID",
    "SETUID",
    "SETPCAP",
    "NET_BIND_SERVICE",
    "SYS_CHROOT",
    "MKNOD",
    "AUDIT_WRITE",
    "SETFCAP",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap(u8);

impl Cap {
    pub fn from_name(name: &str) -> Option<Cap> {
        let upper = name.trim().to_ascii_uppercase();
        let short = upper.strip_prefix("CAP_").unwrap_or(&upper);
        NAMES.iter().position(|n| *n == short).map(|i| Cap(i as u8))
    }

    pub fn number(&self) -> u32 {
        u32::from(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CapSet(u64);

impl CapSet {
    pub fn empty() -> Self {
        CapSet(0)
    }

    pub fn contains(&self, cap: Cap) -> bool {
        self.0 & (1 << cap.0) != 0
    }

    pub fn insert(&mut self, cap: Cap) {
        self.0 |= 1 << cap.0;
    }

    pub fn remove(&mut self, cap: Cap) {
        self.0 &= !(1 << cap.0);
    }

    pub fn names(&self) -> Vec<String> {
        (0..NAMES.len()).filter(|i| self.0 & (1 << i) != 0).map(|i| NAMES[i].to_string()).collect()
    }

    pub fn words(&self) -> (u32, u32) {
        (self.0 as u32, (self.0 >> 32) as u32)
    }

    pub fn with_policy(add: &[String], drop: &[String]) -> Result<CapSet> {
        let mut set = default_set();
        for name in drop {
            if name.eq_ignore_ascii_case("ALL") {
                set = CapSet::empty();
                continue;
            }
            let cap = Cap::from_name(name).ok_or_else(|| Error::Invalid(format!("unknown capability {name}")))?;
            set.remove(cap);
        }
        for name in add {
            if name.eq_ignore_ascii_case("ALL") {
                for i in 0..NAMES.len() {
                    set.0 |= 1 << i;
                }
                continue;
            }
            let cap = Cap::from_name(name).ok_or_else(|| Error::Invalid(format!("unknown capability {name}")))?;
            set.insert(cap);
        }
        Ok(set)
    }

    pub fn apply(&self) -> std::io::Result<()> {
        self.drop_bounding()?;
        self.set_current()
    }

    pub fn drop_bounding(&self) -> std::io::Result<()> {
        let last = last_cap()?;
        for cap in 0..=last {
            if cap < 64 && self.0 & (1 << cap) == 0 {
                let rc = unsafe { libc::prctl(libc::PR_CAPBSET_DROP, libc::c_ulong::from(cap), 0, 0, 0) };
                if rc != 0 {
                    let e = std::io::Error::last_os_error();
                    if e.raw_os_error() != Some(libc::EINVAL) {
                        return Err(e);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn set_current(&self) -> std::io::Result<()> {
        #[repr(C)]
        struct Header {
            version: u32,
            pid: i32,
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct Data {
            effective: u32,
            permitted: u32,
            inheritable: u32,
        }
        let (lo, hi) = self.words();
        let header = Header { version: 0x2008_0522, pid: 0 };
        let data = [Data { effective: lo, permitted: lo, inheritable: lo }, Data { effective: hi, permitted: hi, inheritable: hi }];
        let rc = unsafe { libc::syscall(libc::SYS_capset, &header as *const Header, data.as_ptr()) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let rc = unsafe { libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

fn last_cap() -> std::io::Result<u32> {
    let text = std::fs::read_to_string("/proc/sys/kernel/cap_last_cap")?;
    text.trim().parse().map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))
}

pub fn default_set() -> CapSet {
    let mut s = CapSet::empty();
    for n in DEFAULT {
        if let Some(c) = Cap::from_name(n) {
            s.insert(c);
        }
    }
    s
}
