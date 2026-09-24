use collocate_core::layout::Layout;
use collocate_store::NodeStore;
use collocated::config::Config;
use collocated::daemon::{instance_id, Daemon};
use collocated::{setup, standby};
use std::path::PathBuf;

fn usage() -> ! {
    eprintln!("usage: collocated [--config PATH] [--check] [--reinit-node | --adopt] [--teardown]");
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

fn fail(code: i32, e: impl std::fmt::Display) -> ! {
    eprintln!("collocated: {e}");
    std::process::exit(code);
}

fn main() {
    let layout = Layout::detect();
    let mut config_path = layout.config.clone();
    let (mut check, mut reinit, mut adopt, mut teardown, mut worker, mut with_input) = (false, false, false, false, false, false);
    let mut worker_state: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => config_path = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--check" => check = true,
            "--reinit-node" => reinit = true,
            "--adopt" => adopt = true,
            "--teardown" => teardown = true,
            "--image-worker" => worker = true,
            "--with-input" => with_input = true,
            "--state-dir" => worker_state = Some(args.next().map(PathBuf::from).unwrap_or_else(|| usage())),
            _ => usage(),
        }
    }
    if worker {
        let state = worker_state.unwrap_or_else(|| layout.state_dir.clone());
        std::process::exit(collocated::images::worker_main(&state, with_input));
    }
    if check {
        let report = collocate_sys::probe::probe();
        for c in &report.checks {
            println!("{:<20} {}  {}", c.name, if c.ok { "ok " } else { "FAIL" }, c.detail);
        }
        std::process::exit(i32::from(!report.all_ok()));
    }
    let loaded = Config::load(&config_path).unwrap_or_else(|e| fail(2, e));
    if teardown {
        let cfg = loaded.unwrap_or_else(|| Config::with_layout(&layout));
        if let Err(e) = setup::teardown(&cfg) {
            fail(1, e);
        }
        std::process::exit(0);
    }
    let cfg = match loaded {
        Some(c) => c,
        None if reinit || adopt => fail(2, "collocate is not initialized"),
        None => match standby::serve(&layout, &config_path) {
            Ok(true) => setup::restart(),
            Ok(false) => std::process::exit(0),
            Err(e) => fail(1, e),
        },
    };
    if reinit || adopt {
        let result = NodeStore::open(&cfg.state_dir, &cfg.run_dir).and_then(|s| {
            if reinit {
                s.reinit_node(&instance_id())
            } else {
                s.adopt_node(&instance_id())
            }
        });
        if let Err(e) = result {
            fail(1, e);
        }
    }
    prune_mounts(&cfg);
    let mut daemon = Daemon::new(cfg).unwrap_or_else(|e| fail(1, e));
    daemon.set_config_path(config_path);
    daemon.set_image_worker(Some(PathBuf::from("/proc/self/exe")));
    if let Err(e) = daemon.run() {
        fail(1, e);
    }
    if let Some(plan) = daemon.take_reexec() {
        if let Some(bridge) = plan.old_bridge {
            setup::teardown_network(&bridge);
        }
        drop(daemon);
        setup::restart();
    }
}
