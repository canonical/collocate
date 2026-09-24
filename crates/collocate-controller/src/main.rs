use collocate_compose::model::ComposeFile;
use collocate_compose::up::UpOptions;
use collocate_controller::controller::Controller;
use collocate_core::client::{Api, Client};
use collocate_core::layout::Layout;
use collocate_core::request::{Request, Response};
use collocate_core::{Error, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn usage() -> ! {
    eprintln!("usage: collocate-controller [-f FILE] [--host SOCKET] [--interval SECONDS]");
    std::process::exit(2);
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn load(path: &Path) -> Result<ComposeFile> {
    ComposeFile::load(&std::fs::read_to_string(path)?)
}

fn subnet(api: &mut dyn Api) -> Result<String> {
    match api.call(Request::Info)? {
        Response::Text { text } => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["subnet"].as_str().map(String::from))
            .ok_or_else(|| Error::Internal("daemon did not report its subnet".into())),
        other => Err(Error::Internal(format!("unexpected response {other:?}"))),
    }
}

fn initialized(api: &mut dyn Api) -> bool {
    match api.call(Request::Info) {
        Ok(Response::Text { text }) => {
            serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v["initialized"].as_bool()).unwrap_or(true)
        }
        _ => false,
    }
}

fn wait_ready(path: &Path, host: &Path, interval: Duration) -> Client<std::os::unix::net::UnixStream> {
    let mut announced = false;
    loop {
        if path.is_file() {
            if let Ok(mut c) = Client::connect(host) {
                if initialized(&mut c) {
                    return c;
                }
            }
        }
        if !announced {
            eprintln!("collocate-controller: waiting for {} and an initialized daemon at {}", path.display(), host.display());
            announced = true;
        }
        std::thread::sleep(interval.max(Duration::from_secs(5)));
    }
}

fn run(path: PathBuf, host: PathBuf, interval: Duration) -> Result<()> {
    let mut client = wait_ready(&path, &host, interval);
    let opts = UpOptions {
        subnet: subnet(&mut client)?,
        base_dir: path.parent().filter(|p| !p.as_os_str().is_empty()).map_or_else(|| PathBuf::from("."), Path::to_path_buf),
        regenerate_secrets: None,
        dry_run: false,
        ready_timeout: Duration::from_secs(30),
        poll_interval: Duration::from_millis(250),
    };
    let mut controller = Controller::new(load(&path)?, opts)?;
    let mut seen = modified(&path);
    loop {
        if modified(&path) != seen {
            seen = modified(&path);
            match load(&path) {
                Ok(f) => {
                    controller.set_file(f);
                    eprintln!("collocate-controller: reloaded {}", path.display());
                }
                Err(e) => eprintln!("collocate-controller: keeping previous configuration: {e}"),
            }
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        match controller.tick(&mut client, now) {
            Ok(events) => {
                for e in events {
                    println!("{e}");
                }
            }
            Err(Error::Unreachable(m)) => {
                eprintln!("collocate-controller: daemon connection lost: {m}");
                client = wait_ready(&path, &host, interval);
            }
            Err(Error::Eof) => {
                eprintln!("collocate-controller: daemon closed the connection, reconnecting");
                client = wait_ready(&path, &host, interval);
            }
            Err(e) => eprintln!("collocate-controller: {e}"),
        }
        std::thread::sleep(interval);
    }
}

fn main() {
    let layout = Layout::detect();
    let mut file = layout.controller_file.clone();
    let mut host = layout.socket();
    let mut interval = 2u64;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-f" | "--file" => file = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--host" => host = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--interval" => interval = args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    if let Err(e) = run(file, host, Duration::from_secs(interval.max(1))) {
        eprintln!("collocate-controller: {e}");
        std::process::exit(e.exit_code());
    }
}
