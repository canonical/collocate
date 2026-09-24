use crate::request::{Request, Response};
use crate::wire::{read_frame, write_frame};
use crate::{Error, Result};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

pub fn into_result(resp: Response) -> Result<Response> {
    match resp {
        Response::Error { code, message } => Err(match code {
            3 => Error::NotFound(message),
            5 => Error::Conflict(message),
            6 => Error::Timeout(message),
            4 => Error::Unreachable(message),
            2 => Error::Invalid(message),
            7 => Error::NotInitialized(message),
            8 => Error::Denied(message),
            9 => Error::Forbidden(message),
            _ => Error::Internal(message),
        }),
        other => Ok(other),
    }
}

pub trait Api {
    fn call(&mut self, req: Request) -> Result<Response>;
}

impl<S: Read + Write> Api for Client<S> {
    fn call(&mut self, req: Request) -> Result<Response> {
        Client::call(self, &req)
    }
}

pub struct Client<S: Read + Write> {
    stream: S,
}

impl<S: Read + Write> Client<S> {
    pub fn new(stream: S) -> Self {
        Client { stream }
    }

    pub fn call(&mut self, req: &Request) -> Result<Response> {
        write_frame(&mut self.stream, req)?;
        into_result(read_frame(&mut self.stream)?)
    }

    pub fn read_response(&mut self) -> Result<Response> {
        into_result(read_frame(&mut self.stream)?)
    }

    pub fn stream(&self) -> &S {
        &self.stream
    }
}

impl Client<UnixStream> {
    pub fn connect(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        UnixStream::connect(path).map(Client::new).map_err(|e| Error::Unreachable(format!("{}: {e}", path.display())))
    }
}
