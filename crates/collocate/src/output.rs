use clap::ValueEnum;
use collocate_core::{Error, Result};
use std::io::{IsTerminal, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Table,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum Verbosity {
    Quiet,
    Brief,
    Verbose,
    Debug,
    Trace,
}

pub fn resolve_verbosity(explicit: Option<Verbosity>, quiet: bool, verbose_count: u8) -> Verbosity {
    if let Some(v) = explicit {
        return v;
    }
    if quiet {
        return Verbosity::Quiet;
    }
    match verbose_count {
        0 => Verbosity::Brief,
        1 => Verbosity::Verbose,
        2 => Verbosity::Debug,
        _ => Verbosity::Trace,
    }
}

pub fn should_emit(configured: Verbosity, level: Verbosity) -> bool {
    level <= configured
}

pub fn narrate(configured: Verbosity, level: Verbosity, message: &str) {
    if should_emit(configured, level) {
        eprintln!("{message}");
    }
}

pub fn success_line(verb: &str, subject: &str) -> String {
    format!("{verb} {subject}.")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmPolicy {
    AutoYes,
    Ask,
    RefuseNonInteractive,
}

pub fn confirm_policy(assume_yes: bool, is_tty: bool) -> ConfirmPolicy {
    if assume_yes {
        ConfirmPolicy::AutoYes
    } else if is_tty {
        ConfirmPolicy::Ask
    } else {
        ConfirmPolicy::RefuseNonInteractive
    }
}

pub fn parse_answer(input: &str) -> Option<bool> {
    match input.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

pub fn confirm(prompt: &str, default: bool, assume_yes: bool) -> Result<bool> {
    match confirm_policy(assume_yes, std::io::stdin().is_terminal()) {
        ConfirmPolicy::AutoYes => Ok(true),
        ConfirmPolicy::RefuseNonInteractive => {
            Err(Error::Invalid(format!("{prompt} needs confirmation; pass --yes to run this non-interactively")))
        }
        ConfirmPolicy::Ask => {
            let hint = if default { "Y/n" } else { "y/N" };
            eprint!("{prompt} [{hint}] ");
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            Ok(parse_answer(&line).unwrap_or(default))
        }
    }
}

pub fn terminal_width(default: usize) -> usize {
    std::env::var("COLUMNS").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

pub fn is_interactive() -> bool {
    std::io::stdout().is_terminal()
}
