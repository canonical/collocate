use collocate_compose::model::ComposeFile;
use collocate_compose::up::UpOptions;
use collocate_controller::controller::Controller;
use collocate_core::client::{Api, Client};
use collocate_core::request::{Request, Response};
use collocate_core::{Error, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn usage() -> ! {
    eprintln!("usage: collocate-controller [-f FILE] [--host SOCKET] [--state-dir DIR] [--interval SECONDS]");
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

fn run(path: PathBuf, host: PathBuf, state_dir: PathBuf, interval: Duration) -> Result<()> {
    let mut client = Client::connect(&host)?;
    let opts = UpOptions {
        subnet: subnet(&mut client)?,
        base_dir: path.parent().filter(|p| !p.as_os_str().is_empty()).map_or_else(|| PathBuf::from("."), Path::to_path_buf),
        state_dir,
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
                client = Client::connect(&host)?;
            }
            Err(Error::Eof) => {
                eprintln!("collocate-controller: daemon closed the connection, reconnecting");
                client = Client::connect(&host)?;
            }
            Err(e) => eprintln!("collocate-controller: {e}"),
        }
        std::thread::sleep(interval);
    }
}

fn main() {
    let mut file = PathBuf::from("collocate-compose.yaml");
    let mut host = PathBuf::from("/run/collocate/collocate.sock");
    let mut state_dir = PathBuf::from("/var/lib/collocate");
    let mut interval = 2u64;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-f" | "--file" => file = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--host" => host = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--state-dir" => state_dir = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--interval" => interval = args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    if let Err(e) = run(file, host, state_dir, Duration::from_secs(interval.max(1))) {
        eprintln!("collocate-controller: {e}");
        std::process::exit(e.exit_code());
    }
}
