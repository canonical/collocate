use collocate_core::request::{RegistryCredential, Request, Response};
use collocate_core::wire::{read_frame, write_frame};
use collocate_core::{Error, Result};
use collocate_image::config::ImageStore;
use collocate_image::pull::{ensure_pulled, PullPolicy};
use collocate_registry::auth::Credentials;
use collocate_registry::reference::canonical_registry;
use collocate_registry::Reference;
use std::fs::File;
use std::os::fd::FromRawFd;
use std::path::Path;

pub const WORKER_SOCKET_FD: i32 = 3;
pub const WORKER_INPUT_FD: i32 = 4;

pub struct RequestCredentials(pub Vec<RegistryCredential>);

impl Credentials for RequestCredentials {
    fn for_registry(&self, registry: &str) -> Option<(String, String)> {
        self.0.iter().find(|c| canonical_registry(&c.registry) == registry).map(|c| (c.username.clone(), c.password.clone()))
    }
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<Response> {
    Ok(Response::Json { value: serde_json::to_value(v)? })
}

pub fn handle_inline(state_dir: &Path, req: Request) -> Result<Response> {
    let store = ImageStore::new(state_dir);
    match req {
        Request::ImageList => to_json(&store.list()?),
        Request::ImageShow { name } => to_json(&store.get(&name)?),
        Request::ImageDelete { name } => {
            store.remove(&name)?;
            Ok(Response::Ok)
        }
        Request::ImagePrune => to_json(&store.gc()?),
        other => Err(Error::Internal(format!("not an inline image request: {other:?}"))),
    }
}

pub fn handle_worker(state_dir: &Path, req: Request, input: Option<File>) -> Result<Response> {
    match req {
        Request::ImagePull { reference, policy, credentials } => {
            let parsed = Reference::parse(&reference).map_err(|e| Error::Invalid(format!("image {reference}: {e}")))?;
            let policy = PullPolicy::parse(&policy)?;
            let meta = ensure_pulled(&parsed, state_dir, &RequestCredentials(credentials), policy)?;
            to_json(&meta)
        }
        Request::ImageImport => {
            let file = input.ok_or_else(|| Error::Invalid("image import needs an archive file descriptor".into()))?;
            to_json(&collocate_image::import::import_archive(file, state_dir)?)
        }
        other => Err(Error::Internal(format!("not a worker image request: {other:?}"))),
    }
}

fn write_all_nonblocking(fd: i32, mut data: &[u8]) -> std::io::Result<()> {
    while !data.is_empty() {
        let n = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if n >= 0 {
            data = &data[n as usize..];
            continue;
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EAGAIN) => {
                let mut pfd = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
                if unsafe { libc::poll(&mut pfd, 1, 30_000) } <= 0 {
                    return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "client stopped reading"));
                }
            }
            Some(libc::EINTR) => {}
            _ => return Err(err),
        }
    }
    Ok(())
}

pub fn worker_main(state_dir: &Path, with_input: bool) -> i32 {
    let input = with_input.then(|| unsafe { File::from_raw_fd(WORKER_INPUT_FD) });
    let resp = match read_frame::<_, Request>(&mut std::io::stdin().lock()) {
        Ok(req) => handle_worker(state_dir, req, input).unwrap_or_else(|e| Response::error(&e)),
        Err(e) => Response::error(&e),
    };
    let mut frame = Vec::new();
    if write_frame(&mut frame, &resp).is_err() {
        return 1;
    }
    match write_all_nonblocking(WORKER_SOCKET_FD, &frame) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
