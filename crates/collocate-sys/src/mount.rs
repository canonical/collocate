use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    pub mount_point: String,
    pub fstype: String,
    pub source: String,
    pub options: String,
}

fn unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 4 <= bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_mountinfo(text: &str) -> Vec<MountEntry> {
    text.lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let fields: Vec<&str> = left.split_whitespace().collect();
            let rest: Vec<&str> = right.split_whitespace().collect();
            if fields.len() < 6 || rest.len() < 2 {
                return None;
            }
            Some(MountEntry {
                mount_point: unescape(fields[4]),
                fstype: rest[0].to_string(),
                source: unescape(rest[1]),
                options: fields[5].to_string(),
            })
        })
        .collect()
}

pub fn is_mount_point(entries: &[MountEntry], path: &str) -> bool {
    entries.iter().any(|e| e.mount_point == path)
}

fn cstr(s: &str) -> io::Result<CString> {
    CString::new(s).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn check(rc: libc::c_long) -> io::Result<()> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub fn mount(source: Option<&str>, target: &str, fstype: Option<&str>, flags: u64, data: Option<&str>) -> io::Result<()> {
    let s = source.map(cstr).transpose()?;
    let t = cstr(target)?;
    let f = fstype.map(cstr).transpose()?;
    let d = data.map(cstr).transpose()?;
    let rc = unsafe {
        libc::mount(
            s.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            t.as_ptr(),
            f.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            flags,
            d.as_ref().map_or(std::ptr::null(), |c| c.as_ptr() as *const libc::c_void),
        )
    };
    check(rc as libc::c_long)
}

pub fn make_private_recursive(target: &str) -> io::Result<()> {
    mount(None, target, None, libc::MS_REC | libc::MS_PRIVATE, None)
}

pub fn bind(src: &str, dst: &str, recursive: bool) -> io::Result<()> {
    let flags = libc::MS_BIND | if recursive { libc::MS_REC } else { 0 };
    mount(Some(src), dst, None, flags, None)
}

pub fn remount_bind_readonly(dst: &str) -> io::Result<()> {
    mount(None, dst, None, libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_REC, None)
}

pub fn umount_lazy(target: &str) -> io::Result<()> {
    let t = cstr(target)?;
    check(unsafe { libc::umount2(t.as_ptr(), libc::MNT_DETACH) } as libc::c_long)
}

pub fn pivot_root(new_root: &str, put_old: &str) -> io::Result<()> {
    let n = cstr(new_root)?;
    let p = cstr(put_old)?;
    check(unsafe { libc::syscall(libc::SYS_pivot_root, n.as_ptr(), p.as_ptr()) })
}

const SYS_MOVE_MOUNT: libc::c_long = 429;
const SYS_FSOPEN: libc::c_long = 430;
const SYS_FSCONFIG: libc::c_long = 431;
const SYS_FSMOUNT: libc::c_long = 432;
const FSCONFIG_SET_STRING: u32 = 1;
const FSCONFIG_CMD_CREATE: u32 = 6;
const FSOPEN_CLOEXEC: u32 = 1;
const FSMOUNT_CLOEXEC: u32 = 1;
const MOVE_MOUNT_F_EMPTY_PATH: u32 = 4;

pub struct FsContext {
    fd: OwnedFd,
}

impl FsContext {
    pub fn open(fstype: &str) -> io::Result<FsContext> {
        let t = cstr(fstype)?;
        let fd = unsafe { libc::syscall(SYS_FSOPEN, t.as_ptr(), FSOPEN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(FsContext { fd: unsafe { OwnedFd::from_raw_fd(fd as i32) } })
    }

    pub fn set_string(&self, key: &str, value: &str) -> io::Result<()> {
        let k = cstr(key)?;
        let v = cstr(value)?;
        check(unsafe { libc::syscall(SYS_FSCONFIG, self.fd.as_raw_fd(), FSCONFIG_SET_STRING, k.as_ptr(), v.as_ptr(), 0) })
    }

    pub fn set_flag(&self, key: &str) -> io::Result<()> {
        let k = cstr(key)?;
        check(unsafe { libc::syscall(SYS_FSCONFIG, self.fd.as_raw_fd(), 0u32, k.as_ptr(), std::ptr::null::<libc::c_char>(), 0) })
    }

    pub fn create(&self) -> io::Result<()> {
        check(unsafe {
            libc::syscall(
                SYS_FSCONFIG,
                self.fd.as_raw_fd(),
                FSCONFIG_CMD_CREATE,
                std::ptr::null::<libc::c_char>(),
                std::ptr::null::<libc::c_char>(),
                0,
            )
        })
    }

    pub fn mount_at(self, target: &str, attr_flags: u32) -> io::Result<()> {
        let mfd = unsafe { libc::syscall(SYS_FSMOUNT, self.fd.as_raw_fd(), FSMOUNT_CLOEXEC, attr_flags) };
        if mfd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mfd = unsafe { OwnedFd::from_raw_fd(mfd as i32) };
        let t = cstr(target)?;
        let empty = cstr("")?;
        check(unsafe {
            libc::syscall(SYS_MOVE_MOUNT, mfd.as_raw_fd(), empty.as_ptr(), libc::AT_FDCWD, t.as_ptr(), MOVE_MOUNT_F_EMPTY_PATH)
        })
    }
}
