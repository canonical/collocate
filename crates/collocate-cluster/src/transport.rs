use crate::lxc::exec_args;
use collocate_core::client::{into_result, Api};
use collocate_core::request::{Request, Response};
use collocate_core::wire::{read_frame, write_frame};
use collocate_core::{Error, Result};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub struct LxcApi {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl LxcApi {
    pub fn spawn(program: &str, node: &str, relay: &[String]) -> Result<LxcApi> {
        LxcApi::spawn_args(program, &exec_args(node, relay))
    }

    pub fn spawn_args(program: &str, args: &[String]) -> Result<LxcApi> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| Error::Unreachable(format!("{program}: {e}")))?;
        let stdin = child.stdin.take().ok_or_else(|| Error::Internal("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| Error::Internal("no stdout".into()))?;
        Ok(LxcApi { child, stdin, stdout })
    }
}

impl Api for LxcApi {
    fn call(&mut self, req: Request) -> Result<Response> {
        write_frame(&mut self.stdin, &req).map_err(|e| Error::Unreachable(e.to_string()))?;
        let resp: Response = read_frame(&mut self.stdout).map_err(|e| match e {
            Error::Eof => Error::Unreachable("node closed the connection".into()),
            other => other,
        })?;
        into_result(resp)
    }
}

impl Drop for LxcApi {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
