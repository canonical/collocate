use collocate_compose::model::NodeDef;
use collocate_core::{Error, Result};

fn memory_arg(input: &str) -> String {
    let s = input.trim();
    match s.chars().last().map(|c| c.to_ascii_lowercase()) {
        Some('k') => format!("{}KiB", &s[..s.len() - 1]),
        Some('m') => format!("{}MiB", &s[..s.len() - 1]),
        Some('g') => format!("{}GiB", &s[..s.len() - 1]),
        Some('t') => format!("{}TiB", &s[..s.len() - 1]),
        _ => format!("{s}B"),
    }
}

pub fn launch_args(node: &str, def: &NodeDef) -> Vec<String> {
    let mut a = vec!["launch".to_string(), def.image.clone().unwrap_or_else(|| "ubuntu:24.04".to_string()), node.to_string()];
    if let Some(t) = &def.target {
        a.push("--target".into());
        a.push(t.clone());
    }
    if let Some(c) = def.cpus {
        a.push("-c".into());
        a.push(format!("limits.cpu={}", c.ceil() as u64));
    }
    if let Some(m) = &def.memory {
        a.push("-c".into());
        a.push(format!("limits.memory={}", memory_arg(m)));
    }
    a.push("-c".into());
    a.push("security.nesting=true".into());
    a
}

pub fn exec_args(node: &str, argv: &[String]) -> Vec<String> {
    let mut a = vec!["exec".to_string(), node.to_string(), "--".to_string()];
    a.extend(argv.iter().cloned());
    a
}

pub fn push_args(src: &str, node: &str, dest: &str) -> Vec<String> {
    vec!["file".into(), "push".into(), src.into(), format!("{node}{dest}")]
}

pub fn list_args() -> Vec<String> {
    vec!["list".into(), "--format".into(), "json".into()]
}

pub fn running_nodes(json: &str) -> Result<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let arr = v.as_array().ok_or_else(|| Error::Parse("lxc list did not return an array".into()))?;
    Ok(arr.iter().filter(|i| i["status"].as_str() == Some("Running")).filter_map(|i| i["name"].as_str().map(String::from)).collect())
}

pub struct Lxc {
    pub program: String,
}

impl Lxc {
    pub fn new(program: impl Into<String>) -> Lxc {
        Lxc { program: program.into() }
    }

    pub fn run(&self, args: &[String], stdin: Option<&str>) -> Result<String> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new(&self.program)
            .args(args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Unreachable(format!("{}: {e}", self.program)))?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(text.as_bytes())?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let detail = if stderr.trim().is_empty() { stdout.trim().to_string() } else { stderr.trim().to_string() };
            return Err(Error::Internal(format!("{} {} failed: {detail}", self.program, args.join(" "))));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn ok(&self, args: &[String]) -> bool {
        self.run(args, None).is_ok()
    }
}
