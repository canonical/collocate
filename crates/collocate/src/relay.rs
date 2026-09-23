use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

fn main() {
    let path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("COLLOCATE_HOST").ok())
        .unwrap_or_else(|| "/run/collocate/collocate.sock".to_string());
    let sock = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("collocate-relay: {path}: {e}");
            std::process::exit(4);
        }
    };
    let mut upstream = match sock.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("collocate-relay: {e}");
            std::process::exit(1);
        }
    };
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        let mut stdin = std::io::stdin().lock();
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || upstream.write_all(&buf[..n]).is_err() {
                break;
            }
        }
        let _ = upstream.shutdown(std::net::Shutdown::Write);
    });
    let mut downstream = sock;
    let mut out = std::io::stdout().lock();
    let mut buf = [0u8; 65536];
    while let Ok(n) = downstream.read(&mut buf) {
        if n == 0 || out.write_all(&buf[..n]).is_err() || out.flush().is_err() {
            break;
        }
    }
}
