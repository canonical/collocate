use crate::{Error, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::io::{Read, Write};

pub const MAX_FRAME: usize = 1 << 20;

pub fn write_frame<W: Write, T: Serialize>(w: &mut W, value: &T) -> Result<()> {
    let payload = serde_json::to_vec(value)?;
    if payload.len() > MAX_FRAME {
        return Err(Error::FrameTooLarge(payload.len()));
    }
    w.write_all(&(payload.len() as u32).to_be_bytes())?;
    w.write_all(&payload)?;
    w.flush()?;
    Ok(())
}

pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> Result<T> {
    let mut header = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        let n = r.read(&mut header[got..])?;
        if n == 0 {
            return if got == 0 { Err(Error::Eof) } else { Err(Error::Io(std::io::ErrorKind::UnexpectedEof.into())) };
        }
        got += n;
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME {
        return Err(Error::FrameTooLarge(len));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(serde_json::from_slice(&payload)?)
}
