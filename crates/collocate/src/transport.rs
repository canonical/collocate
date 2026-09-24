use crate::cli::Cli;
use collocate_core::client::{Api, Client};
use collocate_core::request::{Request, Response};
use collocate_core::{Error, Result};
use collocate_remote::client::{Endpoint, HttpsApi};
use collocate_remote::config::{client_identity, config_dir, Remote, RemoteConfig, LOCAL};
use std::cell::RefCell;

thread_local! {
    static REMOTE: RefCell<Option<(String, HttpsApi)>> = const { RefCell::new(None) };
}

pub fn socket_hint(err: Error) -> Error {
    match err {
        Error::Unreachable(m) if m.contains("Permission denied") => Error::Unreachable(format!(
            "{m}; add yourself to the collocate group with 'sudo usermod -aG collocate $USER' and log in again, or use sudo"
        )),
        other => other,
    }
}

pub fn selected_remote(cli: &Cli) -> Result<Option<(String, Remote)>> {
    if cli.remote.as_deref() == Some(LOCAL) {
        return Ok(None);
    }
    let cfg = match RemoteConfig::load(&config_dir()) {
        Ok(c) => c,
        Err(Error::Io(_)) if cli.remote.is_none() => return Ok(None),
        Err(e) => return Err(e),
    };
    let name = cli.remote.clone().unwrap_or_else(|| cfg.default_remote().to_string());
    if name == LOCAL {
        return Ok(None);
    }
    Ok(Some((name.clone(), cfg.get(&name)?.clone())))
}

fn with_remote<T>(name: &str, remote: &Remote, f: impl FnOnce(&mut HttpsApi) -> Result<T>) -> Result<T> {
    REMOTE.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().map(|(n, _)| n.as_str()) != Some(name) {
            let identity = client_identity(&config_dir())?;
            let endpoint = Endpoint { addresses: remote.addresses.clone(), fingerprint: remote.fingerprint.clone() };
            *slot = Some((name.to_string(), HttpsApi::new(endpoint, identity)));
        }
        let (_, api) = slot.as_mut().expect("remote client");
        f(api).map_err(|e| match e {
            Error::Unreachable(m) => Error::Unreachable(format!("remote {name}: {m}")),
            other => other,
        })
    })
}

pub fn with_remote_api<T>(cli: &Cli, f: impl FnOnce(&mut HttpsApi) -> Result<T>) -> Result<Option<T>> {
    match selected_remote(cli)? {
        Some((name, remote)) => with_remote(&name, &remote, f).map(Some),
        None => Ok(None),
    }
}

struct RemoteApi {
    name: String,
    remote: Remote,
}

impl Api for RemoteApi {
    fn call(&mut self, req: Request) -> Result<Response> {
        with_remote(&self.name, &self.remote, |api| api.call(req))
    }
}

pub fn local(cli: &Cli) -> Result<Client<std::os::unix::net::UnixStream>> {
    Client::connect(&cli.host).map_err(socket_hint)
}

pub fn api(cli: &Cli) -> Result<Box<dyn Api>> {
    match selected_remote(cli)? {
        Some((name, remote)) => Ok(Box::new(RemoteApi { name, remote })),
        None => Ok(Box::new(local(cli)?)),
    }
}

pub fn call(cli: &Cli, req: Request) -> Result<Response> {
    api(cli)?.call(req)
}

pub fn is_remote(cli: &Cli) -> Result<bool> {
    Ok(selected_remote(cli)?.is_some())
}

pub enum ArchiveReader {
    Stdin(std::io::Stdin),
    File(std::fs::File, u64),
}

impl ArchiveReader {
    pub fn open(path: &std::path::Path) -> Result<ArchiveReader> {
        if path == std::path::Path::new("-") {
            return Ok(ArchiveReader::Stdin(std::io::stdin()));
        }
        let f = std::fs::File::open(path).map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))?;
        let len = f.metadata()?.len();
        Ok(ArchiveReader::File(f, len))
    }

    pub fn length(&self) -> Option<u64> {
        match self {
            ArchiveReader::Stdin(_) => None,
            ArchiveReader::File(_, n) => Some(*n),
        }
    }
}

impl std::io::Read for ArchiveReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            ArchiveReader::Stdin(s) => s.read(buf),
            ArchiveReader::File(f, _) => f.read(buf),
        }
    }
}

impl std::os::fd::AsRawFd for ArchiveReader {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        match self {
            ArchiveReader::Stdin(s) => s.as_raw_fd(),
            ArchiveReader::File(f, _) => f.as_raw_fd(),
        }
    }
}
