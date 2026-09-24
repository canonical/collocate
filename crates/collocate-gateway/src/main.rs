use collocate_core::client::Client;
use collocate_core::layout::Layout;
use collocate_core::request::{Request, Response};
use collocate_core::settings::bind_address;
use collocate_gateway::Gateway;
use collocate_remote::tls::server_config;
use collocate_trust::TrustStore;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CHECK: Duration = Duration::from_secs(5);

fn usage() -> ! {
    eprintln!("usage: collocate-gateway [--host SOCKET]");
    std::process::exit(2);
}

#[derive(Clone, PartialEq, Eq)]
struct Target {
    address: String,
    state_dir: PathBuf,
}

enum Probe {
    Unreachable,
    Disabled,
    Enabled(Target),
}

fn probe(socket: &Path) -> Probe {
    let Ok(mut c) = Client::connect(socket) else { return Probe::Unreachable };
    let Ok(Response::Text { text }) = c.call(&Request::Info) else { return Probe::Unreachable };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    match (v["initialized"].as_bool(), v["https_address"].as_str(), v["state_dir"].as_str()) {
        (Some(true), Some(a), Some(s)) => Probe::Enabled(Target { address: a.to_string(), state_dir: PathBuf::from(s) }),
        _ => Probe::Disabled,
    }
}

fn server_identity(state_dir: &Path) -> Option<collocate_trust::Identity> {
    TrustStore::open(state_dir.join("trust")).ok()?.server_identity().ok()?
}

fn serve(socket: &Path, target: &Target) {
    let Some(identity) = server_identity(&target.state_dir) else {
        std::thread::sleep(CHECK);
        return;
    };
    let config = match server_config(&identity) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("collocate-gateway: {}", e.payload());
            std::thread::sleep(CHECK);
            return;
        }
    };
    let listener = match bind_address(&target.address).and_then(|a| TcpListener::bind(a).map_err(Into::into)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("collocate-gateway: cannot listen on {}: {}", target.address, e.payload());
            std::thread::sleep(CHECK * 2);
            return;
        }
    };
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    eprintln!("collocate-gateway: listening on {} with certificate {}", target.address, identity.fingerprint);
    let gateway = Gateway::new(socket.to_path_buf(), identity.fingerprint.clone());
    let mut last = Instant::now();
    loop {
        match listener.accept() {
            Ok((sock, peer)) => {
                if sock.set_nonblocking(false).is_err() || !gateway.try_serve(sock, config.clone()) {
                    eprintln!("collocate-gateway: refusing {peer}: too many connections");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => eprintln!("collocate-gateway: accept: {e}"),
        }
        if last.elapsed() >= CHECK {
            last = Instant::now();
            let changed = match probe(socket) {
                Probe::Unreachable => false,
                Probe::Disabled => true,
                Probe::Enabled(t) => t != *target,
            };
            let rotated = server_identity(&target.state_dir).map(|i| i.fingerprint) != Some(identity.fingerprint.clone());
            if changed || rotated {
                eprintln!("collocate-gateway: configuration changed, restarting the listener");
                return;
            }
        }
    }
}

fn main() {
    let mut socket = Layout::detect().socket();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--host" => socket = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    let mut announced = false;
    loop {
        match probe(&socket) {
            Probe::Enabled(target) => {
                announced = false;
                serve(&socket, &target);
            }
            _ => {
                if !announced {
                    eprintln!("collocate-gateway: remote access is disabled; waiting for an https address on {}", socket.display());
                    announced = true;
                }
                std::thread::sleep(CHECK);
            }
        }
    }
}
