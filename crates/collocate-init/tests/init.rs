use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const INIT: &str = env!("CARGO_BIN_EXE_collocate-init");

fn init(args: &[&str]) -> Command {
    let mut c = Command::new(INIT);
    c.args(args).stdin(Stdio::null());
    c
}

fn code(args: &[&str]) -> i32 {
    let st = init(args).status().unwrap();
    st.code().unwrap_or_else(|| 128 + st.signal().unwrap())
}

fn wait_for(path: &Path) {
    let start = Instant::now();
    while !path.exists() {
        assert!(start.elapsed() < Duration::from_secs(5), "timed out waiting for {path:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_exit(mut child: Child) -> i32 {
    let start = Instant::now();
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            return st.code().unwrap_or_else(|| 128 + st.signal().unwrap());
        }
        assert!(start.elapsed() < Duration::from_secs(5), "init did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn propagates_the_workload_exit_code() {
    assert_eq!(code(&["/bin/sh", "-c", "exit 3"]), 3);
    assert_eq!(code(&["/bin/true"]), 0);
}

#[test]
fn signal_deaths_map_to_128_plus_signal() {
    assert_eq!(code(&["/bin/sh", "-c", "kill -9 $$"]), 137);
}

#[test]
fn accepts_a_double_dash_separator() {
    assert_eq!(code(&["--", "/bin/sh", "-c", "exit 4"]), 4);
}

#[test]
fn missing_command_is_a_usage_error() {
    assert_eq!(code(&[]), 2);
}

#[test]
fn exec_failure_exits_127() {
    assert_eq!(code(&["/definitely/not/here"]), 127);
}

#[test]
fn path_lookup_is_supported() {
    assert_eq!(code(&["sh", "-c", "exit 6"]), 6);
}

fn forwards(signal: i32, name: &str) {
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("ready");
    let script = format!("trap 'exit 42' {name}; touch {}; while :; do sleep 0.05; done", ready.display());
    let child = init(&["/bin/sh", "-c", &script]).spawn().unwrap();
    wait_for(&ready);
    unsafe { libc::kill(child.id() as i32, signal) };
    assert_eq!(wait_exit(child), 42);
}

#[test]
fn forwards_sigterm_to_the_workload() {
    forwards(libc::SIGTERM, "TERM");
}

#[test]
fn forwards_sigint_to_the_workload() {
    forwards(libc::SIGINT, "INT");
}

#[test]
fn forwards_sighup_and_sigusr1() {
    forwards(libc::SIGHUP, "HUP");
    forwards(libc::SIGUSR1, "USR1");
}

#[test]
fn reaps_orphaned_grandchildren() {
    let script = "(sleep 0.2 &); sleep 0.8; if grep -qs '^[0-9]* (sleep) Z' /proc/[0-9]*/stat; then exit 9; else exit 0; fi";
    assert_eq!(code(&["/bin/sh", "-c", script]), 0);
}

#[test]
fn stdout_and_stderr_pass_through() {
    let out = init(&["/bin/sh", "-c", "echo out; echo err >&2"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "err\n");
}

#[test]
fn environment_is_inherited() {
    let out = init(&["/bin/sh", "-c", "echo $FOO"]).env("FOO", "bar").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "bar\n");
}

#[test]
fn workloads_do_not_inherit_ignored_sigpipe() {
    let out = init(&["/bin/sh", "-c", "grep SigIgn /proc/self/status"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let hex = text.split_whitespace().nth(1).unwrap();
    let ign = u64::from_str_radix(hex, 16).unwrap();
    assert_eq!(ign & (1 << (libc::SIGPIPE - 1)), 0, "{text}");
}
