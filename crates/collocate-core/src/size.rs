use crate::{Error, Result};

pub fn parse_size(input: &str) -> Result<u64> {
    let bad = || Error::InvalidSize(input.to_string());
    let s = input.trim();
    if s.is_empty() {
        return Err(bad());
    }
    let (digits, shift) = match s.chars().last().map(|c| c.to_ascii_lowercase()) {
        Some('k') => (&s[..s.len() - 1], 10),
        Some('m') => (&s[..s.len() - 1], 20),
        Some('g') => (&s[..s.len() - 1], 30),
        Some('t') => (&s[..s.len() - 1], 40),
        _ => (s, 0),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = digits.parse().map_err(|_| bad())?;
    n.checked_mul(1u64 << shift).ok_or_else(bad)
}

pub fn parse_duration_secs(input: &str) -> Result<u64> {
    let bad = || Error::Invalid(format!("invalid duration {input:?}"));
    let s = input.trim();
    let (digits, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        Some('d') => (&s[..s.len() - 1], 86400),
        _ => (s, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    digits.parse::<u64>().map_err(|_| bad())?.checked_mul(mult).ok_or_else(bad)
}
