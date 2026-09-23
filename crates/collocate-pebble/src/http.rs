use crate::error::{Error, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

fn read_line(r: &mut impl BufRead) -> Result<String> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Err(Error::Protocol("connection closed mid-response".into()));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn read_chunked(r: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let size_line = read_line(r)?;
        let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| Error::Protocol(format!("bad chunk size {size_line:?}")))?;
        if size == 0 {
            while !read_line(r)?.is_empty() {}
            return Ok(body);
        }
        let start = body.len();
        body.resize(start + size, 0);
        r.read_exact(&mut body[start..])?;
        read_line(r)?;
    }
}

pub fn get(socket: &Path, path: &str, timeout: Duration) -> Result<Response> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut r = BufReader::new(stream);
    let status_line = read_line(&mut r)?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| Error::Protocol(format!("bad status line {status_line:?}")))?;
    let mut content_length = None;
    let mut chunked = false;
    let mut content_type = String::new();
    loop {
        let line = read_line(&mut r)?;
        if line.is_empty() {
            break;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => content_length = v.parse::<usize>().ok(),
            "transfer-encoding" => chunked = v.eq_ignore_ascii_case("chunked"),
            "content-type" => content_type = v.to_string(),
            _ => {}
        }
    }
    let body = if chunked {
        read_chunked(&mut r)?
    } else if let Some(n) = content_length {
        let mut b = vec![0; n];
        r.read_exact(&mut b)?;
        b
    } else {
        let mut b = Vec::new();
        r.read_to_end(&mut b)?;
        b
    };
    Ok(Response { status, content_type, body })
}
