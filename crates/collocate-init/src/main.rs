use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

const FORWARDED: [libc::c_int; 6] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT, libc::SIGUSR1, libc::SIGUSR2];

fn exit_code(status: libc::c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

fn write_stderr(msg: &str) {
    unsafe { libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len()) };
}

fn signal_set() -> libc::sigset_t {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in FORWARDED {
            libc::sigaddset(&mut set, s);
        }
        libc::sigaddset(&mut set, libc::SIGCHLD);
        set
    }
}

fn run(argv: Vec<CString>) -> i32 {
    let mut ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    let set = signal_set();
    let mut old: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
        libc::sigprocmask(libc::SIG_BLOCK, &set, &mut old);
    }
    let child = unsafe { libc::fork() };
    if child < 0 {
        write_stderr("collocate-init: fork failed\n");
        return 1;
    }
    if child == 0 {
        unsafe {
            libc::sigprocmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            libc::execvp(ptrs[0], ptrs.as_ptr());
        }
        let name = argv[0].to_string_lossy().into_owned();
        write_stderr(&format!("collocate-init: cannot execute {name}\n"));
        unsafe { libc::_exit(127) };
    }
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let sig = unsafe { libc::sigwaitinfo(&set, &mut info) };
        if sig < 0 {
            continue;
        }
        if sig == libc::SIGCHLD {
            loop {
                let mut status = 0;
                let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
                if pid <= 0 {
                    break;
                }
                if pid == child {
                    return exit_code(status);
                }
            }
        } else {
            unsafe { libc::kill(child, sig) };
        }
    }
}

fn main() {
    let mut args: Vec<CString> = std::env::args_os().skip(1).filter_map(|a| CString::new(a.as_bytes()).ok()).collect();
    if args.first().is_some_and(|a| a.as_bytes() == b"--") {
        args.remove(0);
    }
    if args.is_empty() {
        write_stderr("usage: collocate-init [--] command [args...]\n");
        std::process::exit(2);
    }
    std::process::exit(run(args));
}
