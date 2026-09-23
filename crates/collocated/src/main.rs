use collocate_store::NodeStore;
use collocated::config::Config;
use collocated::daemon::{instance_id, Daemon};
use std::path::PathBuf;

fn usage() -> ! {
    eprintln!("usage: collocated [--config PATH] [--check] [--reinit-node | --adopt]");
    std::process::exit(2);
}

fn prune_mounts(cfg: &Config) {
    if collocate_sys::misc::unshare(libc::CLONE_NEWNS as u64).is_err() {
        return;
    }
    let _ = collocate_sys::mount::make_private_recursive("/");
    let images = cfg.images_dir();
    if images.is_dir() {
        if let Some(p) = images.to_str() {
            if collocate_sys::mount::bind(p, p, true).is_ok() {
                let _ = collocate_sys::mount::mount(None, p, None, libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_REC, None);
            }
        }
    }
}

fn main() {
    let mut config_path = PathBuf::from("/etc/collocate/collocated.toml");
    let (mut check, mut reinit, mut adopt) = (false, false, false);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => config_path = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--check" => check = true,
            "--reinit-node" => reinit = true,
            "--adopt" => adopt = true,
            _ => usage(),
        }
    }
    let cfg = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("collocated: {e}");
            std::process::exit(2);
        }
    };
    if check {
        let report = collocate_sys::probe::probe();
        for c in &report.checks {
            println!("{:<20} {}  {}", c.name, if c.ok { "ok " } else { "FAIL" }, c.detail);
        }
        std::process::exit(i32::from(!report.all_ok()));
    }
    if reinit || adopt {
        let result = NodeStore::open(&cfg.state_dir, &cfg.run_dir).and_then(|s| {
            if reinit {
                s.reinit_node(&instance_id())
            } else {
                s.adopt_node(&instance_id())
            }
        });
        if let Err(e) = result {
            eprintln!("collocated: {e}");
            std::process::exit(1);
        }
    }
    prune_mounts(&cfg);
    let mut daemon = match Daemon::new(cfg) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("collocated: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = daemon.run() {
        eprintln!("collocated: {e}");
        std::process::exit(1);
    }
}
